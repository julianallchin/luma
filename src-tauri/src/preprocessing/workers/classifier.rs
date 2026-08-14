//! Joint bar classifier preprocessor.
//!
//! Slices per-bar features out of the full-mix MERT-95M layer-7 cache (the
//! [`super::mert`] preprocessor's `.fullmix.npy`) and runs the bundled
//! `BarWindowClassifier` head against them. The same `mert` preprocessor
//! also writes a drum-stem cache for the n2n drum-onset preprocessor in
//! the same Python process, so MERT-95M loads once per track.
//!
//! Pre-v4 versions ran MERT *per bar* inside this worker. Sharing the cache
//! is faster (no redundant model load + per-bar passes), gives strictly more
//! context per bar (transformer attention spans 60 s chunks instead of one
//! bar), and unblocks dropping torch from the n2n worker side. Quality risk:
//! the head was trained against per-bar MERT, so the slice-from-global
//! features are subtly different. `version = 4` invalidates older rows so
//! reconcile-on-startup re-classifies every track on launch.
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
use crate::preprocessing::workers::{build_bar_boundaries, list_pending_bar_aligned};

pub struct ClassifierPreprocessor;

#[async_trait]
impl Preprocessor for ClassifierPreprocessor {
    fn name(&self) -> &'static str {
        "classifier"
    }
    fn version(&self) -> u32 {
        // v3: per-bar MERT (transformers, in-process).
        // v4: shared MERT cache (sliced per-bar from the .npy written by the
        //     `mert` preprocessor on the full mix).
        4
    }
    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::BeatGrid, Artifact::Mert]
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

    /// Bar-aligned staleness check — see [`list_pending_bar_aligned`]. The
    /// classifier's `bar_idx` indexes the beat grid that was current when it
    /// ran, so a later grid change has to re-queue the track.
    async fn list_pending(&self, pool: &SqlitePool) -> Result<Vec<String>, String> {
        list_pending_bar_aligned(
            pool,
            self.name(),
            self.version(),
            self.artifact_table(),
            "classifications_json",
            "$[0].start",
            "$[0].end",
        )
        .await
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let (fullmix_path, _drum_path) = tracks_db::get_track_mert_paths(ctx.pool(), track_id)
            .await?
            .ok_or_else(|| format!("Missing MERT cache row for track {track_id}"))?;
        // Classifier scores the full mix — drum stem cache is for n2n only.
        let mert_path: std::path::PathBuf = fullmix_path.into();

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
            classifier_worker::classify_bars(&handle, Path::new(&mert_path), &bar_boundaries)
        })
        .await
        .map_err(|e| format!("Classifier worker task failed: {e}"))??;
        ctx.checkpoint()?;

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

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use super::ClassifierPreprocessor;
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
    fn bundled_classifier_weights_are_nonzero() {
        // Sanity: include_bytes! resolved against the real .pt file.
        // Bundled checkpoint should be ~1 MB; cheap protection against an
        // empty placeholder making it past review.
        assert!(classifier_worker::bundled_weights_len() > 100_000);
    }
}
