//! Python cells: run one in an agent thread's kernel, or interrupt the one in
//! flight.
//!
//! The cell engine itself lives in `services::agent_execution` — these two
//! handlers are the injection layer over it, resolving the admitted principal
//! before handing off.
//!
//! A command always addresses the thread's own kernel. Child workspaces are a
//! Rust-loop concept: a subagent's cell is issued by
//! [`run_python_cell_inner`] from inside the turn, never over IPC, so there is
//! no execution id or workspace id on the wire for a caller to get wrong.

use crate::database::local::agent_threads as threads_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::agent_execution::{PythonCellResult, PythonScopeInput};
use crate::services::agent_execution::{cancel_python_cell_inner, run_python_cell_inner};

/// Run one cell in the durable thread's kernel. `turn_message_id` is required:
/// a cell with edit authority must be attributable to a durable turn — a user
/// one, or the session turn a non-conversational client opened.
pub async fn run_python_cell(
    services: &AppServices,
    thread_id: String,
    turn_message_id: String,
    code: String,
    scope: PythonScopeInput,
) -> Result<PythonCellResult, CommandError> {
    let current_user_id = services.admitted_principal().await?;
    Ok(run_python_cell_inner(
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
        None,
        None,
    )
    .await?)
}

/// Interrupt the thread kernel's running cell (the model-turn abort path,
/// §16.1). `false` when there was nothing to interrupt.
pub async fn cancel_python_cell(
    services: &AppServices,
    thread_id: String,
) -> Result<bool, CommandError> {
    let current_user_id = services.admitted_principal().await?;
    threads_db::get_thread_row(&services.db.0, &thread_id, current_user_id.as_deref())
        .await
        .map_err(|error| {
            CommandError::NotFound(format!(
                "agent thread '{thread_id}' is not available: {error}"
            ))
        })?;
    Ok(cancel_python_cell_inner(&services.workspaces, &thread_id))
}
