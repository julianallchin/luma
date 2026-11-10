//! Resolve the path-based Python worker environment shared by every host.
//!
//! Resolution stays cheap: the native desktop prepares the environment in the
//! background at startup, and a cell waits for that
//! ([`PythonWorkspaceService::workspace_for_cell`]), so resolving only locates
//! the interpreter and script.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::sandbox;
use super::worker_launcher::numba_cache_dir;
use super::worker_process::{CancelToken, WorkerConfig, WorkerHandle};
use super::workspace::{PythonWorkspaceService, WorkerEnv};
use crate::runtime::python as python_env;
use crate::storage::StorageRoot;

/// Locate the managed interpreter and the worker script under `cache_dir`.
///
/// The worker script comes from the repository when there is one — a developer
/// build must run the source it is editing, not the copy deployed at some
/// earlier launch — and falls back to that deployed copy otherwise.
///
/// # Errors
///
/// If no virtualenv exists yet (run the native app once to create it), or the
/// worker script is missing from both locations.
pub fn resolve_worker_env(cache_dir: &Path) -> Result<WorkerEnv, String> {
    let python_bin = python_env::find_existing_venv_python(cache_dir).ok_or_else(|| {
        format!(
            "no managed python environment under {} — run the app once to create it",
            cache_dir.display()
        )
    })?;

    let repo_script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("python")
        .join("luma_exec")
        .join("worker.py");
    let worker_script = if repo_script.exists() {
        repo_script
    } else {
        cache_dir.join("luma_exec").join("worker.py")
    };
    if !worker_script.exists() {
        return Err(format!(
            "agent python worker missing at {}",
            worker_script.display()
        ));
    }
    Ok(WorkerEnv::new(
        python_bin,
        worker_script,
        Arc::new(sandbox::default_launcher),
    )
    .with_numba_seed(numba_cache_dir(&kernel_cache_workspace(cache_dir))))
}

/// The workspace of the worker [`warm_kernel_cache`] starts: its numba cache
/// is the seed every thread's workspace is given.
fn kernel_cache_workspace(cache_dir: &Path) -> PathBuf {
    cache_dir.join("agent-kernel-cache")
}

/// Start one worker, sandboxed like any other, and stop it once it is ready,
/// so the numba kernels its preload compiles are cached where
/// [`WorkerEnv::numba_seed`] points. Only this worker writes there, and it runs
/// no cell, so what each workspace is seeded with comes from Luma's own code.
/// Takes seconds when the cache is already current.
///
/// # Errors
///
/// When the worker environment cannot be resolved or the worker fails to start.
pub fn warm_kernel_cache(cache_dir: &Path) -> Result<(), String> {
    let env = resolve_worker_env(cache_dir)?;
    let workspace_dir = kernel_cache_workspace(cache_dir);
    std::fs::create_dir_all(&workspace_dir)
        .map_err(|e| format!("cannot create {}: {e}", workspace_dir.display()))?;
    let worker = WorkerHandle::spawn(
        WorkerConfig {
            python_bin: env.python_bin,
            worker_script: env.worker_script,
            workspace_dir,
            launcher: (env.launcher_factory)()?,
        },
        &CancelToken::new(),
    )?;
    worker.shutdown();
    Ok(())
}

/// The managed workspace service for a headless host.
///
/// Construction cannot fail: resolution is deferred to the first cell, so a
/// machine with no Python yet still opens the app and still runs every tool
/// that is not Python.
#[must_use]
pub fn workspace_service(storage: &StorageRoot, cache_dir: PathBuf) -> PythonWorkspaceService {
    PythonWorkspaceService::new(
        storage.agent_workspaces_dir(),
        Arc::new(move || resolve_worker_env(&cache_dir)),
    )
}
