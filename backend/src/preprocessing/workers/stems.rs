//! StemsPreprocessor — Demucs separation via the python stem worker.
//!
//! Writes per-stem `.ogg` files to `<stems_dir>/<track_hash>/`, persists
//! `track_stems` rows (4 per track). Consumers decode a stem's PCM on demand.
//!
//! Overrides [`Preprocessor::verify_disk`] to additionally check the OGG
//! files exist — a user-deleted stems directory triggers re-separation
//! even if the SQL rows are still present.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::stem_worker;

/// Stem names we must verify on disk. Mirrors the Demucs `htdemucs` outputs.
const REQUIRED_STEM_NAMES: &[&str] = &["bass", "drums", "other", "vocals"];

pub struct StemsPreprocessor;

#[async_trait]
impl Preprocessor for StemsPreprocessor {
    fn name(&self) -> &'static str {
        "stems"
    }
    fn version(&self) -> u32 {
        1
    }
    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::Audio]
    }
    fn output(&self) -> Artifact {
        Artifact::Stems
    }
    fn artifact_table(&self) -> &'static str {
        "track_stems"
    }
    fn rows_per_track(&self) -> i64 {
        REQUIRED_STEM_NAMES.len() as i64
    }

    async fn verify_disk(
        &self,
        ctx: &PreprocessorContext<'_>,
        _track_id: &str,
    ) -> Result<bool, String> {
        let track_stems_dir = ctx.stems_dir().join(&ctx.track().track_hash);
        Ok(REQUIRED_STEM_NAMES
            .iter()
            .all(|stem| find_stem_file(&track_stems_dir, stem).is_some()))
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let track_path = PathBuf::from(&track.file_path);
        let stems_root = ctx.stems_dir().join(&track.track_hash);
        let workers = ctx.workers().clone();

        let stem_files = tokio::task::spawn_blocking(move || {
            stem_worker::separate_stems(&workers, &track_path, &stems_root)
        })
        .await
        .map_err(|e| format!("Stem worker task failed: {e}"))??;
        ctx.checkpoint()?;

        for stem in &stem_files {
            ctx.checkpoint()?;
            tracks_db::upsert_track_stem(
                ctx.pool(),
                track_id,
                &stem.name,
                &stem.path.to_string_lossy(),
                None,
                self.version(),
            )
            .await?;
        }

        Ok(())
    }
}

/// Find a stem file by name, checking `.ogg` first then `.flac` / `.wav` for
/// backwards compatibility with older runs. Re-exported from [`crate::storage`],
/// which owns the layout, so `StorageRoot::stem_source_path` and the
/// preprocessors probe identically.
pub(crate) use crate::storage::find_stem_file;
