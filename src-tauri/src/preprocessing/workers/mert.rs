//! MERT-95M feature-cache preprocessor.
//!
//! Computes per-track MERT-v1-95M layer-7 hidden states for the FULL-MIX
//! audio and caches them to a per-track `.npy` on disk. Both the bar
//! classifier (per-bar slicing) and the n2n drum-onset preprocessor
//! (full-song sliding-window inference) consume this cache instead of
//! running their own MERT extraction — one MERT compute per track total
//! instead of two.
//!
//! Heavy preprocessor: each invocation loads MERT-95M (~95 MB params) and
//! processes the full track in 60 s overlap-add chunks. The .npy is fp16,
//! ~27 MB for a 4-minute track. Disk pressure is acceptable for a desktop
//! library; if it grows, switch to int8 quantization or share the HF cache
//! across luma installs.

use async_trait::async_trait;
use std::path::PathBuf;

use crate::database::local::tracks as tracks_db;
use crate::mert_worker;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::services::tracks::mert_cache_dir;

pub struct MertPreprocessor;

#[async_trait]
impl Preprocessor for MertPreprocessor {
    fn name(&self) -> &'static str {
        "mert"
    }
    fn version(&self) -> u32 {
        1
    }
    fn inputs(&self) -> &'static [Artifact] {
        // Audio is sufficient — MERT runs on the full mix, not stems. This
        // unblocks downstream consumers (n2n, classifier) before stems
        // separation completes.
        &[Artifact::Audio]
    }
    fn output(&self) -> Artifact {
        Artifact::Mert
    }
    fn status_label(&self) -> &'static str {
        "Extracting MERT features…"
    }
    fn artifact_table(&self) -> &'static str {
        "track_mert"
    }

    async fn verify_disk(
        &self,
        _ctx: &PreprocessorContext<'_>,
        track_id: &str,
    ) -> Result<bool, String> {
        let path = tracks_db::get_track_mert_path(_ctx.pool(), track_id).await?;
        Ok(match path {
            Some(p) => std::path::Path::new(&p).exists(),
            None => false,
        })
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let audio_path = PathBuf::from(&track.file_path);
        let cache_dir = mert_cache_dir(ctx.app_handle())?;
        let out_path = cache_dir.join(format!("{}.npy", track.track_hash));
        let handle = ctx.app_handle().clone();

        let cache = tauri::async_runtime::spawn_blocking(move || {
            mert_worker::compute_mert_cache(&handle, &audio_path, &out_path)
        })
        .await
        .map_err(|e| format!("MERT worker task failed: {e}"))??;

        tracks_db::upsert_track_mert(
            ctx.pool(),
            track_id,
            &cache.path.to_string_lossy(),
            self.version(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use super::MertPreprocessor;
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
            "CREATE TABLE track_mert (
                track_id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
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
    async fn list_pending_returns_tracks_without_mert() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();

        let p = MertPreprocessor;
        let pending = p.list_pending(&pool).await.unwrap();
        assert_eq!(pending, vec!["t1".to_string()]);

        let v = p.version() as i64;
        sqlx::query(
            "INSERT INTO track_mert (track_id, file_path, processor_version)
             VALUES ('t1', '/cache/t1.npy', ?)",
        )
        .bind(v)
        .execute(&pool)
        .await
        .unwrap();
        let pending = p.list_pending(&pool).await.unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn mert_lands_before_classifier_and_n2n() {
        // Both `classifier` and `n2n` declare Mert as an input, so the
        // scheduler must place mert in an earlier topo layer.
        let layered = topo_layers(&registry::registered_preprocessors());
        let layer_of = |name: &str| -> Option<usize> {
            layered
                .layers()
                .iter()
                .position(|layer| layer.iter().any(|p| p.name() == name))
        };
        let mert_layer = layer_of("mert").expect("mert in registry");
        let classifier_layer = layer_of("classifier").expect("classifier in registry");
        let n2n_layer = layer_of("n2n").expect("n2n in registry");
        assert!(
            mert_layer < classifier_layer,
            "mert layer {mert_layer} must precede classifier layer {classifier_layer}",
        );
        assert!(
            mert_layer < n2n_layer,
            "mert layer {mert_layer} must precede n2n layer {n2n_layer}",
        );
    }
}
