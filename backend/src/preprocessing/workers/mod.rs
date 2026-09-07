//! Concrete preprocessor implementations.
//!
//! Each module wraps an existing python-call worker (`beat_worker`,
//! `stem_worker`, `root_worker`) in a [`Preprocessor`] impl. The python-call
//! layer is unchanged; this module owns DB persistence + scheduler wiring.
//!
//! Bar-indexed preprocessors (`classifier`, `genre`) additionally share the
//! two helpers below, so they agree on what "bar N" means and self-heal the
//! same way when the beat grid moves underneath them.

pub mod beat_grid;
pub mod classifier;
pub mod genre;
pub mod mert;
pub mod n2n;
pub mod roots;
pub mod stems;

use sqlx::SqlitePool;

/// Tolerance (seconds) for "first-bar duration matches current beat grid"
/// staleness detection in [`list_pending_bar_aligned`]. A real re-detection
/// that flips BPM (e.g. 120 → 70) shifts the bar duration by >1s, well above
/// this floor; floating-point round-trips through `serde_json` are well below
/// it.
const ALIGNED_BAR_TOLERANCE_SECS: f64 = 0.1;

/// Build `[(start, end), ...]` bar boundaries from downbeat times, falling
/// back to a synthetic final bar of `60/bpm * beats_per_bar` seconds.
///
/// This is *the* definition of Luma's bar axis: every bar-indexed artifact
/// derives its `bar_idx` from this list, which is what lets the agent index
/// `features.bars` and `features.genres` with the same integer.
///
/// Returns an empty Vec when the grid has fewer than two downbeats.
pub fn build_bar_boundaries(
    downbeats: &[f64],
    bpm: Option<f64>,
    beats_per_bar: Option<i64>,
) -> Vec<(f64, f64)> {
    if downbeats.len() < 2 {
        return Vec::new();
    }
    let mut out: Vec<(f64, f64)> = downbeats.windows(2).map(|w| (w[0], w[1])).collect();
    let bpm = bpm.unwrap_or(0.0);
    let bpb = beats_per_bar.unwrap_or(4) as f64;
    if bpm > 0.0 && bpb > 0.0 {
        let bar_secs = (60.0 / bpm) * bpb;
        let last = *downbeats.last().unwrap();
        out.push((last, last + bar_secs));
    }
    out
}

/// Self-correcting `list_pending` for artifacts whose rows are indexed by bar.
///
/// The default trait impl only asks "does a row exist at the right
/// `processor_version`?", but a bar-indexed artifact also encodes the beat grid
/// that was current when it ran (`bar_idx` → `(downbeats[i], downbeats[i+1])`).
/// When that grid is later overwritten — re-detection, sync pull from another
/// device — the indices no longer line up with the audio and the drift
/// compounds bar by bar.
///
/// Detection is cheap because each worker persists its first bar's
/// `start`/`end`: compare that span against `60/bpm * beats_per_bar` of the
/// *current* `track_beats` row and re-queue on disagreement. The worker's
/// normal run-and-upsert path then overwrites the stale row.
///
/// `first_bar_start` / `first_bar_end` are SQLite JSON paths into `json_column`
/// (e.g. `$[0].start`, `$.bars[0].start`), which keeps the parse out of Rust so
/// the bulk reconcile query stays a single round-trip.
pub async fn list_pending_bar_aligned(
    pool: &SqlitePool,
    preprocessor: &str,
    version: u32,
    table: &str,
    json_column: &str,
    first_bar_start: &str,
    first_bar_end: &str,
) -> Result<Vec<String>, String> {
    // Every interpolated fragment is a `&'static str` from the calling
    // preprocessor — never user input.
    let sql = format!(
        "SELECT t.id FROM tracks t
          WHERE t.file_path IS NOT NULL
            AND t.file_path != ''
            AND t.file_path NOT LIKE '%.stub'
            AND NOT EXISTS (
                SELECT 1 FROM preprocessing_failures f
                 WHERE f.track_id = t.id AND f.preprocessor = ?1
                   AND f.next_retry_at > strftime('%Y-%m-%dT%H:%M:%SZ','now')
            )
            AND (
                -- Missing or older-version row: the default condition.
                (SELECT COUNT(*) FROM {table} a
                  WHERE a.track_id = t.id AND a.processor_version >= ?2) < 1
                OR
                -- Stale row: bar boundaries no longer match the current grid.
                EXISTS (
                    SELECT 1
                      FROM {table} a
                      JOIN track_beats b ON b.track_id = a.track_id
                     WHERE a.track_id = t.id
                       AND b.bpm IS NOT NULL AND b.bpm > 0
                       AND b.beats_per_bar IS NOT NULL
                       AND ABS(
                             (CAST(json_extract(a.{json_column}, '{first_bar_end}')   AS REAL)
                            - CAST(json_extract(a.{json_column}, '{first_bar_start}') AS REAL))
                           - (60.0 / b.bpm * b.beats_per_bar)
                           ) > ?3
                )
            )"
    );
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(preprocessor)
        .bind(version as i64)
        .bind(ALIGNED_BAR_TOLERANCE_SECS)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("{preprocessor} list_pending: {e}"))
}

#[cfg(test)]
mod tests {
    use super::build_bar_boundaries;

    #[test]
    fn build_bar_boundaries_pairs_consecutive_downbeats_plus_synth_tail() {
        let db = vec![0.0, 1.0, 2.0, 3.0];
        let out = build_bar_boundaries(&db, Some(120.0), Some(4));
        // 3 real bars (0-1, 1-2, 2-3) + 1 synthetic tail (3, 5).
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], (0.0, 1.0));
        assert_eq!(out[2], (2.0, 3.0));
        assert!((out[3].0 - 3.0).abs() < 1e-9);
        assert!((out[3].1 - 5.0).abs() < 1e-9); // 3 + (60/120)*4 = 3 + 2 = 5
    }

    #[test]
    fn build_bar_boundaries_returns_empty_for_too_few_downbeats() {
        assert!(build_bar_boundaries(&[], Some(120.0), Some(4)).is_empty());
        assert!(build_bar_boundaries(&[1.0], Some(120.0), Some(4)).is_empty());
    }
}
