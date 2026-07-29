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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;

use crate::agent_execution::artifacts::{ArtifactStore, SCRATCH_DIR};
use crate::agent_execution::bindings::manifest::BindingManifest;
use crate::agent_execution::worker_launcher::WorkerLauncher;
use crate::agent_execution::worker_process::{
    CancelToken, ExecOutcome, ExecStatus, FigureRef, Truncation, WorkerConfig, WorkerHandle,
};

/// The default wall-clock ceiling for one cell (design §16.3).
pub const DEFAULT_CELL_TIMEOUT: Duration = Duration::from_secs(90);

const LOSS_NOTICE: &str =
    "The Python kernel was restarted; variables and imports from earlier cells are gone. \
     Re-create anything you still need.";
const RESET_NOTICE: &str = "The Python kernel was reset; the namespace and scratch are empty.";

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
/// the workspace-level facts (kernel restarts, reset notices).
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
    /// Human-readable things the agent must be told (state loss, resets).
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
        let mut state = self.kernel.lock().unwrap();
        let mut notices = std::mem::take(&mut state.pending_notices);

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

        let handle = state.handle.clone().expect("kernel spawned above");
        let manifest_rel = match (&state.pending_rel, &state.installed_rel) {
            (Some(pending), installed) if Some(pending) != installed.as_ref() => {
                Some(pending.clone())
            }
            _ => None,
        };

        let id = handle.next_id();
        let outcome = handle.exec(&id, code, manifest_rel.as_deref(), timeout, cancel);

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

    /// Replace the process (design §13.5): a cleared `globals()` is not a reset
    /// — agent code can mutate module state, matplotlib config and threads.
    /// Scratch is wiped; `inputs/` and `outputs/` survive for artifact leases.
    pub fn reset(&self) -> Result<(), String> {
        let mut state = self.kernel.lock().unwrap();
        if let Some(handle) = state.handle.take() {
            handle.shutdown();
        }
        state.installed_rel = None;
        state.pending_notices.push(RESET_NOTICE.to_string());
        drop(state);

        let scratch = self.dir.join(SCRATCH_DIR);
        if scratch.exists() {
            fs::remove_dir_all(&scratch)
                .map_err(|e| format!("failed to clear {}: {e}", scratch.display()))?;
        }
        fs::create_dir_all(&scratch)
            .map_err(|e| format!("failed to recreate {}: {e}", scratch.display()))
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

pub struct PythonWorkspaceService {
    root: PathBuf,
    env: Arc<EnvCell>,
    workspaces: Mutex<HashMap<String, Arc<Workspace>>>,
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
        }
    }

    /// The test/harness form: the environment is already known.
    pub fn with_env(workspaces_root: PathBuf, env: WorkerEnv) -> Self {
        Self::new(workspaces_root, Arc::new(move || Ok(env.clone())))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get (creating on first touch) the workspace for a thread.
    pub fn workspace_for(&self, thread_id: &str) -> Result<Arc<Workspace>, String> {
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
        Ok(workspace)
    }

    /// Thread deletion: stop the kernel and remove everything it owned.
    pub fn shutdown_thread(&self, thread_id: &str) -> Result<(), String> {
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
        let a = service.workspace_for("thread-a").unwrap();
        let again = service.workspace_for("thread-a").unwrap();
        let b = service.workspace_for("thread-b").unwrap();
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
        assert!(service.workspace_for("../etc").is_err());
        assert!(service.workspace_for("a/b").is_err());
        assert!(service.workspace_for("").is_err());
    }

    #[test]
    fn a_missing_python_env_is_an_infra_failure_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let service = service(tmp.path().to_path_buf());
        let workspace = service.workspace_for("thread-a").unwrap();
        let outcome = workspace.run_cell("1+1", Duration::from_secs(5), &CancelToken::new());
        match outcome.status {
            ExecStatus::Failed { reason } => assert!(reason.contains("no python environment")),
            other => panic!("expected an infra failure, got {other:?}"),
        }
        assert_eq!(outcome.kernel_generation, 0);
    }

    #[test]
    fn deleting_a_thread_removes_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let service = service(tmp.path().to_path_buf());
        let dir = service
            .workspace_for("thread-a")
            .unwrap()
            .dir()
            .to_path_buf();
        assert!(dir.is_dir());
        service.shutdown_thread("thread-a").unwrap();
        assert!(!dir.exists());
        // Deleting twice is not an error.
        service.shutdown_thread("thread-a").unwrap();
    }
}
