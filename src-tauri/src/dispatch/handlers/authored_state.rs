//! Relational authored state: the Git-backed document behind an agent thread,
//! its detached workspaces, and the prepare/finalize turn protocol.
//!
//! Every handler resolves the app database's write-admission principal and
//! hands it to [`AuthoredDocuments`], which is the layer that decides what that
//! principal may see or write. Nothing here authorizes on its own.

use crate::dispatch::{AppServices, CommandError};
use crate::models::authored_state::{
    AuthoredHistoryPage, AuthoredRestoreResult, AuthoredTurnCommit, AuthoredWorkspace,
    AuthoredWorkspaceCheck, AuthoredWorkspaceCommit, AuthoredWorkspaceHandle,
    AuthoredWorkspaceInput, AuthoredWorkspaceMerge, CommitAuthoredWorkspaceInput,
    CreateAuthoredWorkspaceInput, FinalizeAuthoredTurnInput, MergeAuthoredWorkspaceInput,
    PrepareAuthoredTurnInput, PreparedAuthoredTurn, RestoreAuthoredStateInput,
};
use crate::services::authored_state::Actor;

/// Name the writer behind every revision this host produces from now on.
///
/// A host with no agent of its own — the app — leaves this alone and writes as
/// `user`. `luma-mcp` calls it once, with the `clientInfo` of the client that
/// connected, so an external client's writes are labelled as that client's
/// rather than as the operator's. A thread that names its own actor still wins
/// over this for the revisions it produces.
pub async fn authored_state_set_session_actor(
    services: &AppServices,
    actor: String,
) -> Result<(), CommandError> {
    let actor = Actor::parse(&actor).map_err(|error| CommandError::Invalid(error.to_string()))?;
    services.authored.set_session_actor(actor);
    Ok(())
}

pub async fn authored_state_prepare_turn(
    services: &AppServices,
    input: PrepareAuthoredTurnInput,
) -> Result<PreparedAuthoredTurn, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .prepare_turn(&services.db.0, principal.as_deref(), input)
        .await?)
}

pub async fn authored_state_finalize_turn(
    services: &AppServices,
    input: FinalizeAuthoredTurnInput,
) -> Result<AuthoredTurnCommit, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .finalize_turn(&services.db.0, principal.as_deref(), input)
        .await?)
}

/// Commit every turn that was prepared but never finalized — the crash path.
pub async fn authored_state_recover_turns(
    services: &AppServices,
    thread_id: String,
) -> Result<Vec<AuthoredTurnCommit>, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .recover_turns(&services.db.0, principal.as_deref(), &thread_id)
        .await?)
}

pub async fn authored_state_list_history(
    services: &AppServices,
    thread_id: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<AuthoredHistoryPage, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .list_history(
            &services.db.0,
            principal.as_deref(),
            &thread_id,
            cursor.as_deref(),
            limit,
        )
        .await?)
}

pub async fn authored_state_restore(
    services: &AppServices,
    input: RestoreAuthoredStateInput,
) -> Result<AuthoredRestoreResult, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .restore(
            &services.db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.target_revision_id,
            &input.operation_id,
            input.mode,
        )
        .await?)
}

pub async fn authored_state_create_workspace(
    services: &AppServices,
    input: CreateAuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceHandle, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(workspace_handle(
        services
            .authored
            .create_workspace(&services.db.0, principal.as_deref(), input)
            .await?,
    ))
}

pub async fn authored_state_check_workspace(
    services: &AppServices,
    input: AuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceCheck, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .check_workspace(
            &services.db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.workspace_id,
        )
        .await?)
}

pub async fn authored_state_commit_workspace(
    services: &AppServices,
    input: CommitAuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceCommit, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .commit_workspace(&services.db.0, principal.as_deref(), input)
        .await?)
}

pub async fn authored_state_merge_workspace(
    services: &AppServices,
    input: MergeAuthoredWorkspaceInput,
) -> Result<AuthoredWorkspaceMerge, CommandError> {
    let principal = services.admitted_principal().await?;
    Ok(services
        .authored
        .merge_workspace(&services.db.0, principal.as_deref(), input)
        .await?)
}

pub async fn authored_state_remove_workspace(
    services: &AppServices,
    input: AuthoredWorkspaceInput,
) -> Result<(), CommandError> {
    let principal = services.admitted_principal().await?;
    services
        .authored
        .authorize_workspace_removal(
            &services.db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.workspace_id,
        )
        .await?;
    // Close admission and drain any in-flight child cell before the authored
    // branch disappears; otherwise a host call could write after removal.
    services
        .workspaces
        .retire_thread(&input.workspace_id)
        .await?;
    services
        .authored
        .remove_workspace(
            &services.db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.workspace_id,
        )
        .await?;
    services.graph_runs.forget(&input.workspace_id);
    Ok(())
}

/// The wire shape of a workspace: its identity and revisions, never its host
/// path.
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
