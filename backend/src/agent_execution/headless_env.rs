//! Resolve the path-based Python worker environment shared by every host.
//!
//! Resolution stays cheap: the native desktop prepares the environment in the
//! background at startup, and a cell waits for that
//! ([`PythonWorkspaceService::workspace_for_cell`]), so resolving only locates
//! the interpreter and script.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::sandbox;
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
    ))
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
