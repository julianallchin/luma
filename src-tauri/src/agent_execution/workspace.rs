//! One workspace per agent thread; one kernel per workspace (design §13).
//!
//! The registry is deliberately small: a map from thread id to a lazily created
//! [`Workspace`], each of which owns a directory, an [`ArtifactStore`] and at
//! most one live kernel. Different threads run concurrently; one thread's cells
//! are strictly serialized, because they share a namespace.
//!
//! No Tauri types live here — `tauri_env::resolve_worker_env` builds the
//! [`WorkerEnv`] and the service is registered as managed state around it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::agent_execution::artifacts::ArtifactStore;
use crate::agent_execution::bindings::manifest::BindingManifest;
use crate::agent_execution::worker_launcher::WorkerLauncher;
use crate::agent_execution::worker_process::{
    CancelToken, ExecOutcome, ExecStatus, FigureRef, HostCallHandler, HostOperationScope,
    Truncation, WorkerConfig, WorkerHandle,
};

/// The default wall-clock ceiling for one cell (design §16.3).
pub const DEFAULT_CELL_TIMEOUT: Duration = Duration::from_secs(90);

const LOSS_NOTICE: &str =
    "The Python kernel was restarted; variables and imports from earlier cells are gone. \
     Re-create anything you still need.";

type LauncherFactory = Arc<dyn Fn() -> Result<Box<dyn WorkerLauncher>, String> + Send + Sync>;
type EnvResolver = Arc<dyn Fn() -> Result<WorkerEnv, String> + Send + Sync>;

/// Where the interpreter and the worker script live, and how to sandbox them.
#[derive(Clone)]
pub struct WorkerEnv {
    pub python_bin: PathBuf,
    pub worker_script: PathBuf,
    pub launcher_factory: LauncherFactory,
}

impl WorkerEnv {
    pub fn new(
        python_bin: PathBuf,
        worker_script: PathBuf,
        launcher_factory: LauncherFactory,
    ) -> Self {
        Self {
            python_bin,
            worker_script,
            launcher_factory,
        }
    }
}

impl std::fmt::Debug for WorkerEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerEnv")
            .field("python_bin", &self.python_bin)
            .field("worker_script", &self.worker_script)
            .finish_non_exhaustive()
    }
}

/// Resolves the worker environment once, on first use.
///
/// Startup must not block on `ensure_python_env` (venv creation can take
/// minutes on a cold machine, and `setup_python_env_background` is already
/// warming it), so the service is registered eagerly and the environment is
/// resolved the first time a cell actually runs.
struct EnvCell {
    resolve: EnvResolver,
    cached: Mutex<Option<WorkerEnv>>,
}

impl EnvCell {
    fn get(&self) -> Result<WorkerEnv, String> {
        let mut cached = self.cached.lock().unwrap();
        if let Some(env) = cached.as_ref() {
            return Ok(env.clone());
        }
        let env = (self.resolve)()?;
        *cached = Some(env.clone());
        Ok(env)
    }
}

// ---------------------------------------------------------------------------
// Cell outcome
// ---------------------------------------------------------------------------

/// One cell's result as the command layer sees it: the worker's outcome plus
/// workspace-level kernel-restart facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellOutcome {
    pub status: ExecStatus,
    pub stdout: String,
    pub stderr: String,
    pub repr: Option<String>,
    pub traceback: Option<String>,
    pub figures: Vec<FigureRef>,
    pub warnings: Vec<String>,
    pub truncated: Truncation,
    pub duration_ms: u64,
    /// Human-readable things the agent must be told about state loss.
    pub notices: Vec<String>,
    /// Which kernel incarnation ran this cell.
    pub kernel_generation: u64,
}

impl CellOutcome {
    fn from_exec(outcome: ExecOutcome, notices: Vec<String>, kernel_generation: u64) -> Self {
        Self {
            status: outcome.status,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            repr: outcome.repr,
            traceback: outcome.traceback,
            figures: outcome.figures,
            warnings: outcome.warnings,
            truncated: outcome.truncated,
            duration_ms: outcome.duration_ms,
            notices,
            kernel_generation,
        }
    }

    fn infra_failure(reason: String, notices: Vec<String>, kernel_generation: u64) -> Self {
        Self {
            status: ExecStatus::Failed { reason },
            stdout: String::new(),
            stderr: String::new(),
            repr: None,
            traceback: None,
            figures: Vec::new(),
            warnings: Vec::new(),
            truncated: Truncation::default(),
            duration_ms: 0,
            notices,
            kernel_generation,
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

#[derive(Default)]
struct KernelState {
    handle: Option<Arc<WorkerHandle>>,
    generation: u64,
    /// The manifest currently installed in the *live* kernel.
    installed_rel: Option<String>,
    /// The latest revision written to disk, waiting to be installed.
    pending_rel: Option<String>,
    /// Notices owed to the next cell (a crash between turns has no result to
    /// attach itself to).
    pending_notices: Vec<String>,
}

pub struct Workspace {
    thread_id: String,
    dir: PathBuf,
    env: Arc<EnvCell>,
    store: Arc<AsyncMutex<ArtifactStore>>,
    kernel: Mutex<KernelState>,
}

impl Workspace {
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Borrow the artifact store from a **blocking** context. Held on its own
    /// lock so a provider can stage the next revision's inputs while a cell is
    /// still running.
    ///
    /// Panics inside an async task — binding assembly is async (it reads the
    /// database), so the lock is a `tokio` mutex and async callers must go
    /// through [`Workspace::store`] instead.
    pub fn with_store<R>(&self, f: impl FnOnce(&mut ArtifactStore) -> R) -> R {
        let mut store = self.store.blocking_lock();
        f(&mut store)
    }

    /// The artifact store for async callers: binding assembly holds it across
    /// `await`s while it reads the database.
    pub fn store(&self) -> Arc<AsyncMutex<ArtifactStore>> {
        Arc::clone(&self.store)
    }

    /// Write a new binding revision into `inputs/` and queue it for the next
    /// cell. Manifests are never mutated in place — the worker memoizes by
    /// revision *and* by path (appendix A.5).
    pub fn install_revision(&self, manifest: &BindingManifest) -> Result<String, String> {
        let rel = self.with_store(|store| store.write_manifest(manifest))?;
        self.kernel.lock().unwrap().pending_rel = Some(rel.clone());
        Ok(rel)
    }

    /// The revision the live kernel is running, if any.
    pub fn installed_revision(&self) -> Option<String> {
        self.kernel.lock().unwrap().installed_rel.clone()
    }

    pub fn kernel_generation(&self) -> u64 {
        self.kernel.lock().unwrap().generation
    }

    pub fn is_kernel_alive(&self) -> bool {
        self.kernel
            .lock()
            .unwrap()
            .handle
            .as_ref()
            .is_some_and(|h| h.is_alive())
    }

    /// Round-trip a `ping` against the live kernel. `None` when there is none.
    pub fn ping(&self, timeout: Duration) -> Option<Result<i32, String>> {
        let handle = self.kernel.lock().unwrap().handle.clone()?;
        Some(handle.ping(timeout))
    }

    /// Run one cell. Spawns the kernel on first use, re-installs the binding
    /// revision when it changed, and converts a dead kernel into a state-loss
    /// notice for the next cell.
    pub fn run_cell(&self, code: &str, timeout: Duration, cancel: &CancelToken) -> CellOutcome {
        self.run_cell_inner(code, timeout, cancel, None, None)
    }

    /// As [`Workspace::run_cell`], with one scoped host capability table for
    /// this cell. The handler is deliberately not retained by the workspace:
    /// scope and authorization must be resolved afresh by the command layer.
    pub fn run_cell_with_host(
        &self,
        code: &str,
        timeout: Duration,
        cancel: &CancelToken,
        host: &dyn HostCallHandler,
        operation_scope: Option<&HostOperationScope>,
    ) -> CellOutcome {
        self.run_cell_inner(code, timeout, cancel, Some(host), operation_scope)
    }

    fn run_cell_inner(
        &self,
        code: &str,
        timeout: Duration,
        cancel: &CancelToken,
        host: Option<&dyn HostCallHandler>,
        operation_scope: Option<&HostOperationScope>,
    ) -> CellOutcome {
        let mut state = self.kernel.lock().unwrap();
        let mut notices = std::mem::take(&mut state.pending_notices);

        // Stop can arrive while the command is still assembling bindings. Do
        // not launch or touch Python for a cell that was already cancelled.
        if cancel.is_cancelled() {
            let generation = state.generation;
            return CellOutcome::from_exec(
                ExecOutcome::interrupted_before_start(std::time::Instant::now()),
                notices,
                generation,
            );
        }

        // A kernel that died between cells is indistinguishable from no kernel.
        if state.handle.as_ref().is_some_and(|h| !h.is_alive()) {
            state.handle = None;
            state.installed_rel = None;
            notices.push(LOSS_NOTICE.to_string());
        }

        if state.handle.is_none() {
            match self.spawn_kernel() {
                Ok(handle) => {
                    state.generation += 1;
                    state.installed_rel = None;
                    let startup = handle.ready();
                    for warning in &startup.warnings {
                        notices.push(format!("Python worker warning: {warning}"));
                    }
                    state.handle = Some(Arc::new(handle));
                }
                Err(e) => {
                    let generation = state.generation;
                    return CellOutcome::infra_failure(e, notices, generation);
                }
            }
        }

        // Kernel startup can be slow on a cold thread. A cancellation requested
        // during startup still prevents the cell from entering user code while
        // preserving the freshly started namespace for the next turn.
        if cancel.is_cancelled() {
            let generation = state.generation;
            return CellOutcome::from_exec(
                ExecOutcome::interrupted_before_start(std::time::Instant::now()),
                notices,
                generation,
            );
        }

        let handle = state.handle.clone().expect("kernel spawned above");
        let manifest_rel = match (&state.pending_rel, &state.installed_rel) {
            (Some(pending), installed) if Some(pending) != installed.as_ref() => {
                Some(pending.clone())
            }
            _ => None,
        };

        let id = handle.next_id();
        let outcome = match host {
            Some(host) => handle.exec_with_host(
                &id,
                code,
                manifest_rel.as_deref(),
                timeout,
                cancel,
                host,
                operation_scope,
            ),
            None => handle.exec(&id, code, manifest_rel.as_deref(), timeout, cancel),
        };

        if outcome.state_lost {
            state.handle = None;
            state.installed_rel = None;
            state.pending_notices.push(LOSS_NOTICE.to_string());
        } else if manifest_rel.is_some() && !matches!(outcome.status, ExecStatus::Failed { .. }) {
            state.installed_rel = manifest_rel;
        }

        let generation = state.generation;
        CellOutcome::from_exec(outcome, notices, generation)
    }

    /// Stop the kernel and leave the directory alone.
    pub fn shutdown(&self) {
        let mut state = self.kernel.lock().unwrap();
        if let Some(handle) = state.handle.take() {
            handle.shutdown();
        }
        state.installed_rel = None;
    }

    fn spawn_kernel(&self) -> Result<WorkerHandle, String> {
        let env = self.env.get()?;
        let launcher = (env.launcher_factory)()?;
        WorkerHandle::spawn(WorkerConfig {
            python_bin: env.python_bin,
            worker_script: env.worker_script,
            workspace_dir: self.dir.clone(),
            launcher,
        })
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

struct ActiveCell {
    id: u64,
    cancel: CancelToken,
}

#[derive(Default)]
struct ThreadExecutionState {
    admission_closed: bool,
    active: Option<ActiveCell>,
}

#[derive(Default)]
struct ThreadExecutionGate {
    state: Mutex<ThreadExecutionState>,
    drained: Notify,
}

/// A cell owns this lease from command admission through every async setup step
/// and the blocking kernel call. Deletion closes the same gate, cancels the
/// token, and waits for this guard to drop before touching the workspace.
pub(crate) struct CellExecutionLease {
    thread_id: String,
    id: u64,
    cancel: CancelToken,
    gate: Arc<ThreadExecutionGate>,
}

impl CellExecutionLease {
    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Drop for CellExecutionLease {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        if state.active.as_ref().is_some_and(|cell| cell.id == self.id) {
            state.active = None;
            drop(state);
            self.gate.drained.notify_waiters();
        }
    }
}

pub struct PythonWorkspaceService {
    root: PathBuf,
    env: Arc<EnvCell>,
    workspaces: Mutex<HashMap<String, Arc<Workspace>>>,
    execution_gates: Mutex<HashMap<String, Arc<ThreadExecutionGate>>>,
    identity_admission: Mutex<bool>,
    next_cell_id: AtomicU64,
}

pub struct WorkspaceIdentityBarrier<'a> {
    service: &'a PythonWorkspaceService,
}

impl Drop for WorkspaceIdentityBarrier<'_> {
    fn drop(&mut self) {
        *self.service.identity_admission.lock().unwrap() = true;
    }
}

impl PythonWorkspaceService {
    /// The production form: the environment is resolved on first use.
    pub fn new(workspaces_root: PathBuf, resolve: EnvResolver) -> Self {
        Self {
            root: workspaces_root,
            env: Arc::new(EnvCell {
                resolve,
                cached: Mutex::new(None),
            }),
            workspaces: Mutex::new(HashMap::new()),
            execution_gates: Mutex::new(HashMap::new()),
            identity_admission: Mutex::new(true),
            next_cell_id: AtomicU64::new(1),
        }
    }

    /// The test/harness form: the environment is already known.
    pub fn with_env(workspaces_root: PathBuf, env: WorkerEnv) -> Self {
        Self::new(workspaces_root, Arc::new(move || Ok(env.clone())))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Admit one complete cell operation. A thread has at most one cell because
    /// its Python namespace is sequential, and a terminal deletion gate never
    /// reopens in this process.
    pub(crate) fn claim_cell(&self, thread_id: &str) -> Result<CellExecutionLease, String> {
        let identity_admission = self.identity_admission.lock().unwrap();
        if !*identity_admission {
            return Err("Python execution is paused for an authenticated identity switch".into());
        }
        let gate = self.execution_gate(thread_id)?;
        let mut state = gate.state.lock().unwrap();
        if state.admission_closed {
            return Err(format!(
                "agent thread '{thread_id}' is deleting; Python execution is closed"
            ));
        }
        if state.active.is_some() {
            return Err(format!(
                "a python cell is already running for agent thread '{thread_id}'"
            ));
        }
        let id = self.next_cell_id.fetch_add(1, Ordering::SeqCst);
        let cancel = CancelToken::new();
        state.active = Some(ActiveCell {
            id,
            cancel: cancel.clone(),
        });
        drop(state);
        drop(identity_admission);
        Ok(CellExecutionLease {
            thread_id: thread_id.to_string(),
            id,
            cancel,
            gate,
        })
    }

    /// Close global cell admission, cancel and drain every in-flight cell, and
    /// stop every live kernel before an account boundary crosses. Durable
    /// workspace directories survive; only process memory/capabilities are
    /// retired. Dropping the returned barrier reopens admission for the newly
    /// installed principal (or restores it after a failed switch).
    pub async fn suspend_for_identity_switch(&self) -> WorkspaceIdentityBarrier<'_> {
        let gates = {
            let mut admission = self.identity_admission.lock().unwrap();
            *admission = false;
            self.execution_gates
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>()
        };
        for gate in gates {
            loop {
                let notified = gate.drained.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let active = gate
                    .state
                    .lock()
                    .unwrap()
                    .active
                    .as_ref()
                    .map(|cell| cell.cancel.clone());
                let Some(cancel) = active else {
                    break;
                };
                cancel.cancel();
                notified.await;
            }
        }
        let workspaces = self
            .workspaces
            .lock()
            .unwrap()
            .drain()
            .map(|(_, workspace)| workspace)
            .collect::<Vec<_>>();
        for workspace in workspaces {
            workspace.shutdown();
        }
        WorkspaceIdentityBarrier { service: self }
    }

    /// Interrupt the admitted cell, including one that is still resolving its
    /// database scope or assembling bindings.
    pub(crate) fn cancel_cell(&self, thread_id: &str) -> bool {
        let gate = self.execution_gates.lock().unwrap().get(thread_id).cloned();
        let cancel = gate.and_then(|gate| {
            gate.state
                .lock()
                .unwrap()
                .active
                .as_ref()
                .map(|cell| cell.cancel.clone())
        });
        if let Some(cancel) = cancel {
            cancel.cancel();
            true
        } else {
            false
        }
    }

    /// Get (creating on first touch) the workspace owned by an admitted cell.
    /// Requiring the lease at this API boundary prevents future command paths
    /// from accidentally creating a workspace outside deletion's drain set.
    pub(crate) fn workspace_for_cell(
        &self,
        lease: &CellExecutionLease,
    ) -> Result<Arc<Workspace>, String> {
        let thread_id = &lease.thread_id;
        let gate = self.execution_gate(thread_id)?;
        // Hold admission across registry lookup and creation. Deletion either
        // closes first (and this fails) or observes/removes what we created.
        let execution = gate.state.lock().unwrap();
        if execution.admission_closed {
            return Err(format!(
                "agent thread '{thread_id}' is deleting; its Python workspace is retired"
            ));
        }
        if !execution
            .active
            .as_ref()
            .is_some_and(|cell| cell.id == lease.id)
        {
            return Err(format!(
                "python execution lease for agent thread '{thread_id}' is no longer active"
            ));
        }
        let mut workspaces = self.workspaces.lock().unwrap();
        if let Some(existing) = workspaces.get(thread_id) {
            return Ok(Arc::clone(existing));
        }
        let dir = self.thread_dir(thread_id)?;
        let store = ArtifactStore::open(&dir).map_err(String::from)?;
        // `open` canonicalizes; keep the store's view and ours identical so the
        // worker's `--workspace` matches the paths in the manifest.
        let dir = store.root().to_path_buf();
        let workspace = Arc::new(Workspace {
            thread_id: thread_id.to_string(),
            dir,
            env: Arc::clone(&self.env),
            store: Arc::new(AsyncMutex::new(store)),
            kernel: Mutex::new(KernelState::default()),
        });
        workspaces.insert(thread_id.to_string(), Arc::clone(&workspace));
        drop(execution);
        Ok(workspace)
    }

    #[cfg(test)]
    pub(crate) fn workspace_for_test(&self, thread_id: &str) -> Result<Arc<Workspace>, String> {
        let lease = self.claim_cell(thread_id)?;
        self.workspace_for_cell(&lease)
    }

    /// Close admission, cancel and drain the complete starting/running cell,
    /// then stop the kernel and remove everything the thread owned. The closed
    /// gate remains after failures so retryable deletion cannot resurrect it.
    pub async fn retire_thread(&self, thread_id: &str) -> Result<(), String> {
        let gate = self.execution_gate(thread_id)?;
        loop {
            let notified = gate.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let active = {
                let mut state = gate.state.lock().unwrap();
                state.admission_closed = true;
                state.active.as_ref().map(|cell| cell.cancel.clone())
            };
            let Some(cancel) = active else {
                break;
            };
            cancel.cancel();
            notified.await;
        }

        let existing = self.workspaces.lock().unwrap().remove(thread_id);
        if let Some(workspace) = &existing {
            workspace.shutdown();
        }
        let dir = self.thread_dir(thread_id)?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("failed to remove {}: {e}", dir.display())),
        }
    }

    /// Stop every kernel (app shutdown). Directories survive.
    pub fn shutdown_all(&self) {
        let workspaces: Vec<Arc<Workspace>> =
            self.workspaces.lock().unwrap().values().cloned().collect();
        for workspace in workspaces {
            workspace.shutdown();
        }
    }

    fn thread_dir(&self, thread_id: &str) -> Result<PathBuf, String> {
        if thread_id.is_empty()
            || thread_id.contains(['/', '\\'])
            || thread_id.starts_with('.')
            || thread_id.contains('\0')
        {
            return Err(format!("invalid agent thread id '{thread_id}'"));
        }
        Ok(self.root.join(thread_id))
    }

    fn execution_gate(&self, thread_id: &str) -> Result<Arc<ThreadExecutionGate>, String> {
        self.thread_dir(thread_id)?;
        let mut gates = self.execution_gates.lock().unwrap();
        Ok(Arc::clone(
            gates
                .entry(thread_id.to_string())
                .or_insert_with(|| Arc::new(ThreadExecutionGate::default())),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(root: PathBuf) -> PythonWorkspaceService {
        PythonWorkspaceService::new(
            root,
            Arc::new(|| Err("no python environment in this test".to_string())),
        )
    }

    #[test]
    fn workspaces_are_lazy_and_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let service = service(tmp.path().to_path_buf());
        let a = service.workspace_for_test("thread-a").unwrap();
        let again = service.workspace_for_test("thread-a").unwrap();
        let b = service.workspace_for_test("thread-b").unwrap();
        assert!(Arc::ptr_eq(&a, &again));
        assert_ne!(a.dir(), b.dir());
        for sub in ["inputs", "scratch", "outputs"] {
            assert!(a.dir().join(sub).is_dir());
        }
    }

    #[test]
    fn thread_ids_cannot_escape_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let service = service(tmp.path().to_path_buf());
        assert!(service.workspace_for_test("../etc").is_err());
        assert!(service.workspace_for_test("a/b").is_err());
        assert!(service.workspace_for_test("").is_err());
    }

    #[test]
    fn a_missing_python_env_is_an_infra_failure_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let service = service(tmp.path().to_path_buf());
        let workspace = service.workspace_for_test("thread-a").unwrap();
        let outcome = workspace.run_cell("1+1", Duration::from_secs(5), &CancelToken::new());
        match outcome.status {
            ExecStatus::Failed { reason } => assert!(reason.contains("no python environment")),
            other => panic!("expected an infra failure, got {other:?}"),
        }
        assert_eq!(outcome.kernel_generation, 0);
    }

    #[tokio::test]
    async fn deleting_a_thread_removes_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let service = service(tmp.path().to_path_buf());
        let dir = service
            .workspace_for_test("thread-a")
            .unwrap()
            .dir()
            .to_path_buf();
        assert!(dir.is_dir());
        service.retire_thread("thread-a").await.unwrap();
        assert!(!dir.exists());
        // Deleting twice is not an error.
        service.retire_thread("thread-a").await.unwrap();
    }

    #[tokio::test]
    async fn deletion_closes_a_paused_start_before_workspace_resurrection() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(service(tmp.path().to_path_buf()));
        let workspace_dir = service.root().join("thread-a");
        // This is the command's starting phase: admission succeeded, but scope
        // resolution/binding assembly has not reached workspace creation.
        let starting = service.claim_cell("thread-a").unwrap();
        let cancel = starting.cancel_token();

        let deleting_service = Arc::clone(&service);
        let deletion =
            tokio::spawn(async move { deleting_service.retire_thread("thread-a").await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancel.is_cancelled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("deletion closes admission and cancels the paused start");

        assert!(service.workspace_for_cell(&starting).is_err());
        assert!(!workspace_dir.exists());
        assert!(
            !deletion.is_finished(),
            "deletion must drain the starting cell"
        );
        drop(starting);
        deletion.await.unwrap().unwrap();

        assert!(!workspace_dir.exists());
        assert!(service.workspace_for_test("thread-a").is_err());
    }

    #[tokio::test]
    async fn aborting_an_awaiter_does_not_release_a_blocking_cell_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(service(tmp.path().to_path_buf()));
        let lease = service.claim_cell("thread-a").unwrap();
        let cancel = lease.cancel_token();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let waiter = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let _lease = lease;
                let _ = started_tx.send(());
                release_rx.recv().expect("release detached blocking cell");
            })
            .await
        });
        started_rx.await.expect("blocking cell started");
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());

        let deleting_service = Arc::clone(&service);
        let deletion =
            tokio::spawn(async move { deleting_service.retire_thread("thread-a").await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancel.is_cancelled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("deletion cancels detached blocking cell");
        assert!(
            !deletion.is_finished(),
            "the detached blocking task still owns execution admission"
        );

        release_tx.send(()).unwrap();
        deletion.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn identity_barrier_cancels_and_drains_cells_before_reopening_admission() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(service(tmp.path().to_path_buf()));
        let lease = service.claim_cell("thread-a").unwrap();
        let cancel = lease.cancel_token();
        let workspace_dir = service
            .workspace_for_cell(&lease)
            .unwrap()
            .dir()
            .to_path_buf();
        let (barrier_ready_tx, barrier_ready_rx) = tokio::sync::oneshot::channel();
        let (release_barrier_tx, release_barrier_rx) = tokio::sync::oneshot::channel();

        let switching_service = Arc::clone(&service);
        let switching = tokio::spawn(async move {
            let barrier = switching_service.suspend_for_identity_switch().await;
            barrier_ready_tx.send(()).unwrap();
            release_barrier_rx.await.unwrap();
            drop(barrier);
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancel.is_cancelled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("identity switch cancels the active cell");
        assert!(service.claim_cell("thread-b").is_err());
        assert!(!switching.is_finished(), "the active cell must drain first");

        drop(lease);
        barrier_ready_rx.await.unwrap();
        assert!(service.workspaces.lock().unwrap().is_empty());
        assert!(workspace_dir.is_dir(), "durable thread state survives");
        assert!(service.claim_cell("thread-b").is_err());

        release_barrier_tx.send(()).unwrap();
        switching.await.unwrap();
        assert!(service.claim_cell("thread-b").is_ok());
    }
}
