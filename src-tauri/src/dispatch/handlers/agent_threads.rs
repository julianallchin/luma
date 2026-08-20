use crate::database::local::agent_threads as db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AgentThreadMessage, AppendAgentThreadMessagesInput,
    CreateAgentThreadInput,
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
