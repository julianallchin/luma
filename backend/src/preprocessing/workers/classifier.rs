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
//! weights ship inline via `include_bytes!` in `crate::preprocessing::workers::classifier::process`
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

pub(crate) mod process;

use std::path::Path;

use async_trait::async_trait;

use self::process::StoredBarClassifications;
use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{FirstBar, Preprocessor, PreprocessorContext};
use crate::preprocessing::workers::track_bar_boundaries;

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
    fn artifact_table(&self) -> &'static str {
        "track_bar_classifications"
    }

    /// The classifier's bars index the beat grid current when it ran, so a
    /// later grid change has to run it again.
    fn first_bar(&self) -> Option<FirstBar> {
        Some(FirstBar {
            column: "classifications_json",
            start: "$.first_bar[0]",
            end: "$.first_bar[1]",
        })
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let (fullmix_path, _drum_path) = tracks_db::get_track_mert_paths(ctx.pool(), track_id)
            .await?
            .ok_or_else(|| format!("Missing MERT cache row for track {track_id}"))?;
        // Classifier scores the full mix — drum stem cache is for n2n only.
        let mert_path: std::path::PathBuf = fullmix_path.into();

        // The bar axis mirrors TANGO's `_bar_boundaries_from_grid`, so MERT
        // segments here match what the classifier was trained against.
        let bar_boundaries = track_bar_boundaries(ctx.pool(), track_id).await?;
        if bar_boundaries.is_empty() {
            return Err(format!(
                "No bar boundaries derivable from track {track_id} (need a beat grid with ≥ 2 downbeats)"
            ));
        }

        let first_bar = bar_boundaries[0];
        let workers = ctx.workers().clone();
        let analysis = tokio::task::spawn_blocking(move || {
            process::classify_bars(&workers, Path::new(&mert_path), &bar_boundaries)
        })
        .await
        .map_err(|e| format!("Classifier worker task failed: {e}"))??;
        ctx.checkpoint()?;

        let stored = StoredBarClassifications::new(&analysis, first_bar)?;
        let classifications_json = serde_json::to_string(&stored)
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

    use super::process::{self, ClassifierAnalysis, StoredBarClassifications};
    use super::ClassifierPreprocessor;
    use crate::preprocessing::preprocessor::Preprocessor;

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
                uid TEXT,
                file_path TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE auth_write_admission (
                singleton INTEGER PRIMARY KEY,
                armed INTEGER NOT NULL DEFAULT 0,
                active_uid TEXT
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

    /// A beat grid whose first two downbeats are `first_bar`, at a tempo
    /// whose bar is shorter than that: a detected grid's first bar is often
    /// not the tempo's length.
    async fn insert_grid(pool: &SqlitePool, track_id: &str, first_bar: (f64, f64)) {
        sqlx::query(
            "INSERT INTO track_beats
                (track_id, beats_json, downbeats_json, bpm, beats_per_bar)
             VALUES (?, '[]', ?, 128.0, 4)",
        )
        .bind(track_id)
        .bind(format!(
            "[{},{},{}]",
            first_bar.0,
            first_bar.1,
            first_bar.1 + 1.875
        ))
        .execute(pool)
        .await
        .unwrap();
    }

    /// Classifier output computed against a grid whose first bar was
    /// `first_bar`.
    async fn insert_classifications(
        pool: &SqlitePool,
        track_id: &str,
        first_bar: (f64, f64),
        version: u32,
    ) {
        let json = format!(
            r#"{{"first_bar":[{},{}],"intensity":[1.0,2.0],"scores":[[0.1],[0.2]]}}"#,
            first_bar.0, first_bar.1
        );
        sqlx::query(
            r#"INSERT INTO track_bar_classifications
                (track_id, classifications_json, tag_order_json, processor_version)
             VALUES (?, ?, '["kick"]', ?)"#,
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
            r#"INSERT INTO track_bar_classifications
                (track_id, classifications_json, tag_order_json, processor_version)
             VALUES ('t1', '{"first_bar":[0.0,2.0],"intensity":[],"scores":[]}', '[]', ?)"#,
        )
        .bind(p.version() as i64)
        .execute(&pool)
        .await
        .unwrap();
        let pending = p.list_pending(&pool).await.unwrap();
        assert!(pending.is_empty());
    }

    /// A row computed against the current grid is current, so a sync write
    /// that doesn't move the grid doesn't rerun the classifier.
    #[tokio::test]
    async fn list_pending_skips_a_row_from_the_current_grid() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        insert_grid(&pool, "t1", (0.5, 2.5)).await;
        let p = ClassifierPreprocessor;
        insert_classifications(&pool, "t1", (0.5, 2.5), p.version()).await;

        let pending = p.list_pending(&pool).await.unwrap();
        assert!(
            pending.is_empty(),
            "a row from the current grid was re-queued: {pending:?}"
        );
    }

    /// A row computed against another grid is re-queued, so the run that
    /// follows can redo it, even when its first bar is as long as the
    /// current grid's.
    #[tokio::test]
    async fn list_pending_requeues_a_row_from_an_earlier_grid() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        insert_grid(&pool, "t1", (0.5, 2.5)).await;
        let p = ClassifierPreprocessor;
        insert_classifications(&pool, "t1", (0.0, 2.0), p.version()).await;

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
        insert_grid(&pool, "t1", (0.5, 2.5)).await;
        let p = ClassifierPreprocessor;
        insert_classifications(&pool, "t1", (0.0, 2.0), p.version()).await;
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

    /// The stored row is compact: one score array per bar in tag order,
    /// rounded to 3 decimals, with a null for every bar the worker skipped so
    /// index `i` stays bar `i` of the grid.
    #[test]
    fn stored_scores_follow_tag_order_and_keep_skipped_bars() {
        let analysis: ClassifierAnalysis = serde_json::from_str(
            r#"{"tag_order": ["kick", "hats"], "bars": [
                {"bar_idx": 0, "start": 0.5, "end": 2.5,
                 "predictions": {"hats": 0.12345, "intensity": 1.98765, "kick": 0.9}},
                {"bar_idx": 2, "start": 4.5, "end": 6.5,
                 "predictions": {"hats": 0.5, "intensity": 3.0, "kick": 0.0004}}
            ]}"#,
        )
        .unwrap();
        let stored = StoredBarClassifications::new(&analysis, (0.5, 2.5)).unwrap();
        let json: serde_json::Value = serde_json::to_value(&stored).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "first_bar": [0.5, 2.5],
                "intensity": [1.988, null, 3.0],
                "scores": [[0.9, 0.123], null, [0.0, 0.5]],
            })
        );
    }

    #[test]
    fn bundled_classifier_weights_are_nonzero() {
        // Sanity: include_bytes! resolved against the real .pt file.
        // Bundled checkpoint should be ~1 MB; cheap protection against an
        // empty placeholder making it past review.
        assert!(process::bundled_weights_len() > 100_000);
    }
}
