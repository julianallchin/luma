//! `run_python_cell` — the command that joins every piece of the executor.
//!
//! One call: resolve the thread, assemble a fresh binding revision from the
//! current scope (plus the thread's latest graph run), install it in the thread's
//! kernel, run the code, and project the outcome onto the notebook-native
//! [`PythonCellResult`] (design §15).
//!
//! The Tauri wrappers are thin: everything below [`run_python_cell_inner`] takes
//! plain handles, so the headless harness and the integration tests drive exactly
//! the same code path the app does.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use sqlx::SqlitePool;
use tauri::{AppHandle, State};

use crate::agent_execution::artifacts::{ArtifactEncoding, ArtifactKind};
use crate::agent_execution::bindings::providers::{
    assemble_bindings, BindingScope, GraphRunContribution,
};
use crate::agent_execution::graph_runs::GraphRunStore;
use crate::agent_execution::track_host::TrackHost;
use crate::agent_execution::worker_process::{CancelToken, ExecStatus};
use crate::agent_execution::workspace::{
    CellOutcome, PythonWorkspaceService, Workspace, DEFAULT_CELL_TIMEOUT,
};
use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::models::agent_execution::{PythonCellFigure, PythonCellResult, PythonScopeInput};
use crate::models::agent_threads::AgentThread;
use crate::services::track_edits::{TrackEditScope, TrackScope};
use crate::storage::StorageRoot;

/// How many bytes of PNG one cell may hand back to the model. Figures past the
/// budget are dropped with a notice rather than silently truncated — an
/// unexplained missing plot is worse than an explained one.
const FIGURE_BYTE_BUDGET: u64 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// In-flight cells
// ---------------------------------------------------------------------------

/// The cell currently running for one thread. The id pairs a cancel with the
/// execution that asked for it: a late `cancel_python_cell` for a cell that
/// already finished must not interrupt the next one (design §16.1).
struct RunningCell {
    id: u64,
    cancel: CancelToken,
}

fn running_cells() -> &'static Mutex<HashMap<String, RunningCell>> {
    static CELLS: OnceLock<Mutex<HashMap<String, RunningCell>>> = OnceLock::new();
    CELLS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Claim the thread's cell slot. `Err` when one is already in flight — the
/// workspace would serialize them anyway, but the second cell would silently
/// steal the first one's cancel slot.
fn claim(thread_id: &str) -> Result<(u64, CancelToken), String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let mut cells = running_cells().lock().unwrap();
    if cells.contains_key(thread_id) {
        return Err(format!(
            "a python cell is already running for agent thread '{thread_id}'"
        ));
    }
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let cancel = CancelToken::new();
    cells.insert(
        thread_id.to_string(),
        RunningCell {
            id,
            cancel: cancel.clone(),
        },
    );
    Ok((id, cancel))
}

/// Release the slot, but only if it is still ours.
fn release(thread_id: &str, id: u64) {
    let mut cells = running_cells().lock().unwrap();
    if cells.get(thread_id).is_some_and(|c| c.id == id) {
        cells.remove(thread_id);
    }
}

/// Releases a claimed cell slot on every exit path, including scope/binding
/// failures before the worker starts. The claim is intentionally acquired at
/// command entry so Stop can never miss a cell while its bindings are loading.
struct CellClaimGuard {
    thread_id: String,
    id: u64,
}

impl CellClaimGuard {
    fn new(thread_id: &str, id: u64) -> Self {
        Self {
            thread_id: thread_id.to_string(),
            id,
        }
    }
}

impl Drop for CellClaimGuard {
    fn drop(&mut self) {
        release(&self.thread_id, self.id);
    }
}

/// Interrupt the thread's in-flight cell. `false` when there is none.
pub fn cancel_python_cell_inner(thread_id: &str) -> bool {
    match running_cells().lock().unwrap().get(thread_id) {
        Some(cell) => {
            cell.cancel.cancel();
            true
        }
        None => false,
    }
}

struct ResolvedScope {
    bindings: BindingScope,
    /// Present whenever a durable track thread has a coherent score and venue.
    /// It permits high-fidelity read-only candidate rendering, never mutation.
    track: Option<TrackScope>,
    /// Present only when the durable thread scope and authenticated owner both
    /// authorize mutation. Python never supplies any field of this capability.
    track_edit: Option<TrackEditScope>,
}

async fn resolve_scope(
    pool: &SqlitePool,
    thread: &AgentThread,
    requested: PythonScopeInput,
    current_user_id: Option<&str>,
) -> Result<ResolvedScope, String> {
    match thread.agent_kind.as_str() {
        "track_copilot" => {
            if thread.subject_kind.as_deref() != Some("track") {
                return Err("track agent thread is not pinned to a track subject".into());
            }
            let track_id = thread
                .subject_id
                .clone()
                .ok_or_else(|| "track agent thread has no pinned track id".to_string())?;
            assert_pinned("track", requested.track_id.as_deref(), Some(&track_id))?;
            assert_pinned(
                "venue",
                requested.venue_id.as_deref(),
                thread.venue_id.as_deref(),
            )?;
            assert_pinned(
                "score",
                requested.score_id.as_deref(),
                thread.score_id.as_deref(),
            )?;

            let mut editable = false;
            let mut track_scope = None;
            let mut track_edit = None;
            if let (Some(venue_id), Some(score_id)) =
                (thread.venue_id.as_deref(), thread.score_id.as_deref())
            {
                let score = crate::database::local::scores::get_score(pool, score_id)
                    .await
                    .map_err(|error| {
                        format!("pinned score '{score_id}' is not available: {error}")
                    })?;
                if score.track_id != track_id || score.venue_id != venue_id {
                    return Err(format!(
                        "pinned score '{score_id}' does not belong to track '{track_id}' and venue '{venue_id}'"
                    ));
                }
                track_scope = Some(TrackScope {
                    score_id: score_id.to_string(),
                    track_id: track_id.clone(),
                    venue_id: venue_id.to_string(),
                });
                if let Some(user_id) = current_user_id
                    .filter(|user_id| score.uid.as_deref().is_some_and(|owner| owner == *user_id))
                {
                    editable = true;
                    track_edit = Some(TrackEditScope {
                        score_id: score_id.to_string(),
                        track_id: track_id.clone(),
                        venue_id: venue_id.to_string(),
                        user_id: user_id.to_string(),
                    });
                }
            } else if thread.venue_id.is_some() || thread.score_id.is_some() {
                return Err("track agent thread has an incomplete venue/score scope".into());
            }

            Ok(ResolvedScope {
                bindings: BindingScope {
                    agent_kind: thread.agent_kind.clone(),
                    track_id: Some(track_id),
                    venue_id: thread.venue_id.clone(),
                    score_id: thread.score_id.clone(),
                    track_editable: editable,
                    pattern_id: None,
                    window: requested.window,
                    graph_definition: None,
                },
                track: track_scope,
                track_edit,
            })
        }
        "pattern_graph" => {
            if thread.subject_kind.as_deref() != Some("pattern") {
                return Err("graph agent thread is not pinned to a pattern subject".into());
            }
            let pattern_id = thread
                .subject_id
                .clone()
                .ok_or_else(|| "graph agent thread has no pinned pattern id".to_string())?;
            assert_pinned(
                "pattern",
                requested.pattern_id.as_deref(),
                Some(&pattern_id),
            )?;
            Ok(ResolvedScope {
                bindings: BindingScope {
                    agent_kind: thread.agent_kind.clone(),
                    track_id: requested.track_id,
                    venue_id: requested.venue_id,
                    score_id: requested.score_id,
                    track_editable: false,
                    pattern_id: Some(pattern_id),
                    window: requested.window,
                    graph_definition: requested.graph_definition,
                },
                track: None,
                track_edit: None,
            })
        }
        other => Err(format!("unknown agent kind '{other}'")),
    }
}

fn assert_pinned(field: &str, requested: Option<&str>, pinned: Option<&str>) -> Result<(), String> {
    if requested.is_some_and(|requested| Some(requested) != pinned) {
        return Err(format!(
            "requested {field} scope does not match the durable agent thread"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The cell
// ---------------------------------------------------------------------------

/// Run one Python cell in `thread_id`'s kernel.
///
/// `scope` is what the *caller* is looking at; `agent_kind` is read from the
/// thread row, because it decides which branches of the namespace exist and is
/// not a caller's to assert.
#[allow(clippy::too_many_arguments)]
pub async fn run_python_cell_inner(
    pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    service: &PythonWorkspaceService,
    graph_runs: &GraphRunStore,
    thread_id: String,
    code: String,
    scope: PythonScopeInput,
) -> Result<PythonCellResult, String> {
    run_python_cell_inner_as(
        pool,
        storage,
        resource_root,
        service,
        graph_runs,
        thread_id,
        code,
        scope,
        None,
    )
    .await
}

/// The production cell path with server-derived user identity. The public
/// headless helper above intentionally has no edit authority.
#[allow(clippy::too_many_arguments)]
pub async fn run_python_cell_inner_as(
    pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    service: &PythonWorkspaceService,
    graph_runs: &GraphRunStore,
    thread_id: String,
    code: String,
    requested_scope: PythonScopeInput,
    current_user_id: Option<String>,
) -> Result<PythonCellResult, String> {
    // Claim before any database or binding work. Cancellation must cover the
    // whole command, not only the period after the Python worker has started.
    let (cell_id, cancel) = claim(&thread_id)?;
    let _claim = CellClaimGuard::new(&thread_id, cell_id);

    let thread = crate::database::local::agent_threads::get_thread(
        pool,
        &thread_id,
        current_user_id.as_deref(),
    )
    .await
    .map_err(|e| format!("agent thread '{thread_id}' is not available: {e}"))?
    .thread;

    let resolved =
        resolve_scope(pool, &thread, requested_scope, current_user_id.as_deref()).await?;
    let scope = resolved.bindings;
    let graph_run = graph_runs.latest(&thread_id).map(GraphRunContribution::new);
    let workspace = service.workspace_for(&thread_id)?;

    // Assembly is async (it reads the database) and holds the workspace's
    // artifact store, which is why that lock is a tokio one.
    let manifest = {
        let store = workspace.store();
        let mut store = store.lock().await;
        assemble_bindings(
            pool,
            storage,
            resource_root,
            &scope,
            graph_run.as_ref(),
            &mut store,
        )
        .await?
    };

    let workspace_for_cell = Arc::clone(&workspace);
    let edit_scope = resolved.track_edit;
    let host = resolved.track.map(|track_scope| {
        TrackHost::new(
            tokio::runtime::Handle::current(),
            pool.clone(),
            storage.clone(),
            resource_root.to_path_buf(),
            Arc::clone(&workspace),
            track_scope,
            edit_scope,
        )
    });
    let executed = tokio::task::spawn_blocking(move || {
        workspace_for_cell.install_revision(&manifest)?;
        let outcome = match host.as_ref() {
            Some(host) => {
                workspace_for_cell.run_cell_with_host(&code, DEFAULT_CELL_TIMEOUT, &cancel, host)
            }
            None => workspace_for_cell.run_cell(&code, DEFAULT_CELL_TIMEOUT, &cancel),
        };
        Ok::<_, String>(project(&workspace_for_cell, outcome))
    })
    .await;

    executed.map_err(|e| format!("the python cell task failed: {e}"))?
}

/// [`CellOutcome`] -> the model-facing result: statuses flattened, figures read
/// off disk and registered as generated artifacts.
fn project(workspace: &Workspace, outcome: CellOutcome) -> PythonCellResult {
    let mut notices = outcome.notices;
    notices.extend(
        outcome
            .warnings
            .iter()
            .map(|w| format!("Python worker warning: {w}")),
    );

    let status = match &outcome.status {
        ExecStatus::Ok => "ok",
        ExecStatus::Error => "error",
        ExecStatus::Interrupted => "interrupted",
        ExecStatus::Failed { reason } => {
            notices.push(format!("The Python cell could not be completed: {reason}"));
            "failed"
        }
    };

    let mut figures = Vec::new();
    let mut spent: u64 = 0;
    for figure in &outcome.figures {
        let path = workspace.dir().join(&figure.artifact_rel);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                notices.push(format!(
                    "A figure this cell produced could not be read back ({}): {e}",
                    figure.artifact_rel
                ));
                continue;
            }
        };
        let len = bytes.len() as u64;
        if spent + len > FIGURE_BYTE_BUDGET {
            notices.push(format!(
                "A figure was dropped: this cell's figures exceed the {} MB limit.",
                FIGURE_BYTE_BUDGET / (1024 * 1024)
            ));
            continue;
        }
        spent += len;

        // The store is the owner of record for anything under `outputs/`; it
        // re-validates the path the worker reported before trusting it.
        if let Err(e) = workspace.with_store(|store| {
            store.register_output(
                &figure.artifact_rel,
                ArtifactKind::Figure,
                ArtifactEncoding::Png,
            )
        }) {
            notices.push(format!(
                "A figure this cell produced was rejected ({}): {e}",
                figure.artifact_rel
            ));
            continue;
        }

        figures.push(PythonCellFigure {
            artifact_rel: figure.artifact_rel.clone(),
            width: figure.width,
            height: figure.height,
            base64_png: BASE64.encode(&bytes),
        });
    }

    PythonCellResult {
        status: status.to_string(),
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        repr: outcome.repr,
        traceback: outcome.traceback,
        figures,
        notices,
        duration_ms: outcome.duration_ms,
    }
}

// ---------------------------------------------------------------------------
// Tauri wrappers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn run_python_cell(
    app: AppHandle,
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    workspaces: State<'_, PythonWorkspaceService>,
    graph_runs: State<'_, GraphRunStore>,
    thread_id: String,
    code: String,
    scope: PythonScopeInput,
) -> Result<PythonCellResult, String> {
    let storage = StorageRoot::from_app(&app)?;
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;
    let current_user_id = crate::database::local::auth::get_current_user_id(&state_db.0).await?;
    run_python_cell_inner_as(
        &db.0,
        &storage,
        &resource_root,
        &workspaces,
        &graph_runs,
        thread_id,
        code,
        scope,
        current_user_id,
    )
    .await
}

/// Interrupt the thread's running cell (the model-turn abort path, §16.1).
/// Returns whether there was one to interrupt.
#[tauri::command]
pub async fn cancel_python_cell(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    thread_id: String,
) -> Result<bool, String> {
    let current_user_id = crate::database::local::auth::get_current_user_id(&state_db.0).await?;
    crate::database::local::agent_threads::get_thread_row(
        &db.0,
        &thread_id,
        current_user_id.as_deref(),
    )
    .await
    .map_err(|e| format!("agent thread '{thread_id}' is not available: {e}"))?;
    Ok(cancel_python_cell_inner(&thread_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned_track_thread() -> AgentThread {
        AgentThread {
            id: "thread".into(),
            owner_user_id: Some("owner".into()),
            agent_kind: "track_copilot".into(),
            subject_kind: Some("track".into()),
            subject_id: Some("track".into()),
            venue_id: Some("venue".into()),
            score_id: Some("score".into()),
            title: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    async fn scope_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE scores (
                id TEXT PRIMARY KEY,
                uid TEXT,
                track_id TEXT NOT NULL,
                venue_id TEXT NOT NULL,
                name TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             INSERT INTO scores
                (id, uid, track_id, venue_id, name, created_at, updated_at)
             VALUES ('score', 'owner', 'track', 'venue', NULL, '', '');",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[test]
    fn a_late_cancel_cannot_hit_the_next_cell() {
        let thread = format!("t-{}", uuid::Uuid::new_v4());
        let (id, cancel) = claim(&thread).unwrap();
        // A second cell for the same thread is refused while the first holds
        // the slot.
        assert!(claim(&thread).is_err());

        assert!(cancel_python_cell_inner(&thread));
        assert!(cancel.is_cancelled());
        release(&thread, id);

        // The cancel that chased the finished cell finds nothing, and the next
        // cell gets a fresh token.
        assert!(!cancel_python_cell_inner(&thread));
        let (next_id, next_cancel) = claim(&thread).unwrap();
        assert!(!next_cancel.is_cancelled());

        // Releasing under a stale id must not free the live slot.
        release(&thread, id);
        assert!(cancel_python_cell_inner(&thread));
        release(&thread, next_id);
    }

    #[test]
    fn a_cell_claim_is_released_when_setup_exits_early() {
        let thread = format!("t-{}", uuid::Uuid::new_v4());
        {
            let (id, _) = claim(&thread).unwrap();
            let _claim = CellClaimGuard::new(&thread, id);
            assert!(cancel_python_cell_inner(&thread));
        }
        assert!(!cancel_python_cell_inner(&thread));
        let (next_id, _) = claim(&thread).unwrap();
        release(&thread, next_id);
    }

    #[tokio::test]
    async fn track_scope_is_derived_from_the_thread_and_auth() {
        let pool = scope_pool().await;
        let thread = pinned_track_thread();
        let resolved = resolve_scope(&pool, &thread, PythonScopeInput::default(), Some("owner"))
            .await
            .unwrap();
        assert_eq!(resolved.bindings.track_id.as_deref(), Some("track"));
        assert_eq!(resolved.bindings.venue_id.as_deref(), Some("venue"));
        assert_eq!(resolved.bindings.score_id.as_deref(), Some("score"));
        assert!(resolved.bindings.track_editable);
        assert_eq!(resolved.track_edit.unwrap().user_id, "owner");

        let read_only = resolve_scope(
            &pool,
            &thread,
            PythonScopeInput::default(),
            Some("someone-else"),
        )
        .await
        .unwrap();
        assert!(!read_only.bindings.track_editable);
        assert_eq!(read_only.track.as_ref().unwrap().score_id, "score");
        assert!(read_only.track_edit.is_none());
    }

    #[tokio::test]
    async fn incoming_ids_cannot_retarget_a_durable_track_thread() {
        let pool = scope_pool().await;
        let requested = PythonScopeInput {
            track_id: Some("another-track".into()),
            ..Default::default()
        };
        let error = resolve_scope(&pool, &pinned_track_thread(), requested, Some("owner"))
            .await
            .err()
            .unwrap();
        assert!(error.contains("does not match the durable agent thread"));
    }
}
