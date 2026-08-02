//! The only place the execution layer meets Tauri.
//!
//! Everything below this file takes plain paths, so the headless harness and
//! the integration tests can drive the same code without an `AppHandle`.

use std::sync::Arc;

use tauri::AppHandle;

use crate::agent_execution::sandbox;
use crate::agent_execution::workspace::{PythonWorkspaceService, WorkerEnv};
use crate::python_env;
use crate::storage::StorageRoot;

/// Deploy `luma_exec` into the app cache and locate the managed interpreter.
///
/// Both halves are memoized inside `python_env`, so calling this per kernel
/// spawn is cheap after the first time — but the *first* call can create the
/// virtualenv, which is why the service resolves it lazily.
pub fn resolve_worker_env(app: &AppHandle) -> Result<WorkerEnv, String> {
    let python_bin = python_env::ensure_python_env(app)?;
    let package_dir = python_env::ensure_python_resource_dir(app, "luma_exec")?;
    let worker_script = package_dir.join("worker.py");
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

/// Build the managed workspace service from the already-resolved durable
/// storage root. Construction cannot fail; only a first cell needs to resolve
/// the optional Python runtime.
pub fn workspace_service(app: &AppHandle, storage: &StorageRoot) -> PythonWorkspaceService {
    let handle = app.clone();
    PythonWorkspaceService::new(
        storage.agent_workspaces_dir(),
        Arc::new(move || resolve_worker_env(&handle)),
    )
}
