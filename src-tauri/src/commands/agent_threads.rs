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
use crate::models::agent_threads::{AgentThread, AgentThreadDetail};
use crate::services::authored_documents::AuthoredDocuments;

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

/// Delete the thread after its child authored workspaces, Python workspace,
/// and published graph run have been retired. Revision history remains
/// restorable.
#[tauri::command]
pub async fn agent_thread_delete(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    workspaces: State<'_, std::sync::Arc<PythonWorkspaceService>>,
    graph_runs: State<'_, std::sync::Arc<GraphRunStore>>,
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
