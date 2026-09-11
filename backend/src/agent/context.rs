//! Editor context is captured in the durable user turn, never in tool arguments.

use crate::models::agent_threads::AgentThread;
use sqlx::SqlitePool;

use super::TurnContext;

pub const PART_TYPE: &str = "data-luma-context";

pub async fn execution_thread(
    pool: &SqlitePool,
    thread_id: &str,
    principal: Option<&str>,
) -> Result<AgentThread, String> {
    let mut thread =
        crate::database::local::agent_threads::get_thread_row(pool, thread_id, principal).await?;
    let context: Option<String> = sqlx::query_scalar(
        "WITH RECURSIVE lineage AS (
            SELECT message.id, message.parent_message_id, message.depth, message.role, message.parts_json
            FROM agent_thread_messages message
            JOIN agent_thread_transcript_heads head ON head.head_message_id = message.id
            WHERE head.thread_id = ? AND head.uid IS ?
            UNION ALL
            SELECT parent.id, parent.parent_message_id, parent.depth, parent.role, parent.parts_json
            FROM agent_thread_messages parent JOIN lineage child ON parent.id = child.parent_message_id
         )
         SELECT json_extract(part.value, '$.data')
         FROM lineage, json_each(lineage.parts_json) part
         WHERE lineage.role = 'user' AND json_extract(part.value, '$.type') = ?
         ORDER BY lineage.depth DESC LIMIT 1"
    ).bind(thread_id).bind(principal).bind(PART_TYPE).fetch_optional(pool).await
        .map_err(|error| format!("read turn context: {error}"))?;
    if let Some(context) = context {
        let context: TurnContext = serde_json::from_str(&context)
            .map_err(|error| format!("invalid turn context: {error}"))?;
        apply(&mut thread, &context);
        thread.route()?;
    }
    Ok(thread)
}

fn apply(thread: &mut AgentThread, context: &TurnContext) {
    let scope = context.scope.as_ref();
    thread.agent_kind = scope
        .map_or("unbound", |scope| scope.agent_kind.as_str())
        .into();
    thread.subject_kind = scope.map(|scope| scope.subject_kind.as_str().into());
    thread.subject_id = scope.map(|scope| scope.subject_id.clone());
    thread.implementation_id = scope.and_then(|scope| scope.implementation_id.clone());
    thread.venue_id = scope.and_then(|scope| scope.venue_id.clone());
    thread.score_id = scope.and_then(|scope| scope.score_id.clone());
}
