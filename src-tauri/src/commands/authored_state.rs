//! Thin authenticated IPC surface for relational authored state.

use tauri::State;

use crate::database::local::auth;
use crate::database::Db;
use crate::models::authored_state::{
    AuthoredCurrentRevision, AuthoredHistoryPage, AuthoredRestoreResult, AuthoredTurnCommit,
    AuthoredWorkspace, AuthoredWorkspaceCheck, AuthoredWorkspaceCommit, AuthoredWorkspaceHandle,
    AuthoredWorkspaceInput, AuthoredWorkspaceMerge, CommitAuthoredWorkspaceInput,
    CreateAuthoredWorkspaceInput, FinalizeAuthoredTurnInput, ForkAuthoredWorkspaceInput,
    MergeAuthoredWorkspaceInput, MergeAuthoredWorkspaceIntoWorkspaceInput,
    PrepareAuthoredTurnInput, PreparedAuthoredTurn, RestoreAuthoredStateInput,
    WriteAuthoredWorkspaceGraphInput,
};
use crate::services::authored_documents::AuthoredDocuments;

#[tauri::command]
pub async fn authored_state_prepare_turn(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: PrepareAuthoredTurnInput,
) -> Result<PreparedAuthoredTurn, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .prepare_turn(&db.0, principal.as_deref(), input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_finalize_turn(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: FinalizeAuthoredTurnInput,
) -> Result<AuthoredTurnCommit, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .finalize_turn(&db.0, principal.as_deref(), input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_recover_turns(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    thread_id: String,
) -> Result<Vec<AuthoredTurnCommit>, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .recover_turns(&db.0, principal.as_deref(), &thread_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_list_history(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    thread_id: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<AuthoredHistoryPage, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .list_history(
            &db.0,
            principal.as_deref(),
            &thread_id,
            cursor.as_deref(),
            limit,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_restore(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: RestoreAuthoredStateInput,
) -> Result<AuthoredRestoreResult, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .restore(
            &db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.target_revision_id,
            &input.operation_id,
            input.mode,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_current_revision(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    thread_id: String,
) -> Result<AuthoredCurrentRevision, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .current_revision(&db.0, principal.as_deref(), &thread_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_create_workspace(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: CreateAuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceHandle, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .create_workspace(&db.0, principal.as_deref(), input)
        .await
        .map(workspace_handle)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_fork_workspace(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: ForkAuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceHandle, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .fork_workspace(&db.0, principal.as_deref(), input)
        .await
        .map(workspace_handle)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_check_workspace(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: AuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceCheck, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .check_workspace(
            &db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.workspace_id,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_write_workspace_graph(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: WriteAuthoredWorkspaceGraphInput,
) -> Result<crate::models::node_graph::Graph, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .write_workspace_graph(
            &db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.workspace_id,
            &input.graph,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_commit_workspace(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: CommitAuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceCommit, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .commit_workspace(&db.0, principal.as_deref(), input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_merge_workspace(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: MergeAuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceMerge, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .merge_workspace(&db.0, principal.as_deref(), input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_merge_workspace_into_workspace(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: MergeAuthoredWorkspaceIntoWorkspaceInput,
) -> Result<AuthoredWorkspaceMerge, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .merge_workspace_into_workspace(&db.0, principal.as_deref(), input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_remove_workspace(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    python_workspaces: State<'_, crate::agent_execution::PythonWorkspaceService>,
    graph_runs: State<'_, crate::agent_execution::GraphRunStore>,
    input: AuthoredWorkspaceInput,
) -> Result<(), String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .authorize_workspace_removal(
            &db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.workspace_id,
        )
        .await
        .map_err(|error| error.to_string())?;
    // Close admission and drain any in-flight child cell before the authored
    // branch disappears; otherwise a host call could write after removal.
    python_workspaces.retire_thread(&input.workspace_id).await?;
    authored
        .remove_workspace(
            &db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.workspace_id,
        )
        .await
        .map_err(|error| error.to_string())?;
    graph_runs.forget(&input.workspace_id);
    Ok(())
}

fn workspace_handle(workspace: AuthoredWorkspace) -> AuthoredWorkspaceHandle {
    AuthoredWorkspaceHandle {
        id: workspace.id,
        base_revision_id: workspace.base_revision_id,
        head_revision_id: workspace.head_revision_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_ipc_handle_never_serializes_the_host_path() {
        let handle = workspace_handle(AuthoredWorkspace {
            id: "workspace".into(),
            path: "/Users/alice/private/authored-workspaces/workspace".into(),
            base_revision_id: "base".into(),
            head_revision_id: "head".into(),
        });
        let encoded = serde_json::to_value(handle).unwrap();
        assert_eq!(encoded["id"], "workspace");
        assert_eq!(encoded["baseRevisionId"], "base");
        assert_eq!(encoded["headRevisionId"], "head");
        assert!(encoded.get("path").is_none());
        assert!(!encoded.to_string().contains("/Users/alice"));
    }
}
