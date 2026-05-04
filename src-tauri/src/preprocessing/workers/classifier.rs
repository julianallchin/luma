//! Joint bar classifier preprocessor.
//!
//! Loads MERT-v1-95M (~400 MB on first launch from HuggingFace cache),
//! extracts per-bar frame embeddings, then runs the bundled
//! `BarWindowClassifier` head against them. **MERT embeddings are
//! intentionally discarded after prediction** — only the per-bar intensity
//! + tag probabilities reach disk. This is a deliberate tradeoff:
//! classification is fast, the 768-dim frame embeddings are not useful
//! downstream of the classifier in Luma's lighting flow, and persisting
//! them would dominate disk usage (~6 MB / track). When MERT is needed
//! elsewhere, recompute it.
//!
//! The inference logic lives in `python/classifier_worker.py`; the head
//! weights ship inline via `include_bytes!` in `crate::classifier_worker`
//! and bumping `version` re-runs across every track on next launch (the
//! standard preprocessing-DAG backfill pattern).
//!
//! ## Schema versioning
//!
//! `version = 2` ships the **22-tag windowed schema** baked into TANGO's
//! `bar_window_classifier.pt` (continuous `intensity` + 22 multi-label
//! tags across 6 heads):
//!
//! - drums: hats, kick, snare, perc, fill, impact
//! - rhythm: four_four, halftime, breakbeat, build
//! - bass: pluck, sustain
//! - synths: arp, pad, lead, riser
//! - acoustic: piano, acoustic_guitar, electric_guitar, other
//! - vocals: vocal_lead, vocal_chop
//!
//! When swapping weights again, replace the bundled .pt and bump
//! `version`; the reconcile-on-startup loop will re-classify every
//! track automatically.

use std::path::Path;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::classifier_worker;
use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};

/// Tolerance (seconds) for "first-bar duration matches current beat grid"
/// staleness detection in [`ClassifierPreprocessor::list_pending`]. A real
/// re-detection that flips BPM (e.g. 120 → 70) shifts the bar duration by
/// >1s, well above this floor; floating-point round-trips through
/// `serde_json` are well below it.
const ALIGNED_BAR_TOLERANCE_SECS: f64 = 0.1;

pub struct ClassifierPreprocessor;

#[async_trait]
impl Preprocessor for ClassifierPreprocessor {
    fn name(&self) -> &'static str {
        "classifier"
    }
    fn version(&self) -> u32 {
        2
    }
    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::Audio, Artifact::BeatGrid]
    }
    fn output(&self) -> Artifact {
        Artifact::BarClassifications
    }
    fn status_label(&self) -> &'static str {
        "Classifying bars…"
    }
    fn artifact_table(&self) -> &'static str {
        "track_bar_classifications"
    }

    /// Self-correcting completeness check. The default trait impl only
    /// asks "does an artifact row exist at the right `processor_version`?",
    /// but this preprocessor's output indexes into the beat grid that was
    /// current at run time (`bar_idx` → `(downbeats[i], downbeats[i+1])`).
    /// When the grid is later overwritten — re-detection, sync pull from
    /// another device — those indices no longer line up with the audio,
    /// and the drift compounds bar by bar.
    ///
    /// Detection is cheap because the classifier persists each bar's
    /// `start`/`end` alongside its `bar_idx`: the consumed grid's bar
    /// duration is right there, and we just compare it to
    /// `60/bpm * beats_per_bar` of the *current* `track_beats` row. Any
    /// significant deviation means the row was generated against a stale
    /// grid and reconcile-on-startup needs to re-queue the track. The
    /// existing `run()` path then upserts a fresh row over the stale one.
    ///
    /// SQLite's `json_extract` keeps the parse out of Rust so the bulk
    /// reconcile query stays a single round-trip.
    async fn list_pending(&self, pool: &SqlitePool) -> Result<Vec<String>, String> {
        let sql = "
            SELECT t.id FROM tracks t
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
                   (SELECT COUNT(*) FROM track_bar_classifications c
                     WHERE c.track_id = t.id AND c.processor_version >= ?2) < 1
                   OR
                   -- Stale row: bar boundaries no longer match the current
                   -- beat grid.
                   EXISTS (
                       SELECT 1
                         FROM track_bar_classifications c
                         JOIN track_beats b ON b.track_id = c.track_id
                        WHERE c.track_id = t.id
                          AND b.bpm IS NOT NULL AND b.bpm > 0
                          AND b.beats_per_bar IS NOT NULL
                          AND ABS(
                                (CAST(json_extract(c.classifications_json, '$[0].end')   AS REAL)
                               - CAST(json_extract(c.classifications_json, '$[0].start') AS REAL))
                              - (60.0 / b.bpm * b.beats_per_bar)
                              ) > ?3
                   )
               )";
        sqlx::query_scalar(sql)
            .bind(self.name())
            .bind(self.version() as i64)
            .bind(ALIGNED_BAR_TOLERANCE_SECS)
            .fetch_all(pool)
            .await
            .map_err(|e| format!("{} list_pending: {e}", self.name()))
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let audio_path = std::path::PathBuf::from(&track.file_path);

        // Bar boundaries derive from the beat grid: consecutive downbeat
        // pairs plus a synthetic final bar of length (60/bpm * beats_per_bar).
        // Mirrors TANGO's `_bar_boundaries_from_grid` so MERT segments here
        // match what the classifier was trained against.
        let beats = tracks_db::get_track_beats_raw(ctx.pool(), track_id)
            .await?
            .ok_or_else(|| format!("Missing beat grid for track {track_id}"))?;
        let downbeats: Vec<f64> = serde_json::from_str(&beats.downbeats_json)
            .map_err(|e| format!("Failed to parse downbeats_json: {e}"))?;
        let bar_boundaries = build_bar_boundaries(&downbeats, beats.bpm, beats.beats_per_bar);
        if bar_boundaries.is_empty() {
            return Err(format!(
                "No bar boundaries derivable from track {track_id} (need ≥ 2 downbeats)"
            ));
        }

        let handle = ctx.app_handle().clone();
        let analysis = tauri::async_runtime::spawn_blocking(move || {
            classifier_worker::classify_bars(&handle, Path::new(&audio_path), &bar_boundaries)
        })
        .await
        .map_err(|e| format!("Classifier worker task failed: {e}"))??;

        let classifications_json = serde_json::to_string(&analysis.bars)
            .map_err(|e| format!("Failed to serialize bar classifications: {e}"))?;
        let tag_order_json = serde_json::to_string(&analysis.tag_order)
            .map_err(|e| format!("Failed to serialize tag order: {e}"))?;

        tracks_db::upsert_track_bar_classifications(
            ctx.pool(),
            track_id,
            &classifications_json,
            &tag_order_json,
            self.version(),
        )
        .await
    }
}

/// Build `[(start, end), ...]` bar boundaries from downbeat times, falling
/// back to a synthetic final bar of `60/bpm * beats_per_bar` seconds.
///
/// Returns an empty Vec when the grid has fewer than two downbeats.
fn build_bar_boundaries(
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

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use super::{build_bar_boundaries, ClassifierPreprocessor};
    use crate::classifier_worker;
    use crate::preprocessing::preprocessor::Preprocessor;
    use crate::preprocessing::registry;
    use crate::preprocessing::scheduler::topo_layers;

    async fn test_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true)
            .foreign_keys(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("in-memory db");

        sqlx::query(
            "CREATE TABLE tracks (
                id TEXT PRIMARY KEY,
                file_path TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE track_beats (
                track_id TEXT PRIMARY KEY,
                beats_json TEXT NOT NULL,
                downbeats_json TEXT NOT NULL,
                bpm REAL,
                downbeat_offset REAL,
                beats_per_bar INTEGER,
                processor_version INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE track_bar_classifications (
                track_id TEXT PRIMARY KEY,
                classifications_json TEXT NOT NULL,
                tag_order_json TEXT NOT NULL,
                processor_version INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE preprocessing_failures (
                track_id TEXT NOT NULL,
                preprocessor TEXT NOT NULL,
                version INTEGER NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 1,
                last_error TEXT NOT NULL,
                last_attempt TEXT NOT NULL,
                next_retry_at TEXT NOT NULL,
                PRIMARY KEY (track_id, preprocessor)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// Insert a beat-grid row used by the staleness branch of `list_pending`.
    /// `bar_secs` is round-tripped through the `bpm` column (one bar at
    /// 4/4 = `60/bpm * 4`), so callers think in human-readable bar widths.
    async fn insert_beats(pool: &SqlitePool, track_id: &str, bar_secs: f64) {
        let bpm = 60.0 * 4.0 / bar_secs;
        sqlx::query(
            "INSERT INTO track_beats
                (track_id, beats_json, downbeats_json, bpm, beats_per_bar)
             VALUES (?, '[]', '[]', ?, 4)",
        )
        .bind(track_id)
        .bind(bpm)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Classifier output for a synthetic two-bar stretch starting at 0.
    /// `bar_secs` controls the bar duration encoded in `start`/`end`.
    async fn insert_classifications(
        pool: &SqlitePool,
        track_id: &str,
        bar_secs: f64,
        version: u32,
    ) {
        let json = format!(
            r#"[{{"bar_idx":0,"start":0.0,"end":{0},"predictions":{{}}}},
                {{"bar_idx":1,"start":{0},"end":{1},"predictions":{{}}}}]"#,
            bar_secs,
            bar_secs * 2.0,
        );
        sqlx::query(
            "INSERT INTO track_bar_classifications
                (track_id, classifications_json, tag_order_json, processor_version)
             VALUES (?, ?, '[]', ?)",
        )
        .bind(track_id)
        .bind(json)
        .bind(version as i64)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_pending_returns_tracks_without_classifications() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();

        let p = ClassifierPreprocessor;
        let pending = p.list_pending(&pool).await.unwrap();
        assert_eq!(pending, vec!["t1".to_string()]);

        sqlx::query(
            "INSERT INTO track_bar_classifications
                (track_id, classifications_json, tag_order_json, processor_version)
             VALUES ('t1', '[]', '[]', ?)",
        )
        .bind(p.version() as i64)
        .execute(&pool)
        .await
        .unwrap();
        let pending = p.list_pending(&pool).await.unwrap();
        assert!(pending.is_empty());
    }

    /// A row whose first bar matches the current beat grid is healthy and
    /// must NOT be re-queued — sync writes that don't actually change BPM
    /// would otherwise thrash the classifier.
    #[tokio::test]
    async fn list_pending_skips_aligned_classifications() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        // 147 BPM × 4/4 → 1.6327s/bar. Insert beats and classifier with
        // matching span.
        insert_beats(&pool, "t1", 60.0 / 147.0 * 4.0).await;
        let p = ClassifierPreprocessor;
        insert_classifications(&pool, "t1", 60.0 / 147.0 * 4.0, p.version()).await;

        let pending = p.list_pending(&pool).await.unwrap();
        assert!(
            pending.is_empty(),
            "aligned classifier row must not be re-queued, got {pending:?}"
        );
    }

    /// A row whose bar duration disagrees with the current beat grid (the
    /// real-world bug: classifier ran at BPM=120, beats later overwritten
    /// to BPM=70) must be re-queued so the existing run-and-upsert path
    /// can self-heal it.
    #[tokio::test]
    async fn list_pending_requeues_stale_classifications() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        // Current grid is 70 BPM (3.43s/bar), but the cached classifier
        // output is at 120 BPM (2.0s/bar) — a real Relax-track scenario.
        insert_beats(&pool, "t1", 60.0 / 70.0 * 4.0).await;
        let p = ClassifierPreprocessor;
        insert_classifications(&pool, "t1", 2.0, p.version()).await;

        let pending = p.list_pending(&pool).await.unwrap();
        assert_eq!(pending, vec!["t1".to_string()]);
    }

    /// Failure-backoff still wins over staleness: a track in backoff
    /// shouldn't be retried until its window elapses, even if its row
    /// is stale. (Otherwise a permanently-broken classifier run on a track
    /// with churning beats would hammer the worker on every reconcile.)
    #[tokio::test]
    async fn list_pending_respects_failure_backoff_for_stale_rows() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        insert_beats(&pool, "t1", 60.0 / 70.0 * 4.0).await;
        let p = ClassifierPreprocessor;
        insert_classifications(&pool, "t1", 2.0, p.version()).await;
        sqlx::query(
            "INSERT INTO preprocessing_failures
                (track_id, preprocessor, version, last_error, last_attempt, next_retry_at)
             VALUES ('t1', 'classifier', ?, 'boom', '2099-01-01T00:00:00Z', '2099-01-01T00:00:00Z')",
        )
        .bind(p.version() as i64)
        .execute(&pool)
        .await
        .unwrap();

        let pending = p.list_pending(&pool).await.unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn classifier_lands_in_layer_after_beat_grid() {
        // Classifier depends on Audio + BeatGrid, so it must land strictly
        // after `beat_grid` in the topo order.
        let layered = topo_layers(&registry::registered_preprocessors());
        let layer_of = |name: &str| -> Option<usize> {
            layered
                .layers()
                .iter()
                .position(|layer| layer.iter().any(|p| p.name() == name))
        };
        let beat = layer_of("beat_grid").expect("beat_grid in registry");
        let cls = layer_of("classifier").expect("classifier in registry");
        assert!(
            cls > beat,
            "classifier ({cls}) must come after beat_grid ({beat})"
        );
    }

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

    #[test]
    fn bundled_classifier_weights_are_nonzero() {
        // Sanity: include_bytes! resolved against the real .pt file.
        // Bundled checkpoint should be ~1 MB; cheap protection against an
        // empty placeholder making it past review.
        assert!(classifier_worker::bundled_weights_len() > 100_000);
    }
}
