//! StemsPreprocessor — Demucs separation via the python stem worker.
//!
//! Writes per-stem `.ogg` files to `<stems_dir>/<track_hash>/`, persists
//! `track_stems` rows (4 per track), and primes the in-memory [`StemCache`]
//! so node graphs that reference stems don't need to re-decode.
//!
//! Overrides [`Preprocessor::verify_disk`] to additionally check the OGG
//! files exist — a user-deleted stems directory triggers re-separation
//! even if the SQL rows are still present.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::audio::{load_or_decode_audio, stereo_to_mono};
use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
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
                self.version(),
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
