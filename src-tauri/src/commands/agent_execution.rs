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

use std::path::Path;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tauri::{AppHandle, State};

use crate::agent_execution::artifacts::{ArtifactEncoding, ArtifactKind};
use crate::agent_execution::bindings::providers::{
    assemble_bindings, BindingScope, GraphRunContribution,
};
use crate::agent_execution::graph_runs::GraphRunStore;
use crate::agent_execution::track_host::TrackHost;
use crate::agent_execution::worker_process::{ExecStatus, HostOperationScope};
use crate::agent_execution::workspace::{
    CellOutcome, PythonWorkspaceService, Workspace, DEFAULT_CELL_TIMEOUT,
};
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::database::Db;
use crate::models::agent_execution::{PythonCellFigure, PythonCellResult, PythonScopeInput};
use crate::models::agent_threads::AgentThread;
use crate::models::authored_state::AuthoredProjectedDocument;
use crate::services::authored_documents::AuthoredDocuments;
use crate::services::track_edits::{TrackEditScope, TrackScope};
use crate::storage::StorageRoot;

/// How many bytes of PNG one cell may hand back to the model. Figures past the
/// budget are dropped with a notice rather than silently truncated — an
/// unexplained missing plot is worse than an explained one.
const FIGURE_BYTE_BUDGET: u64 = 8 * 1024 * 1024;
const MAX_TURN_MESSAGE_ID_BYTES: usize = 512;

fn host_operation_scope(
    thread_id: &str,
    execution_id: &str,
    turn_message_id: &str,
) -> Result<HostOperationScope, String> {
    if turn_message_id.is_empty()
        || turn_message_id.len() > MAX_TURN_MESSAGE_ID_BYTES
        || turn_message_id.chars().any(char::is_control)
    {
        return Err("Python turn message id is invalid".into());
    }
    let operation_namespace = cell_digest(
        b"luma.python-cell-operation-namespace.v1",
        &[thread_id, execution_id, turn_message_id],
    );
    Ok(HostOperationScope::new(operation_namespace))
}

fn cell_digest(domain: &[u8], parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("sha256:{:x}", hash.finalize())
}

/// Interrupt the thread's in-flight cell. `false` when there is none.
pub fn cancel_python_cell_inner(service: &PythonWorkspaceService, thread_id: &str) -> bool {
    service.cancel_cell(thread_id)
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
            if crate::database::local::tracks::get_track_by_id(pool, &track_id)
                .await?
                .is_none()
            {
                return Err("pinned track is not available".into());
            }
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
                let mut access = VenueAccess::<Read>::read(pool, VenueResource::Score(score_id))
                    .await
                    .map_err(|error| {
                        format!("pinned score '{score_id}' is not available: {error}")
                    })?;
                access.require_venue(venue_id)?;
                let score = crate::database::local::scores::get_score(&mut access, score_id)
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
                    track_document: None,
                    pattern_id: None,
                    implementation_id: None,
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
            let implementation_id = thread
                .implementation_id
                .clone()
                .ok_or_else(|| "graph agent thread has no pinned implementation id".to_string())?;
            assert_pinned(
                "pattern",
                requested.pattern_id.as_deref(),
                Some(&pattern_id),
            )?;
            assert_pinned(
                "implementation",
                requested.implementation_id.as_deref(),
                Some(&implementation_id),
            )?;
            crate::services::graph_documents::load_visible_graph_document(
                pool,
                &pattern_id,
                requested.venue_id.as_deref(),
                Some(&implementation_id),
            )
            .await
            .map_err(|error| error.to_string())?;
            if let Some(track_id) = requested.track_id.as_deref() {
                if crate::database::local::tracks::get_track_by_id(pool, track_id)
                    .await?
                    .is_none()
                {
                    return Err("requested track is not available".into());
                }
            }
            Ok(ResolvedScope {
                bindings: BindingScope {
                    agent_kind: thread.agent_kind.clone(),
                    track_id: requested.track_id,
                    venue_id: requested.venue_id,
                    score_id: requested.score_id,
                    track_editable: false,
                    track_document: None,
                    pattern_id: Some(pattern_id),
                    implementation_id: Some(implementation_id),
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
    authored: &AuthoredDocuments,
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
        authored,
        thread_id,
        code,
        scope,
        None,
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
    authored: &AuthoredDocuments,
    thread_id: String,
    code: String,
    requested_scope: PythonScopeInput,
    turn_message_id: Option<String>,
    current_user_id: Option<String>,
) -> Result<PythonCellResult, String> {
    run_python_cell_inner_as_scoped(
        pool,
        storage,
        resource_root,
        service,
        graph_runs,
        authored,
        thread_id,
        code,
        requested_scope,
        turn_message_id,
        current_user_id,
        None,
        None,
    )
    .await
}

/// Execute against either the durable parent namespace or one authenticated
/// detached child workspace. The durable thread remains the authorization
/// principal; execution/workspace ids only select isolated ephemeral and
/// detached state.
#[allow(clippy::too_many_arguments)]
pub async fn run_python_cell_inner_as_scoped(
    pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    service: &PythonWorkspaceService,
    graph_runs: &GraphRunStore,
    authored: &AuthoredDocuments,
    thread_id: String,
    code: String,
    requested_scope: PythonScopeInput,
    turn_message_id: Option<String>,
    current_user_id: Option<String>,
    execution_id: Option<String>,
    authored_workspace_id: Option<String>,
) -> Result<PythonCellResult, String> {
    let execution_id = match (execution_id, authored_workspace_id.as_deref()) {
        (None, None) => thread_id.clone(),
        (Some(execution_id), Some(workspace_id)) if execution_id == workspace_id => execution_id,
        (Some(_), Some(_)) => {
            return Err("child Python execution id must match its authored workspace id".into())
        }
        _ => {
            return Err(
                "child Python execution requires both execution and authored workspace ids".into(),
            )
        }
    };
    if let Some(workspace_id) = authored_workspace_id.as_deref() {
        authored
            .authorize_workspace(pool, current_user_id.as_deref(), &thread_id, workspace_id)
            .await
            .map_err(|error| error.to_string())?;
    }
    // The lifecycle lease begins before any database or binding work. Deletion
    // closes this admission gate, cancels us, and drains the guard before it can
    // remove the Python workspace or authored child workspaces.
    let lease = service.claim_cell(&execution_id)?;
    let cancel = lease.cancel_token();

    let thread = crate::database::local::agent_threads::get_thread(
        pool,
        &thread_id,
        current_user_id.as_deref(),
    )
    .await
    .map_err(|e| format!("agent thread '{thread_id}' is not available: {e}"))?;

    let operation_scope = match turn_message_id.as_deref() {
        Some(turn_message_id) => {
            let message = thread
                .messages
                .iter()
                .find(|message| message.id == turn_message_id)
                .ok_or_else(|| {
                    "Python turn message is not durable in this agent thread".to_string()
                })?;
            if message.role != "user" {
                return Err("Python turn message must be a durable user message".into());
            }
            Some(host_operation_scope(
                &thread_id,
                &execution_id,
                turn_message_id,
            )?)
        }
        None => None,
    };
    let thread = thread.thread;

    let mut resolved =
        resolve_scope(pool, &thread, requested_scope, current_user_id.as_deref()).await?;
    if let Some(workspace_id) = authored_workspace_id.as_deref() {
        if resolved.track.is_some() {
            let workspace = authored
                .track_workspace(pool, current_user_id.as_deref(), &thread_id, workspace_id)
                .await
                .map_err(|error| error.to_string())?;
            if resolved.track.as_ref() != Some(&workspace.scope) {
                return Err("authored workspace does not match the resolved track scope".into());
            }
            resolved.bindings.track_document = Some(workspace.document);
        } else {
            let workspace = authored
                .check_workspace(pool, current_user_id.as_deref(), &thread_id, workspace_id)
                .await
                .map_err(|error| error.to_string())?;
            let AuthoredProjectedDocument::PatternGraph { graph, .. } = workspace.document else {
                return Err("authored workspace does not match the resolved pattern scope".into());
            };
            resolved.bindings.graph_definition = Some(
                serde_json::to_value(graph)
                    .map_err(|error| format!("encode workspace graph binding: {error}"))?,
            );
        }
    }
    if resolved.track_edit.is_some() && operation_scope.is_none() {
        return Err("editable Python cells require a durable user turn message".into());
    }
    let scope = resolved.bindings;
    let graph_run = graph_runs
        .latest(&execution_id)
        .map(GraphRunContribution::new);
    let workspace = service.workspace_for_cell(&lease)?;

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
            authored.clone(),
            thread_id.clone(),
            track_scope,
            edit_scope,
            authored_workspace_id.clone(),
        )
    });
    let executed = tokio::task::spawn_blocking(move || {
        // Once blocking execution begins, the task itself owns admission. If
        // the async command future is dropped, deletion still cannot overtake
        // the detached kernel/host-call work.
        let _lease = lease;
        workspace_for_cell.install_revision(&manifest)?;
        let outcome = match host.as_ref() {
            Some(host) => workspace_for_cell.run_cell_with_host(
                &code,
                DEFAULT_CELL_TIMEOUT,
                &cancel,
                host,
                operation_scope.as_ref(),
            ),
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
    workspaces: State<'_, PythonWorkspaceService>,
    graph_runs: State<'_, GraphRunStore>,
    authored: State<'_, AuthoredDocuments>,
    thread_id: String,
    execution_id: Option<String>,
    authored_workspace_id: Option<String>,
    turn_message_id: String,
    code: String,
    scope: PythonScopeInput,
) -> Result<PythonCellResult, String> {
    let storage = StorageRoot::from_app(&app)?;
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;
    let current_user_id = crate::database::local::auth::admitted_principal(&db.0).await?;
    run_python_cell_inner_as_scoped(
        &db.0,
        &storage,
        &resource_root,
        &workspaces,
        &graph_runs,
        &authored,
        thread_id,
        code,
        scope,
        Some(turn_message_id),
        current_user_id,
        execution_id,
        authored_workspace_id,
    )
    .await
}

/// Interrupt the thread's running cell (the model-turn abort path, §16.1).
/// Returns whether there was one to interrupt.
#[tauri::command]
pub async fn cancel_python_cell(
    db: State<'_, Db>,
    workspaces: State<'_, PythonWorkspaceService>,
    authored: State<'_, AuthoredDocuments>,
    thread_id: String,
    execution_id: Option<String>,
    authored_workspace_id: Option<String>,
) -> Result<bool, String> {
    let current_user_id = crate::database::local::auth::admitted_principal(&db.0).await?;
    crate::database::local::agent_threads::get_thread_row(
        &db.0,
        &thread_id,
        current_user_id.as_deref(),
    )
    .await
    .map_err(|e| format!("agent thread '{thread_id}' is not available: {e}"))?;
    let execution_id = match (execution_id, authored_workspace_id.as_deref()) {
        (None, None) => thread_id.clone(),
        (Some(execution_id), Some(workspace_id)) if execution_id == workspace_id => {
            authored
                .authorize_workspace(&db.0, current_user_id.as_deref(), &thread_id, workspace_id)
                .await
                .map_err(|error| error.to_string())?;
            execution_id
        }
        (Some(_), Some(_)) => {
            return Err("child Python execution id must match its authored workspace id".into())
        }
        _ => {
            return Err(
                "child Python execution requires both execution and authored workspace ids".into(),
            )
        }
    };
    Ok(cancel_python_cell_inner(&workspaces, &execution_id))
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
            implementation_id: None,
            venue_id: Some("venue".into()),
            score_id: Some("score".into()),
            forked_from_thread_id: None,
            forked_at_message_id: None,
            title: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    async fn scope_pool() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("scope.db");
        let migrate_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&database)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .unwrap();
        migrate_pool.close().await;
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(database)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, title, duration_seconds, file_path)
             VALUES ('track', 'owner', 'track-hash', 'Track', 1.0, 'track.wav');
             INSERT INTO venues (id, uid, name)
             VALUES ('venue', 'owner', 'Venue');
             INSERT INTO scores
                (id, uid, track_id, venue_id, name)
             VALUES ('score', 'owner', 'track', 'venue', NULL);",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("owner"))
            .await
            .unwrap();
        (directory, pool)
    }

    fn execution_service() -> PythonWorkspaceService {
        PythonWorkspaceService::new(
            std::env::temp_dir().join(format!("luma-cell-lease-{}", uuid::Uuid::new_v4())),
            Arc::new(|| Err("the lease test never launches Python".into())),
        )
    }

    #[test]
    fn a_late_cancel_cannot_hit_the_next_cell() {
        let service = execution_service();
        let thread = format!("t-{}", uuid::Uuid::new_v4());
        let first = service.claim_cell(&thread).unwrap();
        let cancel = first.cancel_token();
        // A second cell for the same thread is refused while the first holds
        // the slot.
        assert!(service.claim_cell(&thread).is_err());

        assert!(cancel_python_cell_inner(&service, &thread));
        assert!(cancel.is_cancelled());
        drop(first);

        // The cancel that chased the finished cell finds nothing, and the next
        // cell gets a fresh token.
        assert!(!cancel_python_cell_inner(&service, &thread));
        let next = service.claim_cell(&thread).unwrap();
        let next_cancel = next.cancel_token();
        assert!(!next_cancel.is_cancelled());
        assert!(cancel_python_cell_inner(&service, &thread));
        drop(next);
    }

    #[test]
    fn a_cell_claim_is_released_when_setup_exits_early() {
        let service = execution_service();
        let thread = format!("t-{}", uuid::Uuid::new_v4());
        {
            let _claim = service.claim_cell(&thread).unwrap();
            assert!(cancel_python_cell_inner(&service, &thread));
        }
        assert!(!cancel_python_cell_inner(&service, &thread));
        let next = service.claim_cell(&thread).unwrap();
        drop(next);
    }

    #[tokio::test]
    async fn track_scope_is_derived_from_the_thread_and_auth() {
        let (_directory, pool) = scope_pool().await;
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
        let (_directory, pool) = scope_pool().await;
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
