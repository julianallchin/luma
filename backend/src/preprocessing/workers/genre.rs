//! Per-bar genre preprocessor.
//!
//! Runs Discogs-EffNet (400 Discogs styles) over the full mix and folds its
//! ~1 s analysis patches onto Luma's bar axis, so the track agent can ask
//! "what is this bar's genre?" rather than only "what is this track?". The
//! inference logic lives in `python/genre_worker.py`; see
//! [`crate::preprocessing::workers::genre::process`] for the process bridge.
//!
//! Bar boundaries come from [`super::track_bar_boundaries`] — the same
//! function the bar classifier uses — so `features.bars[i]` and
//! `features.genres[i]` describe the same slice of audio.
//!
//! ⚠ The model weights are **CC BY-NC-ND 4.0** (Essentia / MTG-UPF model zoo)
//! and are not bundled: `process::ensure_model` downloads them on first
//! use from MTG's own server, checksum-pinned, into `<app config>/models/`.
//! If the download fails (offline, checksum drift) the failure is recorded in
//! `preprocessing_failures` with the standard backoff, so a library imported
//! without connectivity simply carries no genre rows yet and every other node
//! proceeds normally. See `docs/design/genre-model.md`.

pub(crate) mod process;

use std::path::Path;

use async_trait::async_trait;

use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{FirstBar, Preprocessor, PreprocessorContext};
use crate::preprocessing::workers::track_bar_boundaries;

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
    fn artifact_table(&self) -> &'static str {
        "track_genres"
    }

    /// Genre rows index the beat grid current when the step ran, so a later
    /// beat re-detection has to run it again.
    fn first_bar(&self) -> Option<FirstBar> {
        Some(FirstBar {
            column: "genres_json",
            start: "$.bars[0].start",
            end: "$.bars[0].end",
        })
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let bar_boundaries = track_bar_boundaries(ctx.pool(), track_id).await?;
        if bar_boundaries.is_empty() {
            return Err(format!(
                "No bar boundaries derivable from track {track_id} (need a beat grid with ≥ 2 downbeats)"
            ));
        }

        let audio_path = ctx.track().file_path.clone();
        let workers = ctx.workers().clone();
        let storage = ctx.storage().clone();
        let analysis = tokio::task::spawn_blocking(move || {
            process::analyze_genres(&workers, &storage, Path::new(&audio_path), &bar_boundaries)
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
            "CREATE TABLE tracks (id TEXT PRIMARY KEY, uid TEXT, file_path TEXT)",
            "CREATE TABLE auth_write_admission (
                singleton INTEGER PRIMARY KEY,
                armed INTEGER NOT NULL DEFAULT 0,
                active_uid TEXT
            )",
            "CREATE TABLE track_beats (
                id TEXT PRIMARY KEY NOT NULL,
                track_id TEXT NOT NULL,
                beats_json TEXT NOT NULL,
                downbeats_json TEXT NOT NULL,
                bpm REAL,
                downbeat_offset REAL,
                beats_per_bar INTEGER,
                processor_version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
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

    /// A beat grid whose first two downbeats are `first_bar`, at a tempo
    /// whose bar is shorter than that: a detected grid's first bar is often
    /// not the tempo's length.
    async fn insert_grid(pool: &SqlitePool, track_id: &str, first_bar: (f64, f64)) {
        sqlx::query(
            "INSERT INTO track_beats
                (id, track_id, beats_json, downbeats_json, bpm, beats_per_bar)
             VALUES (lower(hex(randomblob(16))), ?, '[]', ?, 128.0, 4)",
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

    /// Worker output for two bars of a grid whose first bar was `first_bar`,
    /// in the exact `{"bars": [...], "track_top": [...]}` shape `run`
    /// persists.
    async fn insert_genres(pool: &SqlitePool, track_id: &str, first_bar: (f64, f64), version: u32) {
        let (start, end) = first_bar;
        let json = format!(
            r#"{{"bars":[{{"bar_idx":0,"start":{start},"end":{end},"top":[[0,0.9]]}},
                        {{"bar_idx":1,"start":{end},"end":{},"top":[[0,0.8]]}}],
                "track_top":[[0,0.85]]}}"#,
            end + 1.875,
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

        insert_grid(&pool, "t1", (0.5, 2.5)).await;
        insert_genres(&pool, "t1", (0.5, 2.5), p.version()).await;
        assert!(p.list_pending(&pool).await.unwrap().is_empty());
    }

    /// A row computed against another grid must be re-queued: `bar_idx` no
    /// longer points at the audio it described.
    #[tokio::test]
    async fn list_pending_requeues_rows_from_an_earlier_beat_grid() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        insert_grid(&pool, "t1", (0.5, 2.5)).await;
        let p = GenrePreprocessor;
        insert_genres(&pool, "t1", (0.0, 2.0), p.version()).await;

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
             VALUES ('t1', 'genre', ?, 'model missing', '2099-01-01T00:00:00Z', '2099-01-01T00:00:00Z')",
        )
        .bind(GenrePreprocessor.version() as i64)
        .execute(&pool)
        .await
        .unwrap();

        assert!(GenrePreprocessor
            .list_pending(&pool)
            .await
            .unwrap()
            .is_empty());
    }
}
