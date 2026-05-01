//! Core trait for preprocessing pipeline nodes.
//!
//! Each preprocessor declares **what** it is (name, version, DAG inputs and
//! output, the artifact table it persists into) and **how** it computes
//! ([`Preprocessor::run`]). The trait provides default implementations of
//! [`Preprocessor::is_complete`] and [`Preprocessor::list_pending`] derived
//! from those declarations — workers don't write SQL.
//!
//! Override [`Preprocessor::verify_disk`] for preprocessors whose output
//! includes side-effect files (e.g. stems on disk) so a user-deleted
//! directory triggers re-execution.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use tauri::AppHandle;

use crate::audio::StemCache;
use crate::models::tracks::TrackSummary;
use crate::preprocessing::artifact::Artifact;

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
    /// Stable wire name. Persisted in `preprocessing_failures.preprocessor`.
    /// Never rename — bump [`Preprocessor::version`] instead.
    fn name(&self) -> &'static str;

    /// Bumped when the output schema OR algorithm changes meaningfully.
    /// Stamped on every artifact row this preprocessor writes
    /// (`processor_version` column). Reconcile re-queues any track whose
    /// artifact is at a lower version.
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

    /// Local table this preprocessor writes its artifact rows into. The
    /// table must have `track_id TEXT` and `processor_version INTEGER`
    /// columns; default `is_complete` / `list_pending` query against it.
    fn artifact_table(&self) -> &'static str;

    /// How many artifact rows constitute completeness for one track.
    /// Beats and roots are 1-row-per-track (default); stems are 4.
    fn rows_per_track(&self) -> i64 {
        1
    }

    /// Run for one track. Implementations are responsible for persisting
    /// their typed outputs (e.g. to `track_beats`) with
    /// `processor_version = self.version()`, and any side-effect files.
    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String>;

    /// Optional disk-side check after the SQL completeness test passes.
    /// Stems override this to verify the OGG files weren't deleted.
    async fn verify_disk(
        &self,
        _ctx: &PreprocessorContext<'_>,
        _track_id: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Has this preprocessor's output been produced for this track at the
    /// current `version()`? Default: `rows_per_track` rows exist with
    /// `processor_version >= self.version()`, AND `verify_disk` passes.
    async fn is_complete(
        &self,
        ctx: &PreprocessorContext<'_>,
        track_id: &str,
    ) -> Result<bool, String> {
        // Table name is a `&'static str` from the trait — never user input.
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE track_id = ? AND processor_version >= ?",
            self.artifact_table()
        );
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(track_id)
            .bind(self.version() as i64)
            .fetch_one(ctx.pool())
            .await
            .map_err(|e| format!("{} is_complete: {e}", self.name()))?;
        if count < self.rows_per_track() {
            return Ok(false);
        }
        self.verify_disk(ctx, track_id).await
    }

    /// Bulk reconcile query: track IDs that need this preprocessor to run
    /// because the artifact is missing or stale. Excludes tracks currently
    /// in failure backoff. Default: tracks with eligible local audio whose
    /// artifact-row count at the current version is below `rows_per_track`.
    async fn list_pending(&self, pool: &SqlitePool) -> Result<Vec<String>, String> {
        let sql = format!(
            "SELECT t.id FROM tracks t
             WHERE t.file_path IS NOT NULL
               AND t.file_path != ''
               AND t.file_path NOT LIKE '%.stub'
               AND (
                   SELECT COUNT(*) FROM {table} a
                    WHERE a.track_id = t.id AND a.processor_version >= ?
               ) < ?
               AND NOT EXISTS (
                   SELECT 1 FROM preprocessing_failures f
                    WHERE f.track_id = t.id AND f.preprocessor = ?
                      AND f.next_retry_at > strftime('%Y-%m-%dT%H:%M:%SZ','now')
               )",
            table = self.artifact_table()
        );
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(self.version() as i64)
            .bind(self.rows_per_track())
            .bind(self.name())
            .fetch_all(pool)
            .await
            .map_err(|e| format!("{} list_pending: {e}", self.name()))
    }
}

/// Convenience alias used throughout the scheduler.
pub type PreprocessorRef = Arc<dyn Preprocessor>;
