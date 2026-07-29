//! Tauri commands for durable agent threads.
//!
//! No `AppHandle` anywhere — the lifecycle commands take the database and, where
//! a thread owns live execution state, the workspace service and the graph-run
//! store, so the whole surface stays reachable from the headless harness.

use tauri::State;

use crate::agent_execution::graph_runs::GraphRunStore;
use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::database::local::agent_threads as db;
use crate::database::Db;
use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, CreateAgentThreadInput,
    NewAgentThreadMessage,
};

#[tauri::command]
pub async fn agent_thread_create(
    db: State<'_, Db>,
    input: CreateAgentThreadInput,
) -> Result<AgentThread, String> {
    db::create_thread(&db.0, input).await
}

#[tauri::command]
pub async fn agent_thread_get(
    db: State<'_, Db>,
    thread_id: String,
) -> Result<AgentThreadDetail, String> {
    db::get_thread(&db.0, &thread_id).await
}

#[tauri::command]
pub async fn agent_thread_list(
    db: State<'_, Db>,
    agent_kind: Option<String>,
    subject_kind: Option<String>,
    subject_id: Option<String>,
) -> Result<Vec<AgentThread>, String> {
    db::list_threads(
        &db.0,
        agent_kind.as_deref(),
        subject_kind.as_deref(),
        subject_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn agent_thread_append_messages(
    db: State<'_, Db>,
    thread_id: String,
    messages: Vec<NewAgentThreadMessage>,
) -> Result<Vec<AgentThreadMessage>, String> {
    db::append_messages(&db.0, &thread_id, messages).await
}

#[tauri::command]
pub async fn agent_thread_truncate_from(
    db: State<'_, Db>,
    thread_id: String,
    seq: i64,
) -> Result<u64, String> {
    db::truncate_from_seq(&db.0, &thread_id, seq).await
}

/// Clear the conversation *and* its Python state. A reset that left the kernel
/// running would keep invisible state across a conversation the user believes is
/// empty (design §13.5, acceptance §16) — so the process is replaced and scratch
/// is wiped, not `globals().clear()`.
///
/// The database is cleared first: both halves are idempotent, and a stale kernel
/// with an empty transcript is recoverable where the reverse is not.
#[tauri::command]
pub async fn agent_thread_reset(
    db: State<'_, Db>,
    workspaces: State<'_, PythonWorkspaceService>,
    graph_runs: State<'_, GraphRunStore>,
    thread_id: String,
) -> Result<u64, String> {
    let deleted = db::reset_thread(&db.0, &thread_id).await?;
    graph_runs.forget(&thread_id);
    workspaces.workspace_for(&thread_id)?.reset()?;
    Ok(deleted)
}

/// Delete the thread and everything it owned: the kernel, its workspace
/// directory, and its published graph run (§21.1).
#[tauri::command]
pub async fn agent_thread_delete(
    db: State<'_, Db>,
    workspaces: State<'_, PythonWorkspaceService>,
    graph_runs: State<'_, GraphRunStore>,
    thread_id: String,
) -> Result<(), String> {
    db::delete_thread(&db.0, &thread_id).await?;
    graph_runs.forget(&thread_id);
    workspaces.shutdown_thread(&thread_id)
}

#[tauri::command]
pub async fn agent_thread_rename(
    db: State<'_, Db>,
    thread_id: String,
    title: Option<String>,
) -> Result<AgentThread, String> {
    db::rename_thread(&db.0, &thread_id, title.as_deref()).await
}
