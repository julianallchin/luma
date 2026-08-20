use crate::database::local::agent_threads as db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AgentThreadDetail, AgentThreadMessage,
    AppendAgentThreadMessagesInput, CreateAgentThreadInput,
};

pub async fn agent_thread_create(
    services: &AppServices,
    input: CreateAgentThreadInput,
) -> Result<AgentThread, CommandError> {
    let owner_user_id = services.admitted_principal().await?;
    services
        .authored
        .create_thread_with_authored_state(&services.db.0, input, owner_user_id.as_deref())
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))
}

/// A thread owned by another principal is invisible, not forbidden.
pub async fn agent_thread_get(
    services: &AppServices,
    thread_id: String,
) -> Result<AgentThreadDetail, CommandError> {
    let owner_user_id = services.admitted_principal().await?;
    Ok(db::get_thread(&services.db.0, &thread_id, owner_user_id.as_deref()).await?)
}

/// Each filter is an independent `AND`; `None` is a wildcard for that field.
/// The remaining identity fields have no server-side filter — callers that care
/// re-filter the result.
pub async fn agent_thread_list(
    services: &AppServices,
    agent_kind: Option<String>,
    subject_kind: Option<String>,
    subject_id: Option<String>,
) -> Result<Vec<AgentThread>, CommandError> {
    let owner_user_id = services.admitted_principal().await?;
    Ok(db::list_threads(
        &services.db.0,
        agent_kind.as_deref(),
        subject_kind.as_deref(),
        subject_id.as_deref(),
        owner_user_id.as_deref(),
    )
    .await?)
}

/// Appends only if the caller's transcript head is still current. A lost race
/// is a [`CommandError::Conflict`] carrying both revisions structurally, so a
/// caller can branch on it without parsing the message.
pub async fn agent_thread_append_messages(
    services: &AppServices,
    thread_id: String,
    input: AppendAgentThreadMessagesInput,
) -> Result<Vec<AgentThreadMessage>, CommandError> {
    let owner_user_id = services.admitted_principal().await?;
    match db::append_messages_at_head(&services.db.0, &thread_id, input, owner_user_id.as_deref())
        .await?
    {
        AgentThreadAppendOutcome::Appended { messages, .. } => Ok(messages),
        AgentThreadAppendOutcome::HeadMoved {
            expected_head_message_id,
            current_head_message_id,
        } => Err(CommandError::Conflict {
            message: db::transcript_head_moved_error(
                expected_head_message_id.as_deref(),
                current_head_message_id.as_deref(),
            ),
            expected: expected_head_message_id,
            found: current_head_message_id,
        }),
    }
}

/// Delete the thread once its child authored workspaces, its Python workspace,
/// and its published graph runs have been retired. Revision history remains
/// restorable.
pub async fn agent_thread_delete(
    services: &AppServices,
    thread_id: String,
) -> Result<(), CommandError> {
    let owner_user_id = services.admitted_principal().await?;
    services
        .authored
        .delete_thread_with_authored_state(
            &services.db.0,
            owner_user_id.as_deref(),
            &thread_id,
            |workspace_ids| async {
                for workspace_id in workspace_ids {
                    services.workspaces.retire_thread(&workspace_id).await?;
                    services.graph_runs.forget(&workspace_id);
                }
                services.workspaces.retire_thread(&thread_id).await?;
                services.graph_runs.forget(&thread_id);
                Ok(())
            },
        )
        .await?;
    Ok(())
}

pub async fn agent_thread_rename(
    services: &AppServices,
    thread_id: String,
    title: Option<String>,
) -> Result<AgentThread, CommandError> {
    let owner_user_id = services.admitted_principal().await?;
    Ok(db::rename_thread(
        &services.db.0,
        &thread_id,
        title.as_deref(),
        owner_user_id.as_deref(),
    )
    .await?)
}
