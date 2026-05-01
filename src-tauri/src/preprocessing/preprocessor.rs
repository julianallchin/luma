//! Core trait for preprocessing pipeline nodes.
//!
//! Every preprocessor declares its inputs (other artifacts it depends on) and
//! its single output artifact. The scheduler builds a DAG from those
//! declarations, then runs preprocessors in topological order. See
//! [`crate::preprocessing::scheduler`] for the dispatcher.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use tauri::AppHandle;

use crate::audio::StemCache;
use crate::models::tracks::TrackSummary;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::state;

/// Bundle of dependencies a preprocessor implementation needs at run time.
///
/// Constructed once per `run_for_track` call by the scheduler. Holds borrowed
/// references — implementations should never store the context past a single
/// `run` invocation.
pub struct PreprocessorContext<'a> {
    pool: &'a SqlitePool,
    app_handle: &'a AppHandle,
    stem_cache: &'a StemCache,
    track: &'a TrackSummary,
    /// Directory where stem files are written (per-track subdirs are derived
    /// from `track.track_hash`).
    stems_dir: PathBuf,
}

impl<'a> PreprocessorContext<'a> {
    pub fn new(
        pool: &'a SqlitePool,
        app_handle: &'a AppHandle,
        stem_cache: &'a StemCache,
        track: &'a TrackSummary,
        stems_dir: PathBuf,
    ) -> Self {
        Self {
            pool,
            app_handle,
            stem_cache,
            track,
            stems_dir,
        }
    }

    pub fn pool(&self) -> &SqlitePool {
        self.pool
    }

    pub fn app_handle(&self) -> &AppHandle {
        self.app_handle
    }

    pub fn stem_cache(&self) -> &StemCache {
        self.stem_cache
    }

    pub fn track(&self) -> &TrackSummary {
        self.track
    }

    pub fn stems_dir(&self) -> &std::path::Path {
        &self.stems_dir
    }
}

/// A node in the preprocessing DAG. See module docs for the contract.
#[async_trait]
pub trait Preprocessor: Send + Sync {
    /// Stable wire name. Stored in `preprocessing_runs.preprocessor`.
    /// Never rename — bump [`Preprocessor::version`] instead.
    fn name(&self) -> &'static str;

    /// Bumped when the output schema OR algorithm changes meaningfully. On
    /// startup, runs at older versions are treated as stale and re-queued.
    fn version(&self) -> u32;

    /// Artifacts this preprocessor depends on. The scheduler ensures all
    /// inputs have been produced before invoking [`Preprocessor::run`].
    /// `Artifact::Audio` is always available and may be listed.
    fn inputs(&self) -> &'static [Artifact];

    /// The single artifact this preprocessor produces. To produce two
    /// artifacts, split into two preprocessors (keeps the DAG simple).
    fn output(&self) -> Artifact;

    /// Human-readable string emitted to the UI as the run begins (e.g.
    /// "Analyzing beats…"). Frontend listens for these progress events.
    fn status_label(&self) -> &'static str;

    /// Run for one track. Implementations are responsible for persisting
    /// their typed outputs (e.g. to `track_beats`) and any side-effect files.
    /// The scheduler records completion in `preprocessing_runs` separately.
    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String>;

    /// Has this preprocessor's output already been produced for this track?
    /// Default impl checks `preprocessing_runs` at the current version.
    /// Implementations may override to additionally validate side-effects
    /// (e.g. confirm stem `.ogg` files exist on disk).
    async fn is_complete(
        &self,
        ctx: &PreprocessorContext<'_>,
        track_id: &str,
    ) -> Result<bool, String> {
        state::has_completed_run(ctx.pool(), track_id, self.name(), self.version()).await
    }
}

/// Convenience alias used throughout the scheduler.
pub type PreprocessorRef = Arc<dyn Preprocessor>;
