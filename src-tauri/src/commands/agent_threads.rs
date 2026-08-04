//! Tauri commands for durable agent threads.
//!
//! No `AppHandle` anywhere — the lifecycle commands take the database and, where
//! a thread owns live execution state, the workspace service and the graph-run
//! store, so the whole surface stays reachable from the headless harness.

use tauri::State;

use crate::agent_execution::graph_runs::GraphRunStore;
use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::database::local::{agent_threads as db, auth};
use crate::database::Db;
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AgentThreadDetail, AgentThreadMessage,
    AppendAgentThreadMessagesInput, CreateAgentThreadInput,
};
use crate::services::authored_documents::AuthoredDocuments;

#[tauri::command]
pub async fn agent_thread_create(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: CreateAgentThreadInput,
) -> Result<AgentThread, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    authored
        .create_thread_with_authored_state(&db.0, input, owner_user_id.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn agent_thread_get(
    db: State<'_, Db>,
    thread_id: String,
) -> Result<AgentThreadDetail, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    db::get_thread(&db.0, &thread_id, owner_user_id.as_deref()).await
}

#[tauri::command]
pub async fn agent_thread_list(
    db: State<'_, Db>,
    agent_kind: Option<String>,
    subject_kind: Option<String>,
    subject_id: Option<String>,
) -> Result<Vec<AgentThread>, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    db::list_threads(
        &db.0,
        agent_kind.as_deref(),
        subject_kind.as_deref(),
        subject_id.as_deref(),
        owner_user_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn agent_thread_append_messages(
    db: State<'_, Db>,
    thread_id: String,
    input: AppendAgentThreadMessagesInput,
) -> Result<Vec<AgentThreadMessage>, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    match db::append_messages_at_head(&db.0, &thread_id, input, owner_user_id.as_deref()).await? {
        AgentThreadAppendOutcome::Appended { messages, .. } => Ok(messages),
        AgentThreadAppendOutcome::HeadMoved {
            expected_head_message_id,
            current_head_message_id,
        } => Err(format!(
            "Agent transcript changed before append (expected {}, found {}); reload the conversation before retrying",
            expected_head_message_id.as_deref().unwrap_or("an empty transcript"),
            current_head_message_id.as_deref().unwrap_or("an empty transcript"),
        )),
    }
}

/// Delete the thread after its child authored workspaces, Python workspace,
/// and published graph run have been retired. Revision history remains
/// restorable.
#[tauri::command]
pub async fn agent_thread_delete(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    workspaces: State<'_, PythonWorkspaceService>,
    graph_runs: State<'_, GraphRunStore>,
    thread_id: String,
) -> Result<(), String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    authored
        .delete_thread_with_authored_state(
            &db.0,
            owner_user_id.as_deref(),
            &thread_id,
            |workspace_ids| async {
                for workspace_id in workspace_ids {
                    workspaces.retire_thread(&workspace_id).await?;
                    graph_runs.forget(&workspace_id);
                }
                workspaces.retire_thread(&thread_id).await?;
                graph_runs.forget(&thread_id);
                Ok(())
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_thread_rename(
    db: State<'_, Db>,
    thread_id: String,
    title: Option<String>,
) -> Result<AgentThread, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    db::rename_thread(
        &db.0,
        &thread_id,
        title.as_deref(),
        owner_user_id.as_deref(),
    )
    .await
}
