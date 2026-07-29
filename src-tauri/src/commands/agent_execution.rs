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
use crate::agent_execution::worker_process::{CancelToken, ExecStatus};
use crate::agent_execution::workspace::{
    CellOutcome, PythonWorkspaceService, Workspace, DEFAULT_CELL_TIMEOUT,
};
use crate::database::Db;
use crate::models::agent_execution::{PythonCellFigure, PythonCellResult, PythonScopeInput};
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
    let thread = crate::database::local::agent_threads::get_thread(pool, &thread_id)
        .await
        .map_err(|e| format!("agent thread '{thread_id}' is not available: {e}"))?
        .thread;

    let scope = BindingScope {
        agent_kind: thread.agent_kind,
        track_id: scope.track_id,
        venue_id: scope.venue_id,
        score_id: scope.score_id,
        pattern_id: scope.pattern_id,
        window: scope.window,
        graph_definition: scope.graph_definition,
    };
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

    let (cell_id, cancel) = claim(&thread_id)?;
    let workspace_for_cell = Arc::clone(&workspace);
    let executed = tokio::task::spawn_blocking(move || {
        workspace_for_cell.install_revision(&manifest)?;
        let outcome = workspace_for_cell.run_cell(&code, DEFAULT_CELL_TIMEOUT, &cancel);
        Ok::<_, String>(project(&workspace_for_cell, outcome))
    })
    .await;
    release(&thread_id, cell_id);

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

#[tauri::command]
pub async fn run_python_cell(
    app: AppHandle,
    db: State<'_, Db>,
    workspaces: State<'_, PythonWorkspaceService>,
    graph_runs: State<'_, GraphRunStore>,
    thread_id: String,
    code: String,
    scope: PythonScopeInput,
) -> Result<PythonCellResult, String> {
    let storage = StorageRoot::from_app(&app)?;
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;
    run_python_cell_inner(
        &db.0,
        &storage,
        &resource_root,
        &workspaces,
        &graph_runs,
        thread_id,
        code,
        scope,
    )
    .await
}

/// Interrupt the thread's running cell (the model-turn abort path, §16.1).
/// Returns whether there was one to interrupt.
#[tauri::command]
pub fn cancel_python_cell(thread_id: String) -> Result<bool, String> {
    Ok(cancel_python_cell_inner(&thread_id))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
