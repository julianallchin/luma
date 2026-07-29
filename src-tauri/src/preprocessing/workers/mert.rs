//! MERT-95M feature-cache preprocessor.
//!
//! Computes per-track MERT-v1-95M layer-7 hidden states for two views of the
//! same track — the full mix and the demucs drum stem — and caches each to
//! its own `.npy` on disk. Both passes happen inside one Python process so
//! the 95 MB MERT model loads exactly once per track. On consumer laptops
//! the model-load + transformers import dominates a single track's MERT
//! cost; halving it would otherwise leak straight into perceived import
//! latency.
//!
//! The two caches are consumed by:
//!
//! - Bar classifier (`classifier`) — slices the full-mix cache per bar.
//! - n2n drum-onset preprocessor (`n2n`) — sliding-window inference over
//!   the drum-stem cache (v6+ checkpoints were trained on drum-isolated
//!   stems, so MERT conditioning must be on the drum stem too).
//!
//! Disk pressure: ~27 MB fp16 per cache for a 4-minute track × 2 caches =
//! ~54 MB. Acceptable for a desktop library; if it grows, switch to int8
//! quantization or share the HF cache across luma installs.

use async_trait::async_trait;
use std::path::PathBuf;

use crate::database::local::tracks as tracks_db;
use crate::mert_worker;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::preprocessing::workers::stems::find_stem_file;
use crate::storage::StorageRoot;

pub struct MertPreprocessor;

#[async_trait]
impl Preprocessor for MertPreprocessor {
    fn name(&self) -> &'static str {
        "mert"
    }
    fn version(&self) -> u32 {
        // v1: single full-mix cache (.npy) shared by classifier + n2n.
        // v2: dual cache — full mix for the classifier, drum stem for n2n.
        //     The drum model expects MERT computed on isolated drums (v6+
        //     n2n checkpoints), so n2n moved off the full-mix cache.
        // v3: delegates chunking to n2n.infer.compute_mert_features (30 s /
        //     15 s / 3 s, matching training). v2 used 60 / 30 / 5 — silent
        //     distribution shift that crushed n2n detections on real tracks.
        3
    }
    fn inputs(&self) -> &'static [Artifact] {
        // Stems is required so we can read drums.ogg for the second MERT
        // pass; Audio is still listed so the dependency on the raw file is
        // explicit even though it's transitively guaranteed.
        &[Artifact::Audio, Artifact::Stems]
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
        ctx: &PreprocessorContext<'_>,
        track_id: &str,
    ) -> Result<bool, String> {
        let paths = tracks_db::get_track_mert_paths(ctx.pool(), track_id).await?;
        Ok(match paths {
            Some((fullmix, drum)) => {
                !fullmix.is_empty()
                    && !drum.is_empty()
                    && std::path::Path::new(&fullmix).exists()
                    && std::path::Path::new(&drum).exists()
            }
            None => false,
        })
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let audio_path = PathBuf::from(&track.file_path);
        let stems_dir = ctx.stems_dir().join(&track.track_hash);
        let drum_path = find_stem_file(&stems_dir, "drums").ok_or_else(|| {
            format!(
                "Missing drums stem for track {track_id} under {}",
                stems_dir.display()
            )
        })?;

        let storage = StorageRoot::from_app(ctx.app_handle())?;
        let out_fullmix = storage.mert_fullmix_path(&track.track_hash);
        let out_drum = storage.mert_drum_path(&track.track_hash);
        let handle = ctx.app_handle().clone();

        let cache = tauri::async_runtime::spawn_blocking(move || {
            mert_worker::compute_mert_cache(
                &handle,
                &audio_path,
                &drum_path,
                &out_fullmix,
                &out_drum,
            )
        })
        .await
        .map_err(|e| format!("MERT worker task failed: {e}"))??;

        tracks_db::upsert_track_mert(
            ctx.pool(),
            track_id,
            &cache.fullmix_path.to_string_lossy(),
            &cache.drum_path.to_string_lossy(),
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
                drum_path TEXT NOT NULL DEFAULT '',
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
            "INSERT INTO track_mert (track_id, file_path, drum_path, processor_version)
             VALUES ('t1', '/cache/t1.fullmix.npy', '/cache/t1.drum.npy', ?)",
        )
        .bind(v)
        .execute(&pool)
        .await
        .unwrap();
        let pending = p.list_pending(&pool).await.unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn mert_lands_after_stems_and_before_classifier_and_n2n() {
        // mert reads drums.ogg, so it must come after `stems`; both
        // `classifier` and `n2n` declare Mert as an input, so they must come
        // after `mert`.
        let layered = topo_layers(&registry::registered_preprocessors());
        let layer_of = |name: &str| -> Option<usize> {
            layered
                .layers()
                .iter()
                .position(|layer| layer.iter().any(|p| p.name() == name))
        };
        let stems_layer = layer_of("stems").expect("stems in registry");
        let mert_layer = layer_of("mert").expect("mert in registry");
        let classifier_layer = layer_of("classifier").expect("classifier in registry");
        let n2n_layer = layer_of("n2n").expect("n2n in registry");
        assert!(
            stems_layer < mert_layer,
            "stems layer {stems_layer} must precede mert layer {mert_layer}",
        );
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
