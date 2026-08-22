//! The worker environment for a host with no `AppHandle`.
//!
//! Path-based twin of [`super::tauri_env`], and the reason it exists: the venv
//! and the deployed `luma_exec` package are addressed by *path*, so a host that
//! is not Tauri can reach exactly the environment the Tauri app created. What
//! it deliberately does **not** do is create one — building the virtualenv is
//! minutes of work, and a host that started it lazily on the first tool call
//! would hang a turn instead of answering it.
//!
//! Every non-Tauri host resolves through here. A host that spelled its own
//! factory would be free to spell a stub, which is exactly how the GPUI app
//! came to declare "the GPUI app does not run Python workspaces" and fail every
//! tool call the agent made.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::sandbox;
use super::workspace::{PythonWorkspaceService, WorkerEnv};
use crate::python_env;
use crate::storage::StorageRoot;

/// The app cache directory: where the managed venv and the deployed
/// `luma_exec` package live.
///
/// The Tauri app derives this from its bundle identifier; every other host
/// reconstructs the same path, so they share one environment rather than each
/// building their own.
///
/// # Errors
///
/// If the platform has no cache directory.
pub fn cache_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("LUMA_CACHE_DIR") {
        return Ok(PathBuf::from(path));
    }
    dirs::cache_dir()
        .map(|path| path.join("com.luma.luma"))
        .ok_or_else(|| "could not locate a cache directory".to_string())
}

/// Locate the managed interpreter and the worker script under `cache_dir`.
///
/// The worker script comes from the repository when there is one — a developer
/// build must run the source it is editing, not the copy deployed at some
/// earlier launch — and falls back to that deployed copy otherwise.
///
/// # Errors
///
/// If no virtualenv exists yet (run the Tauri app once to create it), or the
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
