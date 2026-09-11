//! Durable principal-bound agent threads and immutable transcript nodes. Every
//! operation receives its trusted principal separately from caller-controlled payloads:
//! `Some(uid)` can access only that owner's rows, while `None` can access only
//! legacy/signed-out rows whose owner is SQL `NULL`.

use sqlx::{SqliteConnection, SqlitePool};
use std::collections::HashSet;
use uuid::Uuid;

use crate::database::local::auth::principal_key;
#[cfg(test)]
use crate::models::agent_threads::NewAgentThreadMessage;
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AgentThreadDetail, AgentThreadMessage,
    AgentThreadTranscriptHead, AgentThreadUsage, AppendAgentThreadMessagesInput,
    CreateAgentThreadInput,
};

const THREAD_COLUMNS: &str =
    "id, uid, agent_kind, subject_kind, subject_id, implementation_id, venue_id, score_id, forked_from_thread_id, forked_at_message_id, parent_thread_id, parent_call_id, title, actor, engine, model, provider, effort, created_at, updated_at";

/// The FROM/WHERE every thread *read* shares: active threads, admitted by the
/// write-admission singleton, owned by the bound principal (one `?`).
///
/// Spelled once because it is the access-control predicate. A second listing
/// that reconstructed it by hand would be one clause away from serving another
/// account's conversations, and that is not a difference a reviewer would spot
/// in a wall of SQL. Anything appending filters to this binds the principal
/// first and its own values after.
const LIVE_THREADS_FOR_PRINCIPAL: &str = "FROM agent_threads thread
         CROSS JOIN auth_write_admission admission
         WHERE thread.lifecycle_state = 'active'
           AND admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND thread.uid IS admission.active_uid
           AND admission.active_uid IS ?";

fn thread_not_found(thread_id: &str) -> String {
    format!("Agent thread not found: {thread_id}")
}

// Sync delivery is not enqueued here. A thread, a message node, an append
// receipt and a deletion receipt all become visible to push by existing with an
// unset `synced_at`; the row is the payload, so there is nothing to copy.

/// Create a thread. The id is always generated here — thread identity is opaque
/// and never supplied by the caller.
pub async fn create_thread(
    pool: &SqlitePool,
    input: CreateAgentThreadInput,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    let id = Uuid::new_v4().to_string();
    create_thread_with_id(pool, &id, input, owner_user_id).await
}

/// Insert a caller-derived thread identity, so a retried creation finds the
/// thread it made rather than making a second one.
pub(crate) async fn create_thread_with_id(
    pool: &SqlitePool,
    id: &str,
    input: CreateAgentThreadInput,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    input.route()?;
    let settings = super::settings::get_all_settings(pool).await?;
    let defaults = crate::agent::engine::catalog::Selection::configured(&settings)
        .map_err(|e| e.to_string())?;
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread creation: {e}"))?;
    sqlx::query(
        "INSERT INTO agent_threads (id, uid, agent_kind, subject_kind, subject_id, implementation_id, venue_id, score_id, title, parent_thread_id, parent_call_id, engine, model, provider, effort)
         SELECT ?, admission.active_uid, ?, ?, ?, ?, ?, ?, ?, ?, ?,
             COALESCE(parent.engine, ?),
             CASE WHEN parent.id IS NOT NULL THEN parent.model ELSE ? END,
             CASE WHEN parent.id IS NOT NULL THEN parent.provider ELSE ? END,
             CASE WHEN parent.id IS NOT NULL THEN parent.effort ELSE ? END
         FROM auth_write_admission admission
         LEFT JOIN agent_threads parent ON parent.id = ? AND parent.uid IS admission.active_uid
         WHERE admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0 AND admission.active_uid IS ?",
    )
    .bind(id)
    .bind(&input.agent_kind)
    .bind(&input.subject_kind)
    .bind(&input.subject_id)
    .bind(&input.implementation_id)
    .bind(&input.venue_id)
    .bind(&input.score_id)
    .bind(&input.title)
    .bind(&input.parent_thread_id)
    .bind(&input.parent_call_id)
    .bind(defaults.service.engine().key())
    .bind(&defaults.model)
    .bind(defaults.service.provider().map(|p| p.as_str()))
    .bind(&defaults.effort)
    .bind(&input.parent_thread_id)
    .bind(owner_user_id)
    .execute(&mut *transaction)
    .await
    .map_err(|e| format!("Failed to create agent thread: {e}"))?;

    let thread = get_thread_row_for_connection(&mut transaction, id, owner_user_id, true).await?;
    transaction
        .commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread creation: {e}"))?;
    Ok(thread)
}

/// Create a new conversation identity at an immutable prefix of another
/// same-principal transcript. No message row is copied or rewritten. The new
/// thread clones the source's authored route, while its independently mutable
/// transcript head begins at `at_message_id` (`None` means an empty prefix).
pub async fn fork_thread_with_id(
    pool: &SqlitePool,
    new_thread_id: &str,
    source_thread_id: &str,
    at_message_id: Option<&str>,
    title: Option<&str>,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread fork: {e}"))?;
    let fork = fork_thread_for_connection(
        &mut transaction,
        new_thread_id,
        source_thread_id,
        at_message_id,
        title,
        owner_user_id,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread fork: {e}"))?;
    Ok(fork)
}

/// Transaction-local form of [`fork_thread_with_id`]. Restore-with-rewind uses
/// this to advance authored state and create the conversation fork in one
/// SQLite commit. The caller owns transaction boundaries.
pub(crate) async fn fork_thread_for_connection(
    connection: &mut SqliteConnection,
    new_thread_id: &str,
    source_thread_id: &str,
    at_message_id: Option<&str>,
    title: Option<&str>,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    if new_thread_id.is_empty() || new_thread_id == source_thread_id {
        return Err("Agent thread fork requires a distinct non-empty thread id".into());
    }
    let source =
        get_thread_row_for_connection(connection, source_thread_id, owner_user_id, true).await?;
    let (source_head, _) =
        transcript_head_for_connection(connection, source_thread_id, owner_user_id).await?;
    let fork_count = match at_message_id {
        None => 0,
        Some(message_id) => {
            let found = sqlx::query_scalar::<_, i64>(
                "WITH RECURSIVE lineage(id, parent_message_id, depth) AS (
                     SELECT message.id, message.parent_message_id, message.depth
                     FROM agent_thread_messages AS message
                     WHERE message.id = ?
                     UNION ALL
                     SELECT parent.id, parent.parent_message_id, parent.depth
                     FROM agent_thread_messages AS parent
                     JOIN lineage AS child ON child.parent_message_id = parent.id
                 )
                 SELECT depth + 1 FROM lineage WHERE id = ?",
            )
            .bind(source_head.as_deref())
            .bind(message_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|e| format!("Failed to validate agent thread fork prefix: {e}"))?
            .ok_or_else(|| "Agent thread fork point is not in the source transcript".to_owned())?;
            found
        }
    };

    sqlx::query(
        "INSERT INTO agent_threads
         (id, uid, agent_kind, subject_kind, subject_id,
          implementation_id, venue_id, score_id, forked_from_thread_id,
          forked_at_message_id, title, engine, model, provider, effort)
         SELECT ?, admission.active_uid, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
         FROM auth_write_admission AS admission
         WHERE admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0 AND admission.active_uid IS ?",
    )
    .bind(new_thread_id)
    .bind(&source.agent_kind)
    .bind(&source.subject_kind)
    .bind(&source.subject_id)
    .bind(&source.implementation_id)
    .bind(&source.venue_id)
    .bind(&source.score_id)
    .bind(source_thread_id)
    .bind(at_message_id)
    .bind(title.or(source.title.as_deref()))
    .bind(&source.engine)
    .bind(&source.model)
    .bind(&source.provider)
    .bind(&source.effort)
    .bind(owner_user_id)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to create agent thread fork: {e}"))?;

    if let Some(message_id) = at_message_id {
        let moved = sqlx::query(
            "UPDATE agent_thread_transcript_heads
             SET head_message_id = ?, message_count = ?,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE thread_id = ? AND uid IS ?
               AND head_message_id IS NULL AND message_count = 0",
        )
        .bind(message_id)
        .bind(fork_count)
        .bind(new_thread_id)
        .bind(owner_user_id)
        .execute(&mut *connection)
        .await
        .map_err(|e| format!("Failed to set agent thread fork prefix: {e}"))?
        .rows_affected();
        if moved != 1 {
            return Err("Agent thread fork head changed during creation".into());
        }
    }

    let fork =
        get_thread_row_for_connection(connection, new_thread_id, owner_user_id, true).await?;
    Ok(fork)
}

/// Fetch a thread row without its messages.
pub async fn get_thread_row(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|e| format!("Failed to open agent thread read: {e}"))?;
    get_thread_row_for_connection(&mut connection, thread_id, owner_user_id, true).await
}

async fn get_thread_row_for_connection(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
    active_only: bool,
) -> Result<AgentThread, String> {
    let lifecycle = if active_only {
        "AND thread.lifecycle_state = 'active'"
    } else {
        ""
    };
    let row = sqlx::query_as::<_, AgentThread>(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM agent_threads thread
         CROSS JOIN auth_write_admission admission
         WHERE thread.id = ? {lifecycle}
           AND admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND thread.uid IS admission.active_uid
           AND admission.active_uid IS ?",
        THREAD_COLUMNS
            .split(", ")
            .map(|column| format!("thread.{column}"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
    .bind(thread_id)
    .bind(owner_user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|e| format!("Failed to load agent thread: {e}"))?;
    row.ok_or_else(|| thread_not_found(thread_id))
}

/// Refuse a write to a thread this admission does not own.
pub(crate) async fn assert_thread_active(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let found = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM agent_threads thread
         CROSS JOIN auth_write_admission admission
         WHERE thread.id = ? AND thread.uid IS ?
           AND thread.lifecycle_state = 'active'
           AND admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND thread.uid IS admission.active_uid",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|e| format!("Failed to authorize active agent thread: {e}"))?;

    found.map(|_| ()).ok_or_else(|| thread_not_found(thread_id))
}

/// Fetch a thread together with its full ordered message history.
pub async fn get_thread(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<AgentThreadDetail, String> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to begin agent thread read: {e}"))?;
    let thread =
        get_thread_row_for_connection(&mut transaction, thread_id, owner_user_id, true).await?;
    let messages = list_messages_for_connection(&mut transaction, thread_id, owner_user_id).await?;
    transaction
        .commit()
        .await
        .map_err(|e| format!("Failed to finish agent thread read: {e}"))?;
    Ok(AgentThreadDetail { thread, messages })
}

/// All messages of a thread, in seq order.
pub async fn list_messages(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to begin agent message read: {e}"))?;
    get_thread_row_for_connection(&mut transaction, thread_id, owner_user_id, true).await?;
    let messages = list_messages_for_connection(&mut transaction, thread_id, owner_user_id).await?;
    transaction
        .commit()
        .await
        .map_err(|e| format!("Failed to finish agent message read: {e}"))?;
    Ok(messages)
}

async fn list_messages_for_connection(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    let messages = sqlx::query_as::<_, AgentThreadMessage>(
        "WITH RECURSIVE lineage(
             id, parent_message_id, depth, role, parts_json, created_at
         ) AS (
             SELECT message.id, message.parent_message_id, message.depth,
                    message.role, message.parts_json, message.created_at
             FROM agent_thread_transcript_heads AS head
             JOIN agent_thread_messages AS message
               ON message.id = head.head_message_id
             WHERE head.thread_id = ? AND head.uid IS ?
             UNION ALL
             SELECT parent.id, parent.parent_message_id, parent.depth,
                    parent.role, parent.parts_json, parent.created_at
             FROM agent_thread_messages AS parent
             JOIN lineage AS child ON child.parent_message_id = parent.id
         )
         SELECT id, ? AS thread_id, parent_message_id, depth AS seq,
                role, parts_json, created_at
         FROM lineage ORDER BY depth ASC",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .bind(thread_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|e| format!("Failed to load agent thread messages: {e}"))?;
    let expected = sqlx::query_scalar::<_, i64>(
        "SELECT message_count FROM agent_thread_transcript_heads
         WHERE thread_id = ? AND uid IS ?",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|e| format!("Failed to verify agent transcript length: {e}"))?;
    if messages.len() != usize::try_from(expected).unwrap_or(usize::MAX)
        || messages
            .iter()
            .enumerate()
            .any(|(index, message)| message.seq != index as i64)
    {
        return Err("Agent transcript head points to an invalid message chain".into());
    }
    Ok(messages)
}

/// Read the exact compare-and-swap token for a conversation transcript.
pub async fn transcript_head(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<AgentThreadTranscriptHead, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|e| format!("Failed to open agent transcript head read: {e}"))?;
    get_thread_row_for_connection(&mut connection, thread_id, owner_user_id, true).await?;
    let (head_message_id, message_count) =
        transcript_head_for_connection(&mut connection, thread_id, owner_user_id).await?;
    Ok(AgentThreadTranscriptHead {
        thread_id: thread_id.to_owned(),
        head_message_id,
        message_count,
    })
}

async fn transcript_head_for_connection(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(Option<String>, i64), String> {
    sqlx::query_as(
        "SELECT head_message_id, message_count
         FROM agent_thread_transcript_heads
         WHERE thread_id = ? AND uid IS ?",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .fetch_optional(connection)
    .await
    .map_err(|e| format!("Failed to load agent transcript head: {e}"))?
    .ok_or_else(|| thread_not_found(thread_id))
}

/// List threads, most recently updated first. Both filters are optional and
/// independent: `agent_kind` narrows by agent, `subject` by (kind, id) pair.
pub async fn list_threads(
    pool: &SqlitePool,
    agent_kind: Option<&str>,
    subject_kind: Option<&str>,
    subject_id: Option<&str>,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThread>, String> {
    let columns = THREAD_COLUMNS
        .split(", ")
        .map(|column| format!("thread.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("SELECT {columns} {LIVE_THREADS_FOR_PRINCIPAL}");
    if agent_kind.is_some() {
        sql.push_str(" AND agent_kind = ?");
    }
    if subject_kind.is_some() {
        sql.push_str(" AND subject_kind = ?");
    }
    if subject_id.is_some() {
        sql.push_str(" AND subject_id = ?");
    }
    sql.push_str(" ORDER BY updated_at DESC, created_at DESC");

    let mut query = sqlx::query_as::<_, AgentThread>(sqlx::AssertSqlSafe(sql));
    query = query.bind(owner_user_id);
    for value in [agent_kind, subject_kind, subject_id].into_iter().flatten() {
        query = query.bind(value.to_string());
    }

    query
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list agent threads: {}", e))
}

/// All live transcripts for one principal, read in one recursive query for
/// history summaries and search. Each row retains its conversation id.
pub async fn list_history_messages(
    pool: &SqlitePool,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    let sql = format!(
        "WITH RECURSIVE lineage(
             thread_id, id, parent_message_id, depth, role, parts_json, created_at
         ) AS (
             SELECT head.thread_id, message.id, message.parent_message_id,
                    message.depth, message.role, message.parts_json, message.created_at
             FROM agent_thread_transcript_heads AS head
             JOIN agent_thread_messages AS message
               ON message.id = head.head_message_id
             WHERE head.uid IS ?
               AND head.thread_id IN (
                   SELECT thread.id {LIVE_THREADS_FOR_PRINCIPAL}
               )
             UNION ALL
             SELECT child.thread_id, parent.id, parent.parent_message_id,
                    parent.depth, parent.role, parent.parts_json, parent.created_at
             FROM agent_thread_messages AS parent
             JOIN lineage AS child ON child.parent_message_id = parent.id
         )
         SELECT id, thread_id, parent_message_id, depth AS seq,
                role, parts_json, created_at
         FROM lineage ORDER BY thread_id ASC, depth ASC"
    );
    sqlx::query_as::<_, AgentThreadMessage>(sqlx::AssertSqlSafe(sql))
        .bind(owner_user_id)
        .bind(owner_user_id)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list agent history messages: {e}"))
}

/// Atomically append one message batch at the caller's observed head. The
/// idempotency row is committed beside the messages, so an exact retry after
/// response loss returns the original result. A stale head is an explicit
/// reload boundary and is never silently rebased.
pub async fn append_messages(
    pool: &SqlitePool,
    thread_id: &str,
    input: AppendAgentThreadMessagesInput,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    match append_messages_at_head(pool, thread_id, input, owner_user_id).await? {
        AgentThreadAppendOutcome::Appended { messages, .. } => Ok(messages),
        AgentThreadAppendOutcome::HeadMoved {
            expected_head_message_id,
            current_head_message_id,
        } => Err(transcript_head_moved_error(
            expected_head_message_id.as_deref(),
            current_head_message_id.as_deref(),
        )),
    }
}

/// Append only if the caller's exact immutable transcript head is still
/// current. A concurrent winner is returned as `HeadMoved`; no node, receipt,
/// or pointer is partially written. The caller must reload and explicitly
/// decide whether to discard, re-plan, or fork from the observed prefix.
pub async fn append_messages_at_head(
    pool: &SqlitePool,
    thread_id: &str,
    input: AppendAgentThreadMessagesInput,
    owner_user_id: Option<&str>,
) -> Result<AgentThreadAppendOutcome, String> {
    validate_append_operation_id(&input.operation_id)?;
    if input.messages.is_empty() {
        return Err("Agent thread append must contain at least one message".into());
    }
    let expected_head_message_id = input.expected_head_message_id.clone();
    let input = with_generated_message_ids(input);
    let message_count = i64::try_from(input.messages.len())
        .map_err(|_| "Agent thread append contains too many messages".to_owned())?;
    let prepared = input
        .messages
        .iter()
        .map(|message| {
            let id = message
                .id
                .as_ref()
                .expect("message IDs were normalized")
                .clone();
            let parts_json = serde_json::to_string(&message.parts)
                .map_err(|e| format!("Failed to serialize message parts: {e}"))?;
            Ok((id, message.role.clone(), parts_json))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut message_ids = HashSet::with_capacity(prepared.len());
    if prepared.iter().any(|(id, _, _)| !message_ids.insert(id)) {
        return Err("Agent thread append contains duplicate message IDs".into());
    }

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread append: {e}"))?;
    ensure_thread_access(&mut tx, thread_id, owner_user_id).await?;
    let principal_key = principal_key(owner_user_id);

    // A retry is recognised by its messages, not by a receipt: the ids are
    // fixed before the first attempt, so the rows themselves say whether the
    // append already landed.
    let first_id = prepared[0].0.clone();
    let result_head = prepared[prepared.len() - 1].0.clone();
    if let Some(base_head) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT parent_message_id FROM agent_thread_messages WHERE id = ?",
    )
    .bind(&first_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| format!("Failed to inspect agent thread append retry: {e}"))?
    {
        let messages = load_append_result(
            &mut tx,
            thread_id,
            base_head.as_deref(),
            &first_id,
            &result_head,
            message_count,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit agent thread append retry: {e}"))?;
        return Ok(AgentThreadAppendOutcome::Appended {
            previous_head_message_id: base_head,
            head_message_id: result_head,
            messages,
        });
    }

    let (current_head, current_count) =
        transcript_head_for_connection(&mut tx, thread_id, owner_user_id).await?;
    if current_head != expected_head_message_id {
        tx.commit()
            .await
            .map_err(|e| format!("Failed to finish moved transcript-head read: {e}"))?;
        return Ok(AgentThreadAppendOutcome::HeadMoved {
            expected_head_message_id,
            current_head_message_id: current_head,
        });
    }

    let mut parent = current_head.clone();
    let mut next_depth = current_count;
    let mut appended = Vec::with_capacity(prepared.len());
    for (id, role, parts_json) in prepared {
        let exists =
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM agent_thread_messages WHERE id = ?")
                .bind(&id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| format!("Failed to validate agent message id {id}: {e}"))?;
        if exists.is_some() {
            return Err(format!("Agent message id {id} already exists"));
        }
        sqlx::query(
            "INSERT INTO agent_thread_messages
             (id, uid, principal_key, created_in_thread_id, parent_message_id,
              depth, role, parts_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(owner_user_id)
        .bind(&principal_key)
        .bind(thread_id)
        .bind(parent.as_deref())
        .bind(next_depth)
        .bind(&role)
        .bind(&parts_json)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to write agent thread append: {e}"))?;
        appended.push(load_message_node(&mut tx, thread_id, &id).await?);
        parent = Some(id);
        next_depth = next_depth
            .checked_add(1)
            .ok_or_else(|| "Agent thread message depth overflow".to_owned())?;
    }
    let result_head = parent.expect("non-empty append has a head");
    let moved = sqlx::query(
        "UPDATE agent_thread_transcript_heads
         SET head_message_id = ?, message_count = ?,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
         WHERE thread_id = ? AND uid IS ?
           AND head_message_id IS ? AND message_count = ?",
    )
    .bind(&result_head)
    .bind(next_depth)
    .bind(thread_id)
    .bind(owner_user_id)
    .bind(expected_head_message_id.as_deref())
    .bind(current_count)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to advance agent transcript head: {e}"))?
    .rows_affected();
    if moved != 1 {
        return Err("Agent transcript head moved inside its append transaction".into());
    }
    touch(&mut tx, thread_id, owner_user_id).await?;
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread append: {e}"))?;
    Ok(AgentThreadAppendOutcome::Appended {
        previous_head_message_id: current_head,
        head_message_id: result_head,
        messages: appended,
    })
}

/// The one wording for a lost head CAS, shared by every caller that reports
/// one.
pub(crate) fn transcript_head_moved_error(expected: Option<&str>, current: Option<&str>) -> String {
    format!(
        "Agent transcript changed before append (expected {}, found {}); reload the conversation before retrying",
        expected.unwrap_or("an empty transcript"),
        current.unwrap_or("an empty transcript"),
    )
}

fn with_generated_message_ids(
    mut input: AppendAgentThreadMessagesInput,
) -> AppendAgentThreadMessagesInput {
    for message in &mut input.messages {
        message.id.get_or_insert_with(|| Uuid::new_v4().to_string());
    }
    input
}

async fn load_message_node(
    connection: &mut SqliteConnection,
    thread_id: &str,
    message_id: &str,
) -> Result<AgentThreadMessage, String> {
    sqlx::query_as(
        "SELECT message.id, ? AS thread_id, message.parent_message_id,
                message.depth AS seq, message.role, message.parts_json,
                message.created_at
         FROM agent_thread_messages AS message WHERE message.id = ?",
    )
    .bind(thread_id)
    .bind(message_id)
    .fetch_one(connection)
    .await
    .map_err(|e| format!("Failed to load appended agent message: {e}"))
}

async fn load_append_result(
    connection: &mut SqliteConnection,
    thread_id: &str,
    base_head_message_id: Option<&str>,
    first_message_id: &str,
    result_head_message_id: &str,
    message_count: i64,
) -> Result<Vec<AgentThreadMessage>, String> {
    let expected_count = usize::try_from(message_count)
        .map_err(|_| "Agent thread append receipt has an invalid message count".to_owned())?;
    let mut lineage = sqlx::query_as::<_, AgentThreadMessage>(
        "WITH RECURSIVE lineage(
             id, parent_message_id, depth, role, parts_json, created_at
         ) AS (
             SELECT id, parent_message_id, depth, role, parts_json, created_at
             FROM agent_thread_messages WHERE id = ?
             UNION ALL
             SELECT parent.id, parent.parent_message_id, parent.depth,
                    parent.role, parent.parts_json, parent.created_at
             FROM agent_thread_messages AS parent
             JOIN lineage AS child ON child.parent_message_id = parent.id
         )
         SELECT id, ? AS thread_id, parent_message_id, depth AS seq,
                role, parts_json, created_at
         FROM lineage ORDER BY depth ASC",
    )
    .bind(result_head_message_id)
    .bind(thread_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|e| format!("Failed to load agent thread append result: {e}"))?;
    if lineage.len() < expected_count {
        return Err("Agent thread append receipt points to an incomplete message range".into());
    }
    let messages = lineage.split_off(lineage.len() - expected_count);
    let valid = messages.first().is_some_and(|message| {
        message.id == first_message_id
            && message.parent_message_id.as_deref() == base_head_message_id
    }) && messages
        .last()
        .is_some_and(|message| message.id == result_head_message_id)
        && messages.windows(2).all(|pair| {
            pair[1].parent_message_id.as_deref() == Some(pair[0].id.as_str())
                && pair[1].seq == pair[0].seq + 1
        });
    if !valid {
        return Err("Agent thread append receipt points to a different message chain".into());
    }
    Ok(messages)
}

#[cfg(test)]
async fn append_test_messages(
    pool: &SqlitePool,
    thread_id: &str,
    messages: Vec<NewAgentThreadMessage>,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    let expected_head_message_id = transcript_head(pool, thread_id, owner_user_id)
        .await?
        .head_message_id;
    append_messages(
        pool,
        thread_id,
        AppendAgentThreadMessagesInput {
            operation_id: format!("test-append-{}", Uuid::new_v4()),
            expected_head_message_id,
            messages,
        },
        owner_user_id,
    )
    .await
}

fn validate_append_operation_id(operation_id: &str) -> Result<(), String> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || !operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("Invalid agent thread append operation id".into());
    }
    Ok(())
}

/// Delete a thread, its subagent children, and everything that belongs to
/// them. A delete is a delete: there is no receipt and no `deleting` state to
/// resume from, because nothing downstream has to be told twice.
///
/// Answers with the child thread ids, so the caller can retire their Python
/// workspaces and published runs.
///
/// # Errors
///
/// If the thread does not exist under this principal's admission.
pub async fn delete_thread(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin agent thread deletion: {error}"))?;
    let children: Vec<String> =
        sqlx::query_scalar("SELECT id FROM agent_threads WHERE parent_thread_id = ? AND uid IS ?")
            .bind(thread_id)
            .bind(owner_user_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to list subagent threads: {error}"))?;
    let mut ids = children.clone();
    ids.push(thread_id.to_owned());
    for id in &ids {
        for statement in [
            "DELETE FROM drafts WHERE thread_id = ?",
            "DELETE FROM agent_thread_transcript_heads WHERE thread_id = ?",
            "DELETE FROM agent_thread_runs WHERE thread_id = ?",
            "DELETE FROM agent_thread_usage WHERE thread_id = ?",
        ] {
            sqlx::query(statement)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .map_err(|error| format!("Failed to delete agent thread content: {error}"))?;
        }
        // A fork's prefix *is* the source's nodes. A message some other
        // thread still stands on belongs to that thread now, so it stays.
        sqlx::query(
            "DELETE FROM agent_thread_messages
             WHERE created_in_thread_id = ?
               AND id NOT IN (
                   WITH RECURSIVE lineage(id, parent_message_id) AS (
                       SELECT message.id, message.parent_message_id
                       FROM agent_thread_transcript_heads head
                       JOIN agent_thread_messages message
                         ON message.id = head.head_message_id
                       WHERE head.thread_id <> ?
                       UNION
                       SELECT message.id, message.parent_message_id
                       FROM lineage
                       JOIN agent_thread_messages message
                         ON message.id = lineage.parent_message_id
                   )
                   SELECT id FROM lineage
               )",
        )
        .bind(id)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to delete agent thread messages: {error}"))?;
    }
    let deleted = sqlx::query(
        "DELETE FROM agent_threads
         WHERE (id = ? OR parent_thread_id = ?) AND uid IS ?
           AND uid IS (
               SELECT active_uid FROM auth_write_admission
               WHERE singleton = 1 AND armed = 1 AND accepting = 1
                 AND maintenance = 0 AND remote_writes = 0
           )",
    )
    .bind(thread_id)
    .bind(thread_id)
    .bind(owner_user_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to delete agent thread: {error}"))?
    .rows_affected();
    if deleted == 0 {
        return Err(thread_not_found(thread_id));
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit agent thread deletion: {error}"))?;
    Ok(children)
}

/// What this thread has cost so far, or `None` if nobody has recorded it.
///
/// The one reader an accumulating writer needs: the in-app loop seeds its
/// running total from here at the top of a turn, so the row it later writes is
/// the thread's whole spend and not just this turn's.
///
/// # Errors
///
/// If the query fails.
pub async fn thread_usage(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Option<AgentThreadUsage>, String> {
    sqlx::query_as::<_, AgentThreadUsage>(concat!(
        "SELECT thread_id, model, turns, input_tokens, output_tokens, cache_creation_tokens, ",
        "cache_read_tokens, cost_usd, duration_ms, subagents ",
        "FROM agent_thread_usage WHERE thread_id = ?"
    ))
    .bind(thread_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to read agent thread usage: {error}"))
}

/// Store what a run cost. Absolute — the record replaces whatever was there.
///
/// Not gated on the thread being alive, and deliberately: the out-of-process
/// MCP host deletes its thread when its client hangs up, and the harness that
/// spawned it only learns the price afterwards. The ledger outlives the thread
/// the same way `authored_revisions` does.
///
/// # Errors
///
/// If the upsert fails.
pub async fn record_thread_usage(
    pool: &SqlitePool,
    usage: &AgentThreadUsage,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO agent_thread_usage
             (thread_id, model, turns, input_tokens, output_tokens,
              cache_creation_tokens, cache_read_tokens, cost_usd, duration_ms, subagents)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(thread_id) DO UPDATE SET
             model = excluded.model,
             turns = excluded.turns,
             input_tokens = excluded.input_tokens,
             output_tokens = excluded.output_tokens,
             cache_creation_tokens = excluded.cache_creation_tokens,
             cache_read_tokens = excluded.cache_read_tokens,
             cost_usd = excluded.cost_usd,
             duration_ms = excluded.duration_ms,
             subagents = excluded.subagents,
             recorded_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
    )
    .bind(&usage.thread_id)
    .bind(usage.model.as_deref())
    .bind(usage.turns)
    .bind(usage.input_tokens)
    .bind(usage.output_tokens)
    .bind(usage.cache_creation_tokens)
    .bind(usage.cache_read_tokens)
    .bind(usage.cost_usd)
    .bind(usage.duration_ms)
    .bind(usage.subagents)
    .execute(pool)
    .await
    .map_err(|error| format!("Failed to record agent thread usage: {error}"))?;
    Ok(())
}

/// Rename a thread. `updated_at` is bumped by the table trigger.
pub async fn rename_thread(
    pool: &SqlitePool,
    thread_id: &str,
    title: Option<&str>,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread rename: {e}"))?;
    let result = sqlx::query(
        "UPDATE agent_threads SET title = ?
         WHERE id = ? AND uid IS ? AND lifecycle_state = 'active'
           AND uid IS (
               SELECT active_uid FROM auth_write_admission
               WHERE singleton = 1 AND armed = 1 AND accepting = 1
                 AND maintenance = 0 AND remote_writes = 0
           )",
    )
    .bind(title)
    .bind(thread_id)
    .bind(owner_user_id)
    .execute(&mut *transaction)
    .await
    .map_err(|e| format!("Failed to rename agent thread: {e}"))?;

    if result.rows_affected() == 0 {
        return Err(thread_not_found(thread_id));
    }

    let thread =
        get_thread_row_for_connection(&mut transaction, thread_id, owner_user_id, true).await?;
    transaction
        .commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread rename: {e}"))?;
    Ok(thread)
}

/// Change only this principal's active conversation. Runtime selection is
/// captured when a turn starts, so this preference applies to the next turn.
pub async fn set_thread_selection(
    pool: &SqlitePool,
    thread_id: &str,
    selection: &crate::agent::engine::catalog::Selection,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    selection.validate().map_err(|e| e.to_string())?;
    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    let thread = sqlx::query_as::<_, AgentThread>(sqlx::AssertSqlSafe(format!(
        "UPDATE agent_threads SET engine = ?, model = ?, provider = ?, effort = ?
         WHERE id = ? AND id IN (SELECT thread.id {LIVE_THREADS_FOR_PRINCIPAL})
         RETURNING {THREAD_COLUMNS}"
    )))
    .bind(selection.service.engine().key())
    .bind(&selection.model)
    .bind(selection.service.provider().map(|p| p.as_str()))
    .bind(&selection.effort)
    .bind(thread_id)
    .bind(owner_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| format!("Failed to change agent engine: {error}"))?
    .ok_or_else(|| thread_not_found(thread_id))?;
    sqlx::query("INSERT INTO settings (key, value) VALUES ('agent_selection', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(serde_json::to_string(selection).map_err(|error| error.to_string())?)
        .execute(&mut *tx)
        .await
        .map_err(|error| format!("Failed to save model preference: {error}"))?;
    tx.commit().await.map_err(|error| error.to_string())?;
    Ok(thread)
}

/// Name the writer this thread's revisions are attributed to.
///
/// Restamped rather than set once: the model is chosen per turn, so the turn
/// that just resolved one is the only place that knows the truth. Local only —
/// the label that has to survive is the copy every revision keeps, and this
/// column is just where the next revision reads it from.
///
/// # Errors
///
/// The thread is missing, belongs to another principal, is not active, or the
/// principal is not admitted to write.
pub async fn set_thread_actor(
    pool: &SqlitePool,
    thread_id: &str,
    actor: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let result = sqlx::query(
        "UPDATE agent_threads SET actor = ?
         WHERE id = ? AND uid IS ? AND lifecycle_state = 'active'
           AND uid IS (
               SELECT active_uid FROM auth_write_admission
               WHERE singleton = 1 AND armed = 1 AND accepting = 1
                 AND maintenance = 0 AND remote_writes = 0
           )",
    )
    .bind(actor)
    .bind(thread_id)
    .bind(owner_user_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to set agent thread actor: {e}"))?;
    if result.rows_affected() == 0 {
        return Err(thread_not_found(thread_id));
    }
    Ok(())
}

async fn ensure_thread_access(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    assert_thread_active(&mut *tx, thread_id, owner_user_id).await
}

/// Bump `updated_at` on the parent thread after a message-table mutation. The
/// value written is irrelevant — the `agent_threads_updated_at` trigger stamps
/// the current time — but the UPDATE is what fires it.
async fn touch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let result = sqlx::query(
        "UPDATE agent_threads SET updated_at = updated_at
         WHERE id = ? AND uid IS ? AND lifecycle_state = 'active'
           AND uid IS (
               SELECT active_uid FROM auth_write_admission
               WHERE singleton = 1 AND armed = 1 AND accepting = 1
                 AND maintenance = 0 AND remote_writes = 0
           )",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| format!("Failed to touch agent thread: {e}"))?;

    if result.rows_affected() == 0 {
        return Err(thread_not_found(thread_id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    /// Temp-file pool built the way `init_app_db` builds the real one: migrate
    /// with foreign keys off on a single connection, then reopen with foreign
    /// keys on. A file (not `:memory:`) so multiple connections share state.
    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("luma-test.db");

        let migrate_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .expect("migrate pool");

        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .expect("migrations");
        migrate_pool.close().await;

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .expect("pool");

        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .expect("arm guest admission");

        (dir, pool)
    }

    fn track_thread(subject_id: &str) -> CreateAgentThreadInput {
        CreateAgentThreadInput {
            agent_kind: "track_copilot".into(),
            subject_kind: Some("track".into()),
            subject_id: Some(subject_id.into()),
            venue_id: Some(format!("venue-{subject_id}")),
            score_id: Some(format!("score-{subject_id}")),
            ..Default::default()
        }
    }

    fn msg(role: &str, parts: serde_json::Value) -> NewAgentThreadMessage {
        NewAgentThreadMessage {
            id: None,
            role: role.into(),
            parts,
        }
    }

    async fn admit(pool: &SqlitePool, principal: Option<&str>) {
        crate::database::local::auth::arm_write_admission(pool, principal)
            .await
            .expect("arm test admission");
    }

    #[tokio::test]
    async fn engine_selection_is_owned_and_inherited_without_changing_other_threads() {
        let (_dir, pool) = test_pool().await;
        admit(&pool, Some("alice")).await;
        crate::database::local::settings::update_setting(&pool, "agent_engine", "claude")
            .await
            .unwrap();
        let first = create_thread(&pool, track_thread("track"), Some("alice"))
            .await
            .unwrap();
        let second = create_thread(&pool, track_thread("track"), Some("alice"))
            .await
            .unwrap();
        assert_eq!(first.engine, "claude");
        use crate::agent::engine::catalog::{Selection, Service};
        let selected = Selection {
            service: Service::Codex,
            model: Some("test-model".into()),
            effort: Some("high".into()),
        };
        let invalid = Selection {
            service: Service::OpenRouter,
            model: Some("unknown".into()),
            effort: None,
        };
        set_thread_selection(&pool, &first.id, &selected, Some("alice"))
            .await
            .unwrap();
        assert_eq!(
            get_thread_row(&pool, &second.id, Some("alice"))
                .await
                .unwrap()
                .engine,
            "claude"
        );
        let fork = fork_thread_with_id(
            &pool,
            &Uuid::new_v4().to_string(),
            &first.id,
            None,
            None,
            Some("alice"),
        )
        .await
        .unwrap();
        assert_eq!(fork.engine, "codex");
        assert_eq!(fork.model, selected.model);
        assert_eq!(fork.effort, selected.effort);
        let mut child = track_thread("track");
        child.parent_thread_id = Some(first.id.clone());
        child.parent_call_id = Some("call".into());
        let child = create_thread(&pool, child, Some("alice")).await.unwrap();
        assert_eq!(child.engine, "codex");
        assert_eq!(child.model, selected.model);
        assert_eq!(child.effort, selected.effort);
        assert!(
            set_thread_selection(&pool, &first.id, &invalid, Some("alice"))
                .await
                .is_err()
        );
        admit(&pool, Some("bob")).await;
        assert!(
            set_thread_selection(&pool, &first.id, &selected, Some("bob"))
                .await
                .is_err()
        );
        admit(&pool, Some("alice")).await;
        assert_eq!(
            get_thread_row(&pool, &first.id, Some("alice"))
                .await
                .unwrap()
                .engine,
            "codex"
        );
    }

    #[tokio::test]
    async fn model_preference_survives_restart_and_seeds_other_views() {
        use crate::agent::engine::catalog::{Selection, Service};
        let (dir, pool) = test_pool().await;
        let first = create_thread(&pool, track_thread("first"), None)
            .await
            .unwrap();
        let selected = Selection {
            service: Service::Codex,
            model: Some("test-model".into()),
            effort: Some("high".into()),
        };
        set_thread_selection(&pool, &first.id, &selected, None)
            .await
            .unwrap();
        let other = Selection {
            service: Service::Claude,
            model: None,
            effort: None,
        };
        assert!(set_thread_selection(&pool, "missing", &other, None)
            .await
            .is_err());
        pool.close().await;
        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(dir.path().join("luma-test.db"))
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        let settings = crate::database::local::settings::get_all_settings(&pool)
            .await
            .unwrap();
        assert_eq!(Selection::configured(&settings).unwrap(), selected);
        let next = create_thread(&pool, track_thread("another-view"), None)
            .await
            .unwrap();
        assert_eq!(Selection::from_thread(&next).unwrap(), selected);
    }

    async fn legacy_graph_migration_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE agent_threads (
                id TEXT PRIMARY KEY,
                -- The historical migration replayed below predates the rename
                -- to `uid`, so the schema it runs against spells it the old way.
                owner_user_id TEXT,
                agent_kind TEXT NOT NULL,
                subject_kind TEXT,
                subject_id TEXT,
                venue_id TEXT,
                score_id TEXT,
                title TEXT,
                created_at TEXT NOT NULL DEFAULT '2026-01-01',
                updated_at TEXT NOT NULL DEFAULT '2026-01-01',
                lifecycle_state TEXT NOT NULL DEFAULT 'active'
                    CHECK (lifecycle_state IN ('active', 'deleting'))
             );
             CREATE TABLE agent_thread_messages (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                role TEXT NOT NULL,
                parts_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT '2026-01-01',
                UNIQUE (thread_id, seq)
             );
             CREATE TABLE patterns (
                id TEXT PRIMARY KEY,
                uid TEXT,
                name TEXT NOT NULL,
                description TEXT,
                synced_at TEXT,
                origin TEXT NOT NULL DEFAULT 'local'
             );
             CREATE TABLE implementations (
                id TEXT PRIMARY KEY,
                uid TEXT,
                pattern_id TEXT NOT NULL,
                name TEXT,
                graph_json TEXT NOT NULL DEFAULT '{\"nodes\":[],\"edges\":[],\"args\":[]}',
                created_at TEXT NOT NULL DEFAULT '2026-01-01',
                synced_at TEXT,
                origin TEXT NOT NULL DEFAULT 'local'
             );
             CREATE TABLE venue_implementation_overrides (
                venue_id TEXT NOT NULL,
                pattern_id TEXT NOT NULL,
                implementation_id TEXT NOT NULL,
                PRIMARY KEY (venue_id, pattern_id)
             );",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn run_graph_identity_migration(pool: &SqlitePool) {
        sqlx::raw_sql(include_str!(
            "../../../migrations/20260801300000_agent_thread_graph_implementation.sql"
        ))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn graph_identity_migration_fans_out_an_ambiguous_legacy_transcript() {
        let pool = legacy_graph_migration_pool().await;
        sqlx::raw_sql(
            "INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'Pattern');
             INSERT INTO implementations (id, pattern_id, name, created_at) VALUES
                ('one', 'pattern', NULL, '2026-01-01'),
                ('two', 'pattern', NULL, '2026-01-02');
             INSERT INTO agent_threads
                (id, agent_kind, subject_kind, subject_id, venue_id, title)
             VALUES ('legacy', 'pattern_graph', 'pattern', 'pattern', NULL, 'Old chat');
             INSERT INTO agent_thread_messages
                (id, thread_id, seq, role, parts_json)
             VALUES ('old-message', 'legacy', 0, 'user',
                     '[{\"type\":\"text\",\"text\":\"hello\"}]');",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_graph_identity_migration(&pool).await;

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_threads WHERE id = 'legacy'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        let descendants: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, implementation_id FROM agent_threads ORDER BY implementation_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(descendants.len(), 2);
        assert_eq!(descendants[0].1, "one");
        assert_eq!(descendants[1].1, "two");
        assert_ne!(descendants[0].0, descendants[1].0);

        let messages: Vec<(String, String, i64, String)> = sqlx::query_as(
            "SELECT id, thread_id, seq, parts_json FROM agent_thread_messages ORDER BY thread_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(messages.len(), 2);
        assert_ne!(messages[0].0, messages[1].0);
        assert_eq!(messages[0].2, 0);
        assert_eq!(messages[1].2, 0);
        assert_eq!(messages[0].3, messages[1].3);
        assert!(messages
            .iter()
            .all(|(_, thread_id, _, _)| descendants.iter().any(|(id, _)| id == thread_id)));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_threads
                 WHERE agent_kind = 'pattern_graph' AND implementation_id IS NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn graph_identity_migration_never_synthesizes_an_absent_catalog() {
        let pool = legacy_graph_migration_pool().await;
        sqlx::raw_sql(
            "INSERT INTO agent_threads
                (id, owner_user_id, agent_kind, subject_kind, subject_id, title)
             VALUES ('legacy', NULL, 'pattern_graph', 'pattern', 'missing-pattern', 'Offline chat');
             INSERT INTO agent_thread_messages
                (id, thread_id, seq, role, parts_json)
             VALUES ('old-message', 'legacy', 0, 'user',
                     '[{\"type\":\"text\",\"text\":\"survive\"}]');",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_graph_identity_migration(&pool).await;

        let thread: (String, Option<String>, String) = sqlx::query_as(
            "SELECT subject_id, implementation_id, lifecycle_state
             FROM agent_threads WHERE id = 'legacy'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(thread.0, "missing-pattern");
        assert_eq!(
            thread.1,
            Some("legacy-unmaterialized-6d697373696e672d7061747465726e".into())
        );
        assert_eq!(thread.2, "active");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM implementations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT parts_json FROM agent_thread_messages WHERE id = 'old-message'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "[{\"type\":\"text\",\"text\":\"survive\"}]"
        );
    }

    #[tokio::test]
    async fn principal_isolation_covers_reads_and_mutations() {
        let (_dir, pool) = test_pool().await;
        admit(&pool, Some("alice")).await;
        let alice = create_thread(&pool, track_thread("track-1"), Some("alice"))
            .await
            .unwrap();
        append_test_messages(
            &pool,
            &alice.id,
            vec![msg("user", json!([{"type": "text", "text": "private"}]))],
            Some("alice"),
        )
        .await
        .unwrap();

        admit(&pool, Some("bob")).await;
        let bob = create_thread(&pool, track_thread("track-1"), Some("bob"))
            .await
            .unwrap();

        assert_eq!(alice.owner_user_id.as_deref(), Some("alice"));
        assert_eq!(bob.owner_user_id.as_deref(), Some("bob"));
        assert!(list_threads(&pool, None, None, None, Some("alice"))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            list_threads(&pool, None, None, None, Some("bob"))
                .await
                .unwrap()
                .iter()
                .map(|thread| thread.id.as_str())
                .collect::<Vec<_>>(),
            vec![bob.id.as_str()]
        );
        assert!(list_threads(&pool, None, None, None, None)
            .await
            .unwrap()
            .is_empty());

        for wrong_principal in [Some("alice"), Some("bob"), None] {
            assert!(get_thread_row(&pool, &alice.id, wrong_principal)
                .await
                .is_err());
            assert!(get_thread(&pool, &alice.id, wrong_principal).await.is_err());
            assert!(list_messages(&pool, &alice.id, wrong_principal)
                .await
                .is_err());
            assert!(append_test_messages(
                &pool,
                &alice.id,
                vec![msg("user", json!([]))],
                wrong_principal,
            )
            .await
            .is_err());
        }

        assert!(
            rename_thread(&pool, &alice.id, Some("stolen"), Some("alice"))
                .await
                .is_err()
        );
        assert!(delete_thread(&pool, &alice.id, Some("alice"))
            .await
            .is_err());
        assert!(fork_thread_with_id(
            &pool,
            "cross-principal-fork",
            &alice.id,
            None,
            None,
            Some("bob"),
        )
        .await
        .is_err());

        admit(&pool, Some("alice")).await;
        assert_eq!(
            list_threads(&pool, None, None, None, Some("alice"))
                .await
                .unwrap()
                .iter()
                .map(|thread| thread.id.as_str())
                .collect::<Vec<_>>(),
            vec![alice.id.as_str()]
        );
        let unchanged = get_thread(&pool, &alice.id, Some("alice")).await.unwrap();
        assert_eq!(unchanged.thread.title, None);
        assert_eq!(unchanged.messages.len(), 1);
        let fork = fork_thread_with_id(
            &pool,
            "alice-empty-fork",
            &alice.id,
            None,
            Some("Fork"),
            Some("alice"),
        )
        .await
        .unwrap();
        assert_eq!(fork.owner_user_id.as_deref(), Some("alice"));
        assert!(list_messages(&pool, &fork.id, Some("alice"))
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn legacy_null_threads_belong_only_to_the_signed_out_principal() {
        let (_dir, pool) = test_pool().await;
        sqlx::query(
            "INSERT INTO agent_threads
                (id, agent_kind, subject_kind, subject_id, venue_id, score_id)
             VALUES
                ('legacy-thread', 'track_copilot', 'track', 'track-1', 'venue-1', 'score-1')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let legacy = get_thread_row(&pool, "legacy-thread", None).await.unwrap();
        assert_eq!(legacy.owner_user_id, None);
        assert!(get_thread_row(&pool, "legacy-thread", Some("alice"))
            .await
            .is_err());
        assert_eq!(
            list_threads(&pool, None, None, None, None)
                .await
                .unwrap()
                .iter()
                .map(|thread| thread.id.as_str())
                .collect::<Vec<_>>(),
            vec!["legacy-thread"]
        );
        assert!(list_threads(&pool, None, None, None, Some("alice"))
            .await
            .unwrap()
            .is_empty());
    }

    /// History includes every transcript owned by the principal across subjects.
    #[tokio::test]
    async fn history_walks_transcripts_across_subjects() {
        let (_dir, pool) = test_pool().await;
        let first = create_thread(&pool, track_thread("track-a"), None)
            .await
            .unwrap();
        // A second conversation about the SAME track is its own transcript.
        let second = create_thread(&pool, track_thread("track-a"), None)
            .await
            .unwrap();
        // …an empty one contributes no rows and breaks nothing.
        create_thread(&pool, track_thread("track-a"), None)
            .await
            .unwrap();
        // A conversation about another track belongs to the same history.
        let other = create_thread(&pool, track_thread("track-b"), None)
            .await
            .unwrap();
        // Only one of the same-scope threads gets an assistant turn: a
        // reserved turn claims the scope's authored document, and there is
        // one of those per (track, venue, score).
        for (thread, roles) in [
            (&first, &["user", "assistant"][..]),
            (&second, &["user"][..]),
            (&other, &["user", "assistant"][..]),
        ] {
            let messages = roles
                .iter()
                .map(|role| msg(role, json!([{"type": "text", "text": "said"}])))
                .collect();
            append_test_messages(&pool, &thread.id, messages, None)
                .await
                .unwrap();
        }

        let rows = list_history_messages(&pool, None).await.unwrap();
        let mut by_thread: Vec<(&str, i64, &str)> = rows
            .iter()
            .map(|row| (row.thread_id.as_str(), row.seq, row.role.as_str()))
            .collect();
        by_thread.sort();
        let mut expected = vec![
            (other.id.as_str(), 0, "user"),
            (other.id.as_str(), 1, "assistant"),
            (first.id.as_str(), 0, "user"),
            (first.id.as_str(), 1, "assistant"),
            (second.id.as_str(), 0, "user"),
        ];
        expected.sort();
        assert_eq!(by_thread, expected);
    }

    /// A subject listing is admission-controlled exactly like every other
    /// thread read. This is the assertion that would fail if the shared
    /// predicate were ever reconstructed by hand for this query.
    #[tokio::test]
    async fn history_only_serves_its_own_principal() {
        let (_dir, pool) = test_pool().await;
        admit(&pool, Some("alice")).await;
        let thread = create_thread(&pool, track_thread("track-a"), Some("alice"))
            .await
            .unwrap();
        append_test_messages(
            &pool,
            &thread.id,
            vec![msg("user", json!([{"type": "text", "text": "mine"}]))],
            Some("alice"),
        )
        .await
        .unwrap();

        assert_eq!(
            list_history_messages(&pool, Some("alice"))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            list_history_messages(&pool, Some("bob"))
                .await
                .unwrap()
                .is_empty(),
            "another account's conversations must not be listed"
        );
    }

    #[tokio::test]
    async fn two_threads_for_one_subject_are_independent() {
        let (_dir, pool) = test_pool().await;

        let a = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        let b = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        assert_ne!(a.id, b.id);

        append_test_messages(
            &pool,
            &a.id,
            vec![msg("user", json!([{"type": "text", "text": "a"}]))],
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            get_thread(&pool, &a.id, None).await.unwrap().messages.len(),
            1
        );
        assert_eq!(
            get_thread(&pool, &b.id, None).await.unwrap().messages.len(),
            0
        );

        let listed = list_threads(
            &pool,
            Some("track_copilot"),
            Some("track"),
            Some("track-1"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(list_threads(&pool, Some("pattern_graph"), None, None, None)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn fork_shares_an_exact_prefix_then_diverges_without_copying_nodes() {
        let (_dir, pool) = test_pool().await;
        let source = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        rename_thread(&pool, &source.id, Some("Original"), None)
            .await
            .unwrap();
        append_test_messages(
            &pool,
            &source.id,
            vec![
                NewAgentThreadMessage {
                    id: Some("source-0".into()),
                    role: "user".into(),
                    parts: json!([{"type": "text", "text": "zero"}]),
                },
                NewAgentThreadMessage {
                    id: Some("source-1".into()),
                    role: "user".into(),
                    parts: json!([{"type": "text", "text": "one"}]),
                },
                NewAgentThreadMessage {
                    id: Some("source-2".into()),
                    role: "user".into(),
                    parts: json!([{"type": "text", "text": "two"}]),
                },
            ],
            None,
        )
        .await
        .unwrap();

        let fork = fork_thread_with_id(
            &pool,
            "fork-thread",
            &source.id,
            Some("source-1"),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            fork.forked_from_thread_id.as_deref(),
            Some(source.id.as_str())
        );
        assert_eq!(fork.forked_at_message_id.as_deref(), Some("source-1"));
        assert_eq!(fork.title.as_deref(), Some("Original"));
        assert_eq!(
            list_messages(&pool, &fork.id, None)
                .await
                .unwrap()
                .iter()
                .map(|message| (message.id.as_str(), message.seq))
                .collect::<Vec<_>>(),
            vec![("source-0", 0), ("source-1", 1)]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_thread_messages")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3
        );

        append_test_messages(
            &pool,
            &fork.id,
            vec![NewAgentThreadMessage {
                id: Some("fork-2".into()),
                role: "user".into(),
                parts: json!([{"type": "text", "text": "alternate two"}]),
            }],
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            list_messages(&pool, &source.id, None)
                .await
                .unwrap()
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["source-0", "source-1", "source-2"]
        );
        assert_eq!(
            list_messages(&pool, &fork.id, None)
                .await
                .unwrap()
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["source-0", "source-1", "fork-2"]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_thread_messages")
                .fetch_one(&pool)
                .await
                .unwrap(),
            4
        );

        // Deleting the source takes only what nothing else stands on: the
        // fork keeps the prefix it shares, and loses the node past its cut.
        delete_thread(&pool, &source.id, None).await.unwrap();
        assert_eq!(list_messages(&pool, &fork.id, None).await.unwrap().len(), 3);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_thread_messages")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3
        );
    }

    #[tokio::test]
    async fn fork_rejects_a_cut_point_outside_the_source_lineage() {
        let (_dir, pool) = test_pool().await;
        let source = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        let unrelated = create_thread(&pool, track_thread("track-2"), None)
            .await
            .unwrap();
        append_test_messages(
            &pool,
            &source.id,
            vec![NewAgentThreadMessage {
                id: Some("source-message".into()),
                role: "user".into(),
                parts: json!([]),
            }],
            None,
        )
        .await
        .unwrap();
        append_test_messages(
            &pool,
            &unrelated.id,
            vec![NewAgentThreadMessage {
                id: Some("unrelated-message".into()),
                role: "user".into(),
                parts: json!([]),
            }],
            None,
        )
        .await
        .unwrap();

        let error = fork_thread_with_id(
            &pool,
            "invalid-fork",
            &source.id,
            Some("unrelated-message"),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("not in the source transcript"), "{error}");
        assert!(get_thread_row(&pool, "invalid-fork", None).await.is_err());
    }

    #[tokio::test]
    async fn full_tool_history_survives_round_trip() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();

        // Deliberately exotic: nested objects, arrays, floats, unicode, null,
        // a provider-specific part the backend has never heard of.
        let parts = json!([
            {"type": "reasoning", "text": "thinking…", "state": "done"},
            {"type": "text", "text": "Adding a strobe."},
            {
                "type": "tool-run_python_cell",
                "toolCallId": "call_abc123",
                "state": "output-available",
                "input": {"code": "print('héllo')\n", "scope": {"window": [0.0, 30.5]}},
                "output": {
                    "status": "ok",
                    "stdout": "héllo\n",
                    "figures": [{"artifactId": "a-1", "width": 1200, "height": 400}],
                    "notices": [],
                    "durationMs": 123
                }
            },
            {"type": "x-vendor-thing", "nested": {"a": [1, 2.5, null, false, "ü"]}}
        ]);

        let appended = append_test_messages(
            &pool,
            &thread.id,
            vec![NewAgentThreadMessage {
                id: Some("msg-fixed-id".into()),
                role: "assistant".into(),
                parts: parts.clone(),
            }],
            None,
        )
        .await
        .unwrap();
        assert_eq!(appended[0].id, "msg-fixed-id");

        let loaded = get_thread(&pool, &thread.id, None).await.unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].role, "assistant");
        assert_eq!(loaded.messages[0].seq, 0);
        assert_eq!(loaded.messages[0].parts, parts);
    }

    #[tokio::test]
    async fn transcript_append_is_one_atomic_dense_mutation() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        append_test_messages(
            &pool,
            &thread.id,
            vec![
                NewAgentThreadMessage {
                    id: Some("keep".into()),
                    role: "user".into(),
                    parts: json!([{"type": "text", "text": "keep"}]),
                },
                NewAgentThreadMessage {
                    id: Some("edit".into()),
                    role: "assistant".into(),
                    parts: json!([{"type": "text", "text": "old"}]),
                },
            ],
            None,
        )
        .await
        .unwrap();

        let failed = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-batch-failure".into(),
                expected_head_message_id: Some("edit".into()),
                messages: vec![
                    NewAgentThreadMessage {
                        id: Some("temporary".into()),
                        role: "user".into(),
                        parts: json!([{"type": "text", "text": "must roll back"}]),
                    },
                    NewAgentThreadMessage {
                        id: Some("keep".into()),
                        role: "user".into(),
                        parts: json!([{"type": "text", "text": "different"}]),
                    },
                ],
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(failed.contains("already exists"));
        let unchanged = list_messages(&pool, &thread.id, None).await.unwrap();
        assert_eq!(
            unchanged
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["keep", "edit"]
        );
        assert_eq!(unchanged[1].parts, json!([{"type": "text", "text": "old"}]));

        let appended = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-tail".into(),
                expected_head_message_id: Some("edit".into()),
                messages: vec![
                    NewAgentThreadMessage {
                        id: Some("after-edit".into()),
                        role: "assistant".into(),
                        parts: json!([{"type": "text", "text": "new"}]),
                    },
                    NewAgentThreadMessage {
                        id: Some("next".into()),
                        role: "user".into(),
                        parts: json!([{"type": "text", "text": "next"}]),
                    },
                ],
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            appended.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
        let stored = list_messages(&pool, &thread.id, None).await.unwrap();
        assert_eq!(
            stored.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["keep", "edit", "after-edit", "next"]
        );
        assert_eq!(stored[1].parts, json!([{"type": "text", "text": "old"}]));
    }

    #[tokio::test]
    async fn concurrent_same_head_appends_have_one_atomic_winner() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();

        let mut handles = Vec::new();
        for i in 0..8 {
            let pool = pool.clone();
            let thread_id = thread.id.clone();
            handles.push(tokio::spawn(async move {
                append_messages_at_head(
                    &pool,
                    &thread_id,
                    AppendAgentThreadMessagesInput {
                        operation_id: format!("concurrent-append-{i}"),
                        expected_head_message_id: None,
                        messages: vec![NewAgentThreadMessage {
                            id: Some(format!("concurrent-message-{i}")),
                            role: "user".into(),
                            parts: json!([{"type": "text", "text": format!("q{i}")}]),
                        }],
                    },
                    None,
                )
                .await
                .unwrap()
            }));
        }
        let mut winners = 0;
        let mut losers = 0;
        for handle in handles {
            match handle.await.unwrap() {
                AgentThreadAppendOutcome::Appended { .. } => winners += 1,
                AgentThreadAppendOutcome::HeadMoved {
                    expected_head_message_id,
                    current_head_message_id,
                } => {
                    losers += 1;
                    assert_eq!(expected_head_message_id, None);
                    assert!(current_head_message_id.is_some());
                }
            }
        }
        assert_eq!(winners, 1);
        assert_eq!(losers, 7);

        let messages = list_messages(&pool, &thread.id, None).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].seq, 0);
        let node_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_thread_messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(node_count, 1);
    }

    #[tokio::test]
    async fn rename_sets_and_clears_the_title() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        assert_eq!(thread.title, None);

        let renamed = rename_thread(&pool, &thread.id, Some("Strobe pass"), None)
            .await
            .unwrap();
        assert_eq!(renamed.title.as_deref(), Some("Strobe pass"));

        let cleared = rename_thread(&pool, &thread.id, None, None).await.unwrap();
        assert_eq!(cleared.title, None);
    }
}
