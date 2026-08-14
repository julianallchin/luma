//! Per-bar genre preprocessor.
//!
//! Runs Discogs-EffNet (400 Discogs styles) over the full mix and folds its
//! ~1 s analysis patches onto Luma's bar axis, so the track agent can ask
//! "what is this bar's genre?" rather than only "what is this track?". The
//! inference logic lives in `python/genre_worker.py`; see
//! [`crate::genre_worker`] for the process bridge.
//!
//! Bar boundaries come from [`super::build_bar_boundaries`] — the same
//! function the bar classifier uses — so `features.bars[i]` and
//! `features.genres[i]` describe the same slice of audio.
//!
//! ⚠ The model weights are **CC BY-NC-ND 4.0** (Essentia / MTG-UPF model zoo)
//! and are not bundled: the user drops
//! `discogs-effnet-bsdynamic-1.onnx` into `<app config>/models/` themselves and
//! this preprocessor fails with an instructive error until they do. Because
//! that failure is recorded in `preprocessing_failures` with the standard
//! backoff, a library imported without the model simply carries no genre rows
//! and every other node proceeds normally. See `docs/genre-model.md`.

use std::path::Path;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::database::local::tracks as tracks_db;
use crate::genre_worker;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::preprocessing::workers::{build_bar_boundaries, list_pending_bar_aligned};
use crate::storage::StorageRoot;

pub struct GenrePreprocessor;

#[async_trait]
impl Preprocessor for GenrePreprocessor {
    fn name(&self) -> &'static str {
        "genre"
    }
    fn version(&self) -> u32 {
        1
    }
    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::Audio, Artifact::BeatGrid]
    }
    fn output(&self) -> Artifact {
        Artifact::Genre
    }
    fn status_label(&self) -> &'static str {
        "Detecting genre…"
    }
    fn artifact_table(&self) -> &'static str {
        "track_genres"
    }

    /// Bar-aligned staleness check — see [`list_pending_bar_aligned`]. Genre
    /// rows are indexed by `bar_idx` against the grid current at run time, so a
    /// later beat re-detection has to re-queue the track.
    async fn list_pending(&self, pool: &SqlitePool) -> Result<Vec<String>, String> {
        list_pending_bar_aligned(
            pool,
            self.name(),
            self.version(),
            self.artifact_table(),
            "genres_json",
            "$.bars[0].start",
            "$.bars[0].end",
        )
        .await
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
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

        let audio_path = ctx.track().file_path.clone();
        let handle = ctx.app_handle().clone();
        let storage = StorageRoot::from_app(ctx.app_handle())?;
        let analysis = tauri::async_runtime::spawn_blocking(move || {
            genre_worker::analyze_genres(&handle, &storage, Path::new(&audio_path), &bar_boundaries)
        })
        .await
        .map_err(|e| format!("Genre worker task failed: {e}"))??;
        ctx.checkpoint()?;

        // `bars` + `track_top` travel together as the artifact; `labels` is the
        // per-track taxonomy every index in them resolves against.
        let genres_json = serde_json::to_string(&serde_json::json!({
            "bars": analysis.bars,
            "track_top": analysis.track_top,
        }))
        .map_err(|e| format!("Failed to serialize genres: {e}"))?;
        let labels_json = serde_json::to_string(&analysis.labels)
            .map_err(|e| format!("Failed to serialize genre labels: {e}"))?;

        tracks_db::upsert_track_genres(
            ctx.pool(),
            track_id,
            &genres_json,
            &labels_json,
            self.version(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use super::GenrePreprocessor;
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

        for ddl in [
            "CREATE TABLE tracks (id TEXT PRIMARY KEY, file_path TEXT)",
            "CREATE TABLE track_beats (
                track_id TEXT PRIMARY KEY,
                beats_json TEXT NOT NULL,
                downbeats_json TEXT NOT NULL,
                bpm REAL,
                downbeat_offset REAL,
                beats_per_bar INTEGER,
                processor_version INTEGER NOT NULL DEFAULT 1
            )",
            "CREATE TABLE track_genres (
                track_id TEXT PRIMARY KEY,
                genres_json TEXT NOT NULL,
                labels_json TEXT NOT NULL,
                processor_version INTEGER NOT NULL DEFAULT 1
            )",
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
        ] {
            sqlx::query(ddl).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn insert_beats(pool: &SqlitePool, track_id: &str, bar_secs: f64) {
        let bpm = 60.0 * 4.0 / bar_secs;
        sqlx::query(
            "INSERT INTO track_beats (track_id, beats_json, downbeats_json, bpm, beats_per_bar)
             VALUES (?, '[]', '[]', ?, 4)",
        )
        .bind(track_id)
        .bind(bpm)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Worker output for a synthetic two-bar stretch starting at 0, in the
    /// exact `{"bars": [...], "track_top": [...]}` shape `run` persists.
    async fn insert_genres(pool: &SqlitePool, track_id: &str, bar_secs: f64, version: u32) {
        let json = format!(
            r#"{{"bars":[{{"bar_idx":0,"start":0.0,"end":{0},"top":[[0,0.9]]}},
                        {{"bar_idx":1,"start":{0},"end":{1},"top":[[0,0.8]]}}],
                "track_top":[[0,0.85]]}}"#,
            bar_secs,
            bar_secs * 2.0,
        );
        sqlx::query(
            "INSERT INTO track_genres (track_id, genres_json, labels_json, processor_version)
             VALUES (?, ?, '[\"Electronic---House\"]', ?)",
        )
        .bind(track_id)
        .bind(json)
        .bind(version as i64)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_pending_returns_tracks_without_genres() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();

        let p = GenrePreprocessor;
        assert_eq!(p.list_pending(&pool).await.unwrap(), vec!["t1".to_string()]);

        insert_beats(&pool, "t1", 2.0).await;
        insert_genres(&pool, "t1", 2.0, p.version()).await;
        assert!(p.list_pending(&pool).await.unwrap().is_empty());
    }

    /// A row whose bar duration disagrees with the current beat grid must be
    /// re-queued: `bar_idx` no longer points at the audio it described.
    #[tokio::test]
    async fn list_pending_requeues_rows_from_a_stale_beat_grid() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        // Grid is now 70 BPM (3.43 s/bar); the stored row was computed at 120.
        insert_beats(&pool, "t1", 60.0 / 70.0 * 4.0).await;
        let p = GenrePreprocessor;
        insert_genres(&pool, "t1", 2.0, p.version()).await;

        assert_eq!(p.list_pending(&pool).await.unwrap(), vec!["t1".to_string()]);
    }

    /// Failure backoff still wins over staleness, so a permanently-failing
    /// track (e.g. the ONNX weights were never installed) can't hammer the
    /// worker on every reconcile.
    #[tokio::test]
    async fn list_pending_respects_failure_backoff() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO preprocessing_failures
                (track_id, preprocessor, version, last_error, last_attempt, next_retry_at)
             VALUES ('t1', 'genre', 1, 'model missing', '2099-01-01T00:00:00Z', '2099-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(GenrePreprocessor
            .list_pending(&pool)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn genre_lands_after_beat_grid() {
        let layered = topo_layers(&registry::registered_preprocessors());
        let layer_of = |name: &str| -> Option<usize> {
            layered
                .layers()
                .iter()
                .position(|layer| layer.iter().any(|p| p.name() == name))
        };
        let beat = layer_of("beat_grid").expect("beat_grid in registry");
        let genre = layer_of("genre").expect("genre in registry");
        assert!(
            genre > beat,
            "genre ({genre}) must come after beat_grid ({beat})"
        );
    }
}
