//! RootsPreprocessor — chord/root sections via the python ACE worker.
//!
//! Strict dependency on stems: roots reads the `bass` and `other` stems for
//! cleaner harmonic analysis. If stems are missing, this preprocessor
//! returns an error and the scheduler skips it for this run; it will retry
//! once stems exist.
//!
//! NOTE: This is a behaviour change from the legacy pipeline, which silently
//! fell back to the full mix. With the DAG, dependencies are explicit and a
//! missing input is a hard skip.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};
use crate::preprocessing::workers::stems::find_stem_file;
use crate::root_worker;

pub struct RootsPreprocessor;

#[async_trait]
impl Preprocessor for RootsPreprocessor {
    fn name(&self) -> &'static str {
        "roots"
    }

    fn version(&self) -> u32 {
        1
    }

    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::Stems]
    }

    fn output(&self) -> Artifact {
        Artifact::Roots
    }

    fn status_label(&self) -> &'static str {
        "Detecting key changes…"
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let track_stems_dir = ctx.stems_dir().join(&track.track_hash);
        let bass = find_stem_file(&track_stems_dir, "bass")
            .ok_or_else(|| format!("Missing bass stem for track {track_id}"))?;
        let other = find_stem_file(&track_stems_dir, "other")
            .ok_or_else(|| format!("Missing other stem for track {track_id}"))?;
        let sources: Vec<PathBuf> = vec![bass, other];
        let handle = ctx.app_handle().clone();

        let root_data = tauri::async_runtime::spawn_blocking(move || {
            root_worker::compute_roots(&handle, &sources)
        })
        .await
        .map_err(|e| format!("Root worker task failed: {e}"))??;

        let sections_json = serde_json::to_string(&root_data.sections)
            .map_err(|e| format!("Failed to serialize chord sections: {e}"))?;

        tracks_db::upsert_track_roots(
            ctx.pool(),
            track_id,
            &sections_json,
            root_data.logits_path.as_deref(),
        )
        .await
    }
}
