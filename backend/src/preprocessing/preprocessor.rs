//! Core trait for preprocessing pipeline nodes.
//!
//! Each preprocessor declares **what** it is (name, version, DAG inputs and
//! output, the artifact table it persists into) and **how** it computes
//! ([`Preprocessor::run`]). The trait provides default implementations of
//! [`Preprocessor::is_complete`] and [`Preprocessor::list_pending`] derived
//! from those declarations — workers don't write SQL. Both read one
//! definition of a current artifact, so the run path and startup reconcile
//! agree on what is left to do, files on disk apart.
//!
//! Override [`Preprocessor::verify_disk`] for preprocessors whose output
//! includes side-effect files (e.g. stems on disk) so a user-deleted
//! directory triggers re-execution. Declare [`Preprocessor::first_bar`] for
//! one whose rows are indexed by bar, so a moved beat grid reruns it.

use std::path::PathBuf;
use std::sync::Arc;

use crate::models::tracks::TrackSummary;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::{AnalysisGuard, WorkerEnvironment};
use crate::storage::StorageRoot;
use async_trait::async_trait;
use sqlx::SqlitePool;

/// Bundle of dependencies a preprocessor implementation needs at run time.
///
/// Constructed once per `run_for_track` call by the scheduler. Holds borrowed
/// references — implementations should never store the context past a single
/// `run` invocation.
pub struct PreprocessorContext<'a> {
    pool: &'a SqlitePool,
    storage: &'a StorageRoot,
    workers: &'a WorkerEnvironment,
    track: &'a TrackSummary,
    analysis: AnalysisGuard,
    /// Directory where stem files are written (per-track subdirs are derived
    /// from `track.track_hash`).
    stems_dir: PathBuf,
}

impl<'a> PreprocessorContext<'a> {
    pub fn new(
        pool: &'a SqlitePool,
        storage: &'a StorageRoot,
        workers: &'a WorkerEnvironment,
        track: &'a TrackSummary,
        stems_dir: PathBuf,
        analysis: AnalysisGuard,
    ) -> Self {
        Self {
            pool,
            storage,
            workers,
            track,
            stems_dir,
            analysis,
        }
    }

    pub fn pool(&self) -> &SqlitePool {
        self.pool
    }

    pub fn storage(&self) -> &StorageRoot {
        self.storage
    }

    pub fn workers(&self) -> &WorkerEnvironment {
        self.workers
    }

    pub fn track(&self) -> &TrackSummary {
        self.track
    }

    /// Reject a publication computed by an identity generation that has
    /// already been retired. Workers call this after expensive computation
    /// and immediately before persistent or process-memory side effects.
    pub fn checkpoint(&self) -> Result<(), String> {
        self.analysis.checkpoint()
    }

    /// Wait until this host's Python setup lets workers start
    /// ([`WorkerEnvironment::ready`]).
    pub async fn python_ready(&self) -> Result<(), String> {
        self.workers.ready(&self.analysis).await
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

    /// Local table this preprocessor writes its artifact rows into. The
    /// table must have `track_id TEXT` and `processor_version INTEGER`
    /// columns; default `is_complete` / `list_pending` query against it.
    fn artifact_table(&self) -> &'static str;

    /// How many artifact rows constitute completeness for one track.
    /// Beats and roots are 1-row-per-track (default); stems are 4.
    fn rows_per_track(&self) -> i64 {
        1
    }

    /// For a step whose rows are indexed by bar of the track's beat grid
    /// (bar `i` of `workers::track_bar_boundaries`): where a row records the
    /// first bar of the grid it ran against. Such a row describes the audio
    /// only while that grid is current. A later beat re-detection, or a sync
    /// pull of another device's grid, makes it stale, and the step runs again.
    fn first_bar(&self) -> Option<FirstBar> {
        None
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

    /// Has this preprocessor's output been produced for this track, and is
    /// it still current ([`artifact_current`])? Default: that, AND
    /// `verify_disk` passes.
    async fn is_complete(
        &self,
        ctx: &PreprocessorContext<'_>,
        track_id: &str,
    ) -> Result<bool, String> {
        let sql = format!(
            "SELECT {} FROM tracks t WHERE t.id = ?",
            artifact_current(self)
        );
        let current: Option<bool> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(track_id)
            .fetch_optional(ctx.pool())
            .await
            .map_err(|e| format!("{} is_complete: {e}", self.name()))?;
        if current != Some(true) {
            return Ok(false);
        }
        self.verify_disk(ctx, track_id).await
    }

    /// Bulk reconcile query: track IDs that need this preprocessor to run
    /// because the artifact is missing or stale. See [`pending_tracks`].
    async fn list_pending(&self, pool: &SqlitePool) -> Result<Vec<String>, String> {
        pending_tracks(pool, self).await
    }
}

/// Where a row records the first bar of the beat grid it was computed
/// against ([`Preprocessor::first_bar`]): SQLite JSON paths into one column
/// of the artifact table, e.g. `$.first_bar[0]` and `$.first_bar[1]`.
pub struct FirstBar {
    pub column: &'static str,
    pub start: &'static str,
    pub end: &'static str,
}

/// How far (seconds) a row's first bar may sit from the grid's first two
/// downbeats and still be that bar. Both are copies of the same numbers, so
/// only rounding separates them; a grid that moved moves by more.
const SAME_BAR_SECS: f64 = 0.001;

/// Holds for a track `t` whose `p` artifact is current: it has
/// `rows_per_track` rows at `p.version()` or later, and none of them was
/// computed against a beat grid the track no longer has
/// ([`Preprocessor::first_bar`]). The one statement of the rule in SQL: a
/// queued track skips a step for which it holds ([`Preprocessor::is_complete`]),
/// and reconcile queues a track for a step for which it doesn't
/// ([`pending_tracks`]). Only the run path also checks the step's files on
/// disk ([`Preprocessor::verify_disk`]), so reconcile doesn't notice deleted
/// stems or MERT caches; a run queued for another step redoes them.
///
/// A row is stale only on evidence: a bar boundary it records that the grid
/// has somewhere else. A value missing on either side, or JSON that doesn't
/// parse, is no evidence, so a row that can't record its first bar doesn't
/// rerun its step at every launch, and one bad row can't fail the query for
/// the whole library.
///
/// Only bar 0 is compared. That is enough while the beat-grid worker is the
/// only thing that writes a grid: a re-detection moves bar 0 too, and a synced
/// grid arrives with the owner's rows computed against it. If the grid ever
/// becomes editable by hand, an edit that leaves bar 0 alone goes unnoticed
/// here. Stamp each row with a revision of the grid it ran against then, and
/// compare that instead.
fn artifact_current(p: &(impl Preprocessor + ?Sized)) -> String {
    // Every interpolated fragment is a `&'static str` or a number from the
    // trait — never user input.
    let table = p.artifact_table();
    let rows = format!(
        "((SELECT COUNT(*) FROM {table} a
            WHERE a.track_id = t.id AND a.processor_version >= {version}) >= {rows})",
        version = p.version(),
        rows = p.rows_per_track(),
    );
    let Some(FirstBar { column, start, end }) = p.first_bar() else {
        return rows;
    };
    // `json_extract` raises on text that isn't JSON; `CASE` makes that NULL.
    let at = |json: &str, path: &str| {
        format!("CASE WHEN json_valid({json}) THEN json_extract({json}, '{path}') END")
    };
    let (row_start, row_end) = (
        at(&format!("a.{column}"), start),
        at(&format!("a.{column}"), end),
    );
    let (grid_start, grid_end) = (
        at("b.downbeats_json", "$[0]"),
        at("b.downbeats_json", "$[1]"),
    );
    format!(
        "({rows} AND NOT EXISTS (
            SELECT 1 FROM {table} a
              JOIN track_beats b ON b.track_id = a.track_id
             WHERE a.track_id = t.id
               AND (ABS(({row_start}) - ({grid_start})) >= {SAME_BAR_SECS:?}
                 OR ABS(({row_end}) - ({grid_end})) >= {SAME_BAR_SECS:?})
        ))"
    )
}

/// Holds for a track `t` whose synced analysis this device may write: the
/// signed-in principal owns it, or admission is not armed. A synced artifact
/// row is the track owner's. The server accepts it from nobody else, so the
/// device of a venue member who opens someone else's song reads the owner's
/// rows as they sync, and computes only the caches no one else sees.
pub(crate) const AUTHORS_SYNCED_ANALYSIS: &str = "NOT EXISTS (
    SELECT 1 FROM auth_write_admission admission
     WHERE admission.singleton = 1 AND admission.armed = 1
       AND t.uid IS NOT admission.active_uid)";

/// Whether `p`'s artifact table is synced, so its rows belong to the track's
/// owner (see [`AUTHORS_SYNCED_ANALYSIS`]).
pub(crate) fn writes_synced_rows(p: &(impl Preprocessor + ?Sized)) -> bool {
    crate::sync::schema::SYNCED_TABLES
        .iter()
        .any(|table| table.name == p.artifact_table())
}

/// Track IDs `p` should run for: tracks with local audio whose analysis this
/// device writes, not in failure backoff for `p`, whose artifact is missing
/// or stale ([`artifact_current`]).
pub async fn pending_tracks<P: Preprocessor + ?Sized>(
    pool: &SqlitePool,
    p: &P,
) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT t.id FROM tracks t
          WHERE t.file_path IS NOT NULL
            AND t.file_path != ''
            AND t.file_path NOT LIKE '%.stub'
            AND ({authors})
            AND NOT EXISTS (
                SELECT 1 FROM preprocessing_failures f
                 WHERE f.track_id = t.id AND f.preprocessor = ?1 AND {in_backoff}
            )
            AND NOT {current}",
        current = artifact_current(p),
        in_backoff = crate::preprocessing::failures::IN_BACKOFF,
        authors = if writes_synced_rows(p) {
            AUTHORS_SYNCED_ANALYSIS
        } else {
            "1"
        },
    );
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(p.name())
        .fetch_all(pool)
        .await
        .map_err(|e| format!("{} list_pending: {e}", p.name()))
}

/// Convenience alias used throughout the scheduler.
pub type PreprocessorRef = Arc<dyn Preprocessor>;
