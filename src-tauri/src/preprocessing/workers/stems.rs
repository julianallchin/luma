//! StemsPreprocessor — Demucs separation via the python stem worker.
//!
//! Writes per-stem `.ogg` files to `<stems_dir>/<track_hash>/`, persists
//! `track_stems` rows, and primes the in-memory [`StemCache`] so node graphs
//! that reference stems don't need to re-decode.
//!
//! Override of [`Preprocessor::is_complete`] additionally verifies the ogg
//! files still exist on disk — the user can manually delete the stems
//! directory and we want to detect that.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::audio::{load_or_decode_audio, stereo_to_mono};
use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::preprocessing::state;
use crate::services::tracks::TARGET_SAMPLE_RATE;
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

    fn status_label(&self) -> &'static str {
        "Separating stems…"
    }

    async fn is_complete(
        &self,
        ctx: &PreprocessorContext<'_>,
        track_id: &str,
    ) -> Result<bool, String> {
        if !state::has_completed_run(ctx.pool(), track_id, self.name(), self.version()).await? {
            return Ok(false);
        }
        let track_stems_dir = ctx.stems_dir().join(&ctx.track().track_hash);
        for stem_name in REQUIRED_STEM_NAMES {
            if find_stem_file(&track_stems_dir, stem_name).is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let track_path = PathBuf::from(&track.file_path);
        let stems_root = ctx.stems_dir().join(&track.track_hash);
        let handle = ctx.app_handle().clone();

        let stem_files = tauri::async_runtime::spawn_blocking(move || {
            stem_worker::separate_stems(&handle, &track_path, &stems_root)
        })
        .await
        .map_err(|e| format!("Stem worker task failed: {e}"))??;

        for stem in &stem_files {
            tracks_db::upsert_track_stem(
                ctx.pool(),
                track_id,
                &stem.name,
                &stem.path.to_string_lossy(),
                None,
            )
            .await?;
        }

        // Prime the in-memory stem cache so node graphs don't re-decode.
        for stem in &stem_files {
            let cache_tag = format!("{}_stem_{}", track.track_hash, stem.name);
            if let Ok(audio) = load_or_decode_audio(&stem.path, &cache_tag, TARGET_SAMPLE_RATE) {
                if !audio.samples.is_empty() && audio.sample_rate > 0 {
                    let mono = stereo_to_mono(&audio.samples);
                    ctx.stem_cache().insert(
                        track_id,
                        stem.name.clone(),
                        mono.into(),
                        audio.sample_rate,
                    );
                }
            }
        }

        Ok(())
    }
}

/// Find a stem file by name, checking `.ogg` first then `.flac` / `.wav` for
/// backwards compatibility with older runs.
pub(crate) fn find_stem_file(stems_dir: &Path, stem_name: &str) -> Option<PathBuf> {
    for ext in ["ogg", "flac", "wav"] {
        let path = stems_dir.join(format!("{stem_name}.{ext}"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}
