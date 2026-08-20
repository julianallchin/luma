//! Python cells: run one in an agent thread's kernel, or interrupt the one in
//! flight.
//!
//! The cell engine itself lives in `commands::agent_execution` — these two
//! handlers are the injection layer over it, resolving the admitted principal
//! and the addressed kernel before handing off.

use crate::commands::agent_execution::{
    cancel_python_cell_inner, resolve_execution_id, run_python_cell_inner_as_scoped,
};
use crate::database::local::agent_threads as threads_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::agent_execution::{PythonCellResult, PythonScopeInput};

/// Run one cell, either in the durable thread's kernel or in a detached child
/// workspace's. `turn_message_id` is required: a cell with edit authority must
/// be attributable to a durable user turn.
pub async fn run_python_cell(
    services: &AppServices,
    thread_id: String,
    execution_id: Option<String>,
    authored_workspace_id: Option<String>,
    turn_message_id: String,
    code: String,
    scope: PythonScopeInput,
) -> Result<PythonCellResult, CommandError> {
    let current_user_id = services.admitted_principal().await?;
    Ok(run_python_cell_inner_as_scoped(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &services.workspaces,
        &services.graph_runs,
        &services.authored,
        thread_id,
        code,
        scope,
        Some(turn_message_id),
        current_user_id,
        execution_id,
        authored_workspace_id,
    )
    .await?)
}

/// Interrupt the addressed kernel's running cell (the model-turn abort path,
/// §16.1). `false` when there was nothing to interrupt.
pub async fn cancel_python_cell(
    services: &AppServices,
    thread_id: String,
    execution_id: Option<String>,
    authored_workspace_id: Option<String>,
) -> Result<bool, CommandError> {
    let current_user_id = services.admitted_principal().await?;
    threads_db::get_thread_row(&services.db.0, &thread_id, current_user_id.as_deref())
        .await
        .map_err(|error| {
            CommandError::NotFound(format!(
                "agent thread '{thread_id}' is not available: {error}"
            ))
        })?;
    let execution_id =
        resolve_execution_id(&thread_id, execution_id, authored_workspace_id.as_deref())?;
    if let Some(workspace_id) = authored_workspace_id.as_deref() {
        services
            .authored
            .authorize_workspace(
                &services.db.0,
                current_user_id.as_deref(),
                &thread_id,
                workspace_id,
            )
            .await?;
    }
    Ok(cancel_python_cell_inner(
        &services.workspaces,
        &execution_id,
    ))
}
