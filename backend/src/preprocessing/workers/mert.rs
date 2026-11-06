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

pub(crate) mod process;

use async_trait::async_trait;
use std::path::PathBuf;

use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::preprocessing::workers::stems::find_stem_file;

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

        let storage = ctx.storage().clone();
        let out_fullmix = storage.mert_fullmix_path(&track.track_hash);
        let out_drum = storage.mert_drum_path(&track.track_hash);
        let workers = ctx.workers().clone();

        let cache = tokio::task::spawn_blocking(move || {
            process::compute_mert_cache(&workers, &audio_path, &drum_path, &out_fullmix, &out_drum)
        })
        .await
        .map_err(|e| format!("MERT worker task failed: {e}"))??;
        ctx.checkpoint()?;

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
