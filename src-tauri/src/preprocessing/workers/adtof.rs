//! ADTOF drum-onset preprocessor.
//!
//! Runs `adtof_pytorch` (https://github.com/xavriley/ADTOF-pytorch) against
//! the demucs `drums.ogg` stem (cleaner than the full mix). Output is a JSON
//! blob `{midi_note: [t, ...]}` keyed by ADTOF's `LABELS_5`:
//! 35 = kick, 38 = snare, 47 = tom, 42 = hi-hat, 49 = cymbal/crash.
//!
//! Model weights (~3.5 MB) are bundled inside the upstream pip package;
//! `python_env.rs` installs the package via `python/adtof/requirements.txt`
//! so no separate download or `include_bytes!` is needed.
//!
//! Bumping `version` re-runs ADTOF for every track on next launch; do this
//! when the bundled weights change or the post-processing thresholds shift.

use std::path::Path;

use async_trait::async_trait;

use crate::adtof_worker;
use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::preprocessing::workers::stems::find_stem_file;

pub struct AdtofPreprocessor;

#[async_trait]
impl Preprocessor for AdtofPreprocessor {
    fn name(&self) -> &'static str {
        "adtof"
    }
    fn version(&self) -> u32 {
        1
    }
    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::Stems]
    }
    fn output(&self) -> Artifact {
        Artifact::DrumOnsets
    }
    fn status_label(&self) -> &'static str {
        "Transcribing drums…"
    }
    fn artifact_table(&self) -> &'static str {
        "track_drum_onsets"
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let track_stems_dir = ctx.stems_dir().join(&track.track_hash);
        let drums = find_stem_file(&track_stems_dir, "drums")
            .ok_or_else(|| format!("Missing drums stem for track {track_id}"))?;
        let handle = ctx.app_handle().clone();
        let drums_path: std::path::PathBuf = drums;

        let onsets = tauri::async_runtime::spawn_blocking(move || {
            adtof_worker::compute_drum_onsets(&handle, Path::new(&drums_path))
        })
        .await
        .map_err(|e| format!("ADTOF worker task failed: {e}"))??;

        let onsets_json = serde_json::to_string(&onsets.onsets)
            .map_err(|e| format!("Failed to serialize drum onsets: {e}"))?;

        tracks_db::upsert_track_drum_onsets(ctx.pool(), track_id, &onsets_json, self.version())
            .await
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use super::AdtofPreprocessor;
    use crate::preprocessing::preprocessor::Preprocessor;
    use crate::preprocessing::registry;
    use crate::preprocessing::scheduler::topo_layers;

    /// Spin up an in-memory pool with just enough schema to back the
    /// `is_complete` / `list_pending` defaults.
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
            "CREATE TABLE track_drum_onsets (
                track_id TEXT PRIMARY KEY,
                onsets_json TEXT NOT NULL,
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

    #[tokio::test]
    async fn list_pending_returns_tracks_without_onsets() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();

        let p = AdtofPreprocessor;
        let pending = p.list_pending(&pool).await.unwrap();
        assert_eq!(pending, vec!["t1".to_string()]);

        // Insert a current-version row → no longer pending.
        sqlx::query(
            "INSERT INTO track_drum_onsets (track_id, onsets_json, processor_version)
             VALUES ('t1', '{}', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let pending = p.list_pending(&pool).await.unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn adtof_lands_in_same_topo_layer_as_roots() {
        // The canonical registry already includes ADTOF; both `roots` and
        // `adtof` depend on `Stems` so they must land in the same topo layer.
        let layered = topo_layers(&registry::registered_preprocessors());
        let layer_of = |name: &str| -> Option<usize> {
            layered
                .layers()
                .iter()
                .position(|layer| layer.iter().any(|p| p.name() == name))
        };
        let roots_layer = layer_of("roots").expect("roots in registry");
        let adtof_layer = layer_of("adtof").expect("adtof in registry");
        assert_eq!(roots_layer, adtof_layer);
    }
}
