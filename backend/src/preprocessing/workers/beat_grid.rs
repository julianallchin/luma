//! BeatGridPreprocessor — runs `beat_this` via the python beat worker.
//!
//! Wraps the existing [`crate::preprocessing::workers::beat_grid::process::compute_beats`] function; this
//! preprocessor owns scheduling and persistence only.

pub(crate) mod process;

use std::path::Path;

use async_trait::async_trait;

use crate::database::local::tracks as tracks_db;
use crate::models::beat_grid::BeatGrid;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};

pub struct BeatGridPreprocessor;

#[async_trait]
impl Preprocessor for BeatGridPreprocessor {
    fn name(&self) -> &'static str {
        "beat_grid"
    }
    fn version(&self) -> u32 {
        9
    }
    fn inputs(&self) -> &'static [Artifact] {
        &[Artifact::Audio]
    }
    fn output(&self) -> Artifact {
        Artifact::BeatGrid
    }
    fn artifact_table(&self) -> &'static str {
        "track_beats"
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let path = Path::new(&track.file_path).to_path_buf();
        let workers = ctx.workers().clone();

        let beat_data =
            tokio::task::spawn_blocking(move || process::compute_beats(&workers, &path))
                .await
                .map_err(|e| format!("Beat worker task failed: {e}"))??;
        ctx.checkpoint()?;

        let grid = BeatGrid {
            beats: beat_data.beats,
            downbeats: beat_data.downbeats,
            bpm: beat_data.bpm,
            downbeat_offset: beat_data.downbeat_offset,
            beats_per_bar: beat_data.beats_per_bar,
        };
        tracks_db::record_track_beats(ctx.pool(), track_id, &grid, self.version()).await
    }
}
