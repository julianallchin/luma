//! `run_python_cell` — the service that joins every piece of the executor.
//!
//! One call: resolve the thread, assemble a fresh binding revision from the
//! current scope, install it in the thread's
//! kernel, run the code, and project the outcome onto the notebook-native
//! [`PythonCellResult`] (design §15).
//!
//! Everything here takes plain handles, so the dispatch handler, the headless
//! harness and the integration tests drive exactly the same code path.

#[cfg(test)]
mod cell_tests;

use std::path::Path;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::agent::transcript::Role;
use crate::agent_execution::artifacts::{ArtifactEncoding, ArtifactKind};
use crate::agent_execution::bindings::providers::{assemble_bindings, BindingScope};
use crate::agent_execution::perform_host::PerformHost;
use crate::agent_execution::track_host::TrackHost;
use crate::agent_execution::track_host::{TrackEditScope, TrackScope};
use crate::agent_execution::venue_host::VenueHost;
use crate::agent_execution::worker_process::{CancelToken, ExecStatus, HostOperationScope};
use crate::agent_execution::workspace::{
    CellOutcome, PythonWorkspaceService, Workspace, DEFAULT_CELL_TIMEOUT,
};
use crate::agent_execution::CellHost;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::models::agent_execution::{PythonCellFigure, PythonCellResult, PythonScopeInput};
use crate::models::agent_threads::{AgentThread, ThreadRoute};
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

/// What is live in this app that a cell may play, bind or read: the Perform
/// session, the input hub and the control banks. Plain handles, like the
/// pool beside them.
pub struct LiveHandles {
    pub(crate) perform: Arc<crate::perform::PerformSession>,
    pub(crate) inputs: Arc<crate::inputs::InputHub>,
    pub(crate) controls: Arc<crate::controls::ControlBanks>,
}

/// Interrupt the thread's in-flight cell. `false` when there is none.
pub fn cancel_python_cell_inner(service: &PythonWorkspaceService, thread_id: &str) -> bool {
    service.cancel_cell(thread_id)
}

/// Which kernel a call addresses: the durable thread's own, or one detached
/// child workspace's.
///
/// The two child ids are a single fact spelled twice on the wire, so they are
/// checked as a pair — both or neither, and equal. Authorizing the workspace is
/// the caller's next step; this only resolves the id.
pub fn resolve_execution_id(
    thread_id: &str,
    execution_id: Option<String>,
    draft_id: Option<&str>,
) -> Result<String, String> {
    match (execution_id, draft_id) {
        (None, None) => Ok(thread_id.to_string()),
        (Some(execution_id), Some(workspace_id)) if execution_id == workspace_id => {
            Ok(execution_id)
        }
        (Some(_), Some(_)) => Err("child Python execution id must match its draft id".into()),
        _ => Err("child Python execution requires both an execution id and a draft id".into()),
    }
}

struct ResolvedScope {
    bindings: BindingScope,
    /// Present for every durable track thread — the route guarantees track,
    /// venue and score together. It permits high-fidelity read-only candidate
    /// rendering, never mutation.
    track: Option<TrackScope>,
    /// Present only when the durable thread scope pins the score and the
    /// venue's admission lets the principal edit it — anyone with access to
    /// the venue, whoever wrote the score. Python never supplies any field of
    /// this capability.
    track_edit: Option<TrackEditScope>,
    /// The venue whose Perform set the cell may read and change: a Perform
    /// thread's, and no other thread's.
    perform: Option<String>,
}

async fn resolve_scope(
    pool: &SqlitePool,
    thread: &AgentThread,
    requested: PythonScopeInput,
) -> Result<ResolvedScope, String> {
    match thread.route()? {
        ThreadRoute::Unbound => Ok(ResolvedScope {
            bindings: BindingScope {
                agent_kind: "venue_rig".into(),
                track_id: None,
                venue_id: None,
                score_id: None,
                track_editable: false,
                track_document: None,
                window: None,
            },
            track: None,
            track_edit: None,
            perform: None,
        }),
        ThreadRoute::Venue { venue_id } | ThreadRoute::Perform { venue_id } => {
            assert_pinned("venue", requested.venue_id.as_deref(), Some(venue_id))?;
            assert_pinned("track", requested.track_id.as_deref(), None)?;
            assert_pinned("score", requested.score_id.as_deref(), None)?;
            Ok(ResolvedScope {
                bindings: BindingScope {
                    agent_kind: thread.agent_kind.clone(),
                    track_id: None,
                    venue_id: Some(venue_id.to_string()),
                    score_id: None,
                    track_editable: false,
                    track_document: None,
                    window: requested.window,
                },
                track: None,
                track_edit: None,
                perform: matches!(thread.route()?, ThreadRoute::Perform { .. })
                    .then(|| venue_id.to_string()),
            })
        }
        ThreadRoute::Track {
            track_id,
            venue_id,
            score_id,
        } => {
            if crate::database::local::tracks::get_track_by_id(pool, track_id)
                .await?
                .is_none()
            {
                return Err("pinned track is not available".into());
            }
            assert_pinned("track", requested.track_id.as_deref(), Some(track_id))?;
            assert_pinned("venue", requested.venue_id.as_deref(), Some(venue_id))?;
            assert_pinned("score", requested.score_id.as_deref(), Some(score_id))?;

            let mut access = VenueAccess::<Read>::read(pool, VenueResource::Score(score_id))
                .await
                .map_err(|error| format!("pinned score '{score_id}' is not available: {error}"))?;
            access.require_venue(venue_id)?;
            let score = crate::database::local::scores::get_score(&mut access, score_id)
                .await
                .map_err(|error| format!("pinned score '{score_id}' is not available: {error}"))?;
            if score.track_id != track_id || score.venue_id != venue_id {
                return Err(format!(
                    "pinned score '{score_id}' does not belong to track '{track_id}' and venue '{venue_id}'"
                ));
            }
            let track_edit = score.editable.then(|| TrackEditScope {
                score_id: score_id.to_string(),
                track_id: track_id.to_string(),
                venue_id: venue_id.to_string(),
            });

            Ok(ResolvedScope {
                bindings: BindingScope {
                    agent_kind: thread.agent_kind.clone(),
                    track_id: Some(track_id.to_string()),
                    venue_id: Some(venue_id.to_string()),
                    score_id: Some(score_id.to_string()),
                    track_editable: track_edit.is_some(),
                    track_document: None,
                    window: requested.window,
                },
                track: Some(TrackScope {
                    score_id: score_id.to_string(),
                    track_id: track_id.to_string(),
                    venue_id: venue_id.to_string(),
                }),
                track_edit,
                perform: None,
            })
        }
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

/// Execute against either the durable parent namespace or one authenticated
/// detached child workspace. The durable thread remains the authorization
/// principal; execution/workspace ids only select isolated ephemeral and
/// detached state.
///
/// `waiting` is told `true` while the cell waits for Python to be ready — the
/// host's setup, minutes on a first launch, or a cold kernel importing the
/// analysis stack — and `false` once it stops waiting, so a host can say so
/// rather than claim the cell is running.
#[allow(clippy::too_many_arguments)]
pub async fn run_python_cell_inner(
    pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    service: &PythonWorkspaceService,
    live: &LiveHandles,
    thread_id: String,
    code: String,
    requested_scope: PythonScopeInput,
    turn_message_id: Option<String>,
    current_user_id: &str,
    execution_id: Option<String>,
    draft_id: Option<String>,
    waiting: &(dyn Fn(bool) + Sync),
) -> Result<PythonCellResult, String> {
    let execution_id = resolve_execution_id(&thread_id, execution_id, draft_id.as_deref())?;
    // The lifecycle lease begins before any database or binding work. Deletion
    // closes this admission gate, cancels us, and drains the guard before it can
    // remove the Python workspace.
    let lease = service.claim_cell(&execution_id)?;
    let cancel = lease.cancel_token();

    let thread =
        crate::database::local::agent_threads::get_thread(pool, &thread_id, current_user_id)
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
            // A cell attaches to a turn some principal opened. Which
            // principal is the thread's `actor`; that the turn is *durable*
            // is what makes the cell attributable and gives it a stable
            // operation namespace. A client that runs cells without speaking
            // opens a session turn and is no less a principal for it.
            match Role::parse(&message.role) {
                Some(Role::User | Role::Session) => {}
                _ => {
                    return Err("Python turn message must be a durable user or session turn".into())
                }
            }
            Some(host_operation_scope(
                &thread_id,
                &execution_id,
                turn_message_id,
            )?)
        }
        None => None,
    };
    let thread = crate::agent::context::execution_thread(pool, &thread_id, current_user_id).await?;

    let mut resolved = resolve_scope(pool, &thread, requested_scope).await?;
    if let Some(draft) = draft_id.as_deref() {
        let mut connection = pool
            .acquire()
            .await
            .map_err(|error| format!("failed to read the thread's draft: {error}"))?;
        resolved.bindings.track_document =
            Some(crate::services::drafts::state(&mut connection, draft).await?);
    }
    if resolved.track_edit.is_some() && operation_scope.is_none() {
        return Err("editable Python cells require a durable turn message".into());
    }
    let scope = resolved.bindings;
    // Waits for the host's Python setup, which a stop can cut short.
    let setup_running = service.running_setup().is_some();
    if setup_running {
        waiting(true);
    }
    let workspace = service.workspace_for_cell(&lease).await;
    if setup_running {
        waiting(false);
    }
    let workspace = workspace?;
    // Stopping a turn drops this future, but not the blocking tasks below.
    // Interrupt the kernel then, so neither a start nor a cell runs on unseen.
    let interrupt = CancelOnDrop(Some(cancel.clone()));

    // A cold kernel imports the analysis stack: seconds idle, minutes beside a
    // stem separation. Start it on its own so the host can say Python is
    // starting, rather than fail the cell or claim it is running.
    if !workspace.is_kernel_alive() {
        waiting(true);
        let starting = Arc::clone(&workspace);
        let token = cancel.clone();
        let started = tokio::task::spawn_blocking(move || starting.start_kernel(&token)).await;
        waiting(false);
        match started {
            Ok(Ok(())) => {}
            Ok(Err(outcome)) => return Ok(project(&workspace, outcome)),
            Err(e) => return Err(format!("the python kernel start task failed: {e}")),
        }
    }
    if cancel.is_cancelled() {
        // A stopped cell has no use for bindings, and assembling them can
        // decode a song's audio while a sign-out waits for this cell. The
        // workspace reports a cancelled cell interrupted without touching
        // Python.
        let outcome = workspace.run_cell(&code, DEFAULT_CELL_TIMEOUT, &cancel);
        return Ok(project(&workspace, outcome));
    }

    // Assembly is async (it reads the database) and holds the workspace's
    // artifact store, which is why that lock is a tokio one.
    let manifest = {
        let store = workspace.store();
        let mut store = store.lock().await;
        assemble_bindings(pool, storage, resource_root, &scope, &mut store).await?
    };

    let workspace_for_cell = Arc::clone(&workspace);
    let edit_scope = resolved.track_edit;
    // What this cell writes is the thread's actor's doing, in this
    // conversation, on the turn that ran it. Every cell of the turn names the
    // same message, so history reads the whole turn as one group of changes.
    let mut attribution = crate::sync::triggers::TurnAttribution::turn(
        thread.actor.as_deref(),
        &thread_id,
        turn_message_id.as_deref(),
    );
    // A subagent's writes are part of what the person asked for.
    if thread.parent_thread_id.is_some() {
        attribution.group = crate::agent::subagent::person_turn(pool, &thread_id)
            .await?
            .or(attribution.group);
    }
    // A room is renderable whenever one is in scope, whether or not this thread
    // may edit a track in it — a graph agent working against a venue still gets
    // to look at it. When a score *is* pinned, that score is what lights it.
    let track = resolved.track.map(|track_scope| {
        Arc::new(TrackHost::new(
            tokio::runtime::Handle::current(),
            pool.clone(),
            storage.clone(),
            resource_root.to_path_buf(),
            Arc::clone(&live.controls),
            Arc::clone(&workspace),
            track_scope,
            edit_scope,
            draft_id.clone(),
            attribution.clone(),
        ))
    });
    let venue = scope.venue_id.clone().map(|venue_id| {
        VenueHost::new(
            tokio::runtime::Handle::current(),
            pool.clone(),
            resource_root.to_path_buf(),
            Arc::clone(&workspace),
            venue_id,
            attribution.clone(),
            track.clone(),
        )
    });
    let perform = resolved.perform.map(|venue_id| {
        PerformHost::new(
            tokio::runtime::Handle::current(),
            pool.clone(),
            resource_root.to_path_buf(),
            venue_id,
            attribution,
            Arc::clone(&live.perform),
            Arc::clone(&live.inputs),
        )
    });
    let host = CellHost::new(track, venue, perform);
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
    interrupt.disarm();

    executed.map_err(|e| format!("the python cell task failed: {e}"))?
}

/// Cancels a cell whose awaiting future is dropped before the cell finishes.
struct CancelOnDrop(Option<CancelToken>);

impl CancelOnDrop {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel();
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned_track_thread() -> AgentThread {
        AgentThread {
            id: "thread".into(),
            owner_user_id: "owner".into(),
            agent_kind: "track_copilot".into(),
            subject_kind: Some("track".into()),
            subject_id: Some("track".into()),
            venue_id: Some("venue".into()),
            score_id: Some("score".into()),
            forked_from_thread_id: None,
            forked_at_message_id: None,
            parent_thread_id: None,
            parent_call_id: None,
            title: None,
            actor: None,
            engine: "api".into(),
            model: Some(crate::agent::model::DEFAULT_MODEL.into()),
            provider: Some("vercel-ai-gateway".into()),
            effort: None,
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

    /// A windowless library with no Python and one unbound agent thread,
    /// whose cells wait for the setup `state` reports, of a cache with nothing
    /// installed in it.
    async fn waiting_on_setup(
        directory: &Path,
        state: tokio::sync::watch::Receiver<crate::runtime::python::PythonSetup>,
    ) -> (crate::dispatch::AppServices, String) {
        let services = crate::test_support::signed_in(directory, directory.to_path_buf()).await;
        let thread = crate::database::local::agent_threads::create_thread(
            &services.db.0,
            crate::models::agent_threads::CreateAgentThreadInput {
                request_id: uuid::Uuid::new_v4().to_string(),
                agent_kind: "unbound".into(),
                ..Default::default()
            },
            crate::test_support::TEST_OWNER,
        )
        .await
        .unwrap();
        services
            .workspaces
            .attach_setup(crate::runtime::python::BackgroundSetup::new(
                directory.join("cache"),
                state,
            ));
        (services, thread.id)
    }

    async fn run(
        services: &crate::dispatch::AppServices,
        directory: &Path,
        thread: &str,
        waiting: &(dyn Fn(bool) + Sync),
    ) -> Result<PythonCellResult, String> {
        run_python_cell_inner(
            &services.db.0,
            &services.storage,
            directory,
            &services.workspaces,
            &services.live_handles(),
            thread.to_string(),
            "1".to_string(),
            PythonScopeInput::default(),
            None,
            crate::test_support::TEST_OWNER,
            None,
            None,
            waiting,
        )
        .await
    }

    #[tokio::test]
    async fn a_cell_on_a_host_whose_first_python_setup_failed_reports_the_step() {
        let directory = tempfile::tempdir().unwrap();
        let (_setup, state) =
            tokio::sync::watch::channel(crate::runtime::python::PythonSetup::failed(
                "Command to install PyTorch failed.\nstdout: Collecting torch",
            ));
        let (services, thread) = waiting_on_setup(directory.path(), state).await;

        let error = run(&services, directory.path(), &thread, &|_| {})
            .await
            .expect_err("there is no Python to run the cell on");
        assert!(
            error.contains("Command to install PyTorch failed."),
            "the cell should say which setup step failed: {error}"
        );
    }

    #[tokio::test]
    async fn a_cell_stopped_while_it_waits_for_python_setup_comes_back_interrupted() {
        let directory = tempfile::tempdir().unwrap();
        // A setup that never finishes.
        let (_setup, state) =
            tokio::sync::watch::channel(crate::runtime::python::PythonSetup::Running);
        let (services, thread) = waiting_on_setup(directory.path(), state).await;

        let stop = async {
            // A cell can be stopped from the moment it is admitted.
            while !cancel_python_cell_inner(&services.workspaces, &thread) {
                tokio::task::yield_now().await;
            }
        };
        let (cell, ()) = tokio::join!(run(&services, directory.path(), &thread, &|_| {}), stop);
        let cell = cell.expect("a stopped cell is a result");
        // This host has no Python, so a cell that went on to start it would
        // come back failed.
        assert_eq!(cell.status, "interrupted", "{cell:?}");
    }

    #[tokio::test]
    async fn a_cell_says_it_is_waiting_while_python_setup_runs() {
        let directory = tempfile::tempdir().unwrap();
        let (setup, state) =
            tokio::sync::watch::channel(crate::runtime::python::PythonSetup::Running);
        let (services, thread) = waiting_on_setup(directory.path(), state).await;
        let told = std::sync::Mutex::new(Vec::new());
        let waiting = |waiting: bool| told.lock().unwrap().push(waiting);

        let finish_setup = async {
            while told.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
            // Nothing is installed in this cache, so the cell ends with the
            // failed step instead of starting Python.
            setup.send_replace(crate::runtime::python::PythonSetup::failed("setup failed"));
        };
        let (cell, ()) = tokio::join!(
            run(&services, directory.path(), &thread, &waiting),
            finish_setup
        );
        assert!(cell.is_err(), "{cell:?}");
        assert_eq!(*told.lock().unwrap(), [true, false]);
    }

    #[tokio::test]
    async fn track_scope_is_derived_from_the_thread_and_auth() {
        let (_directory, pool) = scope_pool().await;
        let thread = pinned_track_thread();
        let resolved = resolve_scope(&pool, &thread, PythonScopeInput::default())
            .await
            .unwrap();
        assert_eq!(resolved.bindings.track_id.as_deref(), Some("track"));
        assert_eq!(resolved.bindings.venue_id.as_deref(), Some("venue"));
        assert_eq!(resolved.bindings.score_id.as_deref(), Some("score"));
        assert!(resolved.bindings.track_editable);
        assert_eq!(resolved.track_edit.unwrap().score_id, "score");

        crate::database::local::auth::arm_write_admission(&pool, Some("outsider"))
            .await
            .unwrap();
        let refused = resolve_scope(&pool, &thread, PythonScopeInput::default())
            .await
            .err()
            .expect("an account without the venue reached its score");
        assert!(refused.contains("is not available"), "{refused}");
    }

    /// A score is its author's: a member's agent reads a score the owner
    /// wrote and may not write it, as the member may not by hand.
    #[tokio::test]
    async fn a_member_s_track_thread_reads_another_person_s_score() {
        let (_directory, pool) = scope_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("member"))
            .await
            .unwrap();
        let mut download = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut download)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO venue_members (id, uid, venue_id, role)
             VALUES ('venue:member', 'member', 'venue', 'member')",
        )
        .execute(&mut *download)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut download)
            .await
            .unwrap();
        download.commit().await.unwrap();

        let resolved = resolve_scope(&pool, &pinned_track_thread(), PythonScopeInput::default())
            .await
            .unwrap();
        assert_eq!(
            resolved.track.map(|track| track.score_id).as_deref(),
            Some("score"),
            "the owner's score did not open for a member's agent"
        );
        assert!(!resolved.bindings.track_editable);
        assert!(
            resolved.track_edit.is_none(),
            "a member's agent may write the owner's score"
        );
    }

    #[tokio::test]
    async fn incoming_ids_cannot_retarget_a_durable_track_thread() {
        let (_directory, pool) = scope_pool().await;
        let requested = PythonScopeInput {
            track_id: Some("another-track".into()),
            ..Default::default()
        };
        let error = resolve_scope(&pool, &pinned_track_thread(), requested)
            .await
            .err()
            .unwrap();
        assert!(error.contains("does not match the durable agent thread"));
    }
}
