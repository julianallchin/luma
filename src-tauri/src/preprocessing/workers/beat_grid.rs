//! BeatGridPreprocessor — runs `beat_this` via the python beat worker.
//!
//! Wraps the existing [`crate::beat_worker::compute_beats`] function; this
//! preprocessor owns scheduling and persistence only.

use std::path::Path;

use async_trait::async_trait;

use crate::beat_worker;
use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};

pub struct BeatGridPreprocessor;

#[async_trait]
impl Preprocessor for BeatGridPreprocessor {
    fn name(&self) -> &'static str {
        "beat_grid"
    }
    fn version(&self) -> u32 {
        1
    }
    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::Audio]
    }
    fn output(&self) -> Artifact {
        Artifact::BeatGrid
    }
    fn status_label(&self) -> &'static str {
        "Analyzing beats…"
    }
    fn artifact_table(&self) -> &'static str {
        "track_beats"
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let path = Path::new(&track.file_path).to_path_buf();
        let handle = ctx.app_handle().clone();

        let beat_data = tauri::async_runtime::spawn_blocking(move || {
            beat_worker::compute_beats(&handle, &path)
        })
        .await
        .map_err(|e| format!("Beat worker task failed: {e}"))??;

        let beats_json = serde_json::to_string(&beat_data.beats)
            .map_err(|e| format!("Failed to serialize beats: {e}"))?;
        let downbeats_json = serde_json::to_string(&beat_data.downbeats)
            .map_err(|e| format!("Failed to serialize downbeats: {e}"))?;

        tracks_db::upsert_track_beats(
            ctx.pool(),
            track_id,
            &beats_json,
            &downbeats_json,
            Some(beat_data.bpm as f64),
            Some(beat_data.downbeat_offset as f64),
            Some(beat_data.beats_per_bar as i64),
            self.version(),
        )
        .await
    }
}
