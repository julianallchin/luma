//! Durable agent-thread storage (local-only; see the migration header for why
//! this table is excluded from sync and from `wipe_database`). Every operation
//! receives its trusted principal separately from caller-controlled payloads:
//! `Some(uid)` can access only that owner's rows, while `None` can access only
//! legacy/signed-out rows whose owner is SQL `NULL`.

use sha2::{Digest, Sha256};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::HashSet;
use uuid::Uuid;

use crate::canonical_json;
#[cfg(test)]
use crate::models::agent_threads::NewAgentThreadMessage;
use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, AppendAgentThreadMessagesInput,
    CreateAgentThreadInput,
};

const THREAD_COLUMNS: &str =
    "id, owner_user_id, agent_kind, subject_kind, subject_id, implementation_id, venue_id, score_id, title, created_at, updated_at";
const MESSAGE_COLUMNS: &str = "id, thread_id, seq, role, parts_json, created_at";

fn thread_not_found(thread_id: &str) -> String {
    format!("Agent thread not found: {thread_id}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThreadDeletionTransition {
    Started,
    Resuming,
}

/// Create a thread. The id is always generated here — thread identity is opaque
/// and never supplied by the caller.
#[cfg(test)]
pub async fn create_thread(
    pool: &SqlitePool,
    input: CreateAgentThreadInput,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    let id = Uuid::new_v4().to_string();
    create_thread_with_id(pool, &id, input, owner_user_id).await
}

/// Insert a host-derived thread identity. The authored-state service uses a
/// deterministic ID to close the response-loss window for thread creation.
pub(crate) async fn create_thread_with_id(
    pool: &SqlitePool,
    id: &str,
    input: CreateAgentThreadInput,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    input.authored_route()?;
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread creation: {e}"))?;
    sqlx::query(
        "INSERT INTO agent_threads (id, owner_user_id, agent_kind, subject_kind, subject_id, implementation_id, venue_id, score_id, title)
         SELECT ?, admission.active_uid, ?, ?, ?, ?, ?, ?, ?
         FROM auth_write_admission admission
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
           AND thread.owner_user_id IS admission.active_uid
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

pub(crate) async fn find_thread_row_including_deleting(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Option<AgentThread>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|e| format!("Failed to open agent thread lifecycle read: {e}"))?;
    match get_thread_row_for_connection(&mut connection, thread_id, owner_user_id, false).await {
        Ok(thread) => Ok(Some(thread)),
        Err(error) if error == thread_not_found(thread_id) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Exact-owner terminal receipt for a thread whose lifecycle row has already
/// been removed. `None` is deliberately distinct from an unknown or
/// differently-owned thread ID.
pub(crate) async fn find_thread_deletion_receipt(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Option<String>, String> {
    sqlx::query_scalar(
        "SELECT deletion.repository_id FROM agent_thread_deletions deletion
         CROSS JOIN auth_write_admission admission
         WHERE deletion.thread_id = ? AND deletion.owner_user_id IS ?
           AND admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND deletion.owner_user_id IS admission.active_uid",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to load agent thread deletion receipt: {error}"))
}

pub(crate) async fn insert_thread_deletion_receipt(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
    repository_id: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO agent_thread_deletions (thread_id, owner_user_id, repository_id)
         VALUES (?, ?, ?)",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .bind(repository_id)
    .execute(connection)
    .await
    .map_err(|error| format!("Failed to record agent thread deletion: {error}"))?;
    Ok(())
}

/// Trusted startup maintenance query. Deleting rows are intentionally hidden
/// from every user-facing read, so the host must enumerate them directly to
/// resume cleanup after a crash without relying on remembered UI state.
pub(crate) async fn list_deleting_threads(
    pool: &SqlitePool,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThread>, String> {
    sqlx::query_as::<_, AgentThread>(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM agent_threads thread
         CROSS JOIN auth_write_admission admission
         WHERE thread.lifecycle_state = 'deleting'
           AND thread.owner_user_id IS ?
           AND admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND thread.owner_user_id IS admission.active_uid
         ORDER BY updated_at ASC, id ASC",
        THREAD_COLUMNS
            .split(", ")
            .map(|column| format!("thread.{column}"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
    .bind(owner_user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list deleting agent threads: {e}"))
}

/// Atomically close an active thread to new work. Repeating the transition is
/// how a cleanup retry resumes after a failure or process interruption.
pub(crate) async fn mark_thread_deleting(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<ThreadDeletionTransition, String> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread deletion: {e}"))?;
    let changed = sqlx::query(
        "UPDATE agent_threads SET lifecycle_state = 'deleting'
         WHERE id = ? AND owner_user_id IS ? AND lifecycle_state = 'active'
           AND owner_user_id IS (
               SELECT active_uid FROM auth_write_admission
               WHERE singleton = 1 AND armed = 1 AND accepting = 1
                 AND maintenance = 0 AND remote_writes = 0
           )",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to mark agent thread deleting: {e}"))?
    .rows_affected();

    let transition = if changed == 1 {
        ThreadDeletionTransition::Started
    } else {
        let state = sqlx::query_scalar::<_, String>(
            "SELECT thread.lifecycle_state FROM agent_threads thread
                 CROSS JOIN auth_write_admission admission
                 WHERE thread.id = ? AND thread.owner_user_id IS ?
                   AND admission.singleton = 1 AND admission.armed = 1
                   AND admission.accepting = 1 AND admission.maintenance = 0
                   AND admission.remote_writes = 0
                   AND thread.owner_user_id IS admission.active_uid",
        )
        .bind(thread_id)
        .bind(owner_user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to verify agent thread deletion: {e}"))?;
        match state.as_deref() {
            Some("deleting") => ThreadDeletionTransition::Resuming,
            Some(other) => {
                return Err(format!(
                    "Agent thread {thread_id} has invalid lifecycle state: {other}"
                ));
            }
            None => return Err(thread_not_found(thread_id)),
        }
    };

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread deletion: {e}"))?;
    Ok(transition)
}

/// Transaction-local lifecycle assertion for mutations that project authored
/// state. It prevents another process from publishing a prepared Git commit
/// after deletion has durably begun.
pub(crate) async fn assert_thread_active(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let found = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM agent_threads thread
         CROSS JOIN auth_write_admission admission
         WHERE thread.id = ? AND thread.owner_user_id IS ?
           AND thread.lifecycle_state = 'active'
           AND admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND thread.owner_user_id IS admission.active_uid",
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
    let messages = list_messages_for_connection(&mut transaction, thread_id).await?;
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
    let messages = list_messages_for_connection(&mut transaction, thread_id).await?;
    transaction
        .commit()
        .await
        .map_err(|e| format!("Failed to finish agent message read: {e}"))?;
    Ok(messages)
}

async fn list_messages_for_connection(
    connection: &mut SqliteConnection,
    thread_id: &str,
) -> Result<Vec<AgentThreadMessage>, String> {
    sqlx::query_as::<_, AgentThreadMessage>(sqlx::AssertSqlSafe(format!(
        "SELECT {MESSAGE_COLUMNS} FROM agent_thread_messages
         WHERE thread_id = ? ORDER BY seq ASC"
    )))
    .bind(thread_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|e| format!("Failed to load agent thread messages: {e}"))
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
    let mut sql = format!(
        "SELECT {columns} FROM agent_threads thread
         CROSS JOIN auth_write_admission admission
         WHERE thread.lifecycle_state = 'active'
           AND admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND thread.owner_user_id IS admission.active_uid
           AND admission.active_uid IS ?"
    );
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

/// Atomically append one message batch. The idempotency row is committed
/// beside the messages, so an exact retry after response loss returns the
/// original IDs, seqs, timestamps, and parts without appending the batch twice.
pub async fn append_messages(
    pool: &SqlitePool,
    thread_id: &str,
    input: AppendAgentThreadMessagesInput,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    validate_append_operation_id(&input.operation_id)?;
    if input.messages.is_empty() {
        return Err("Agent thread append must contain at least one message".into());
    }
    let message_count = i64::try_from(input.messages.len())
        .map_err(|_| "Agent thread append contains too many messages".to_owned())?;
    let request_fingerprint = append_request_fingerprint(&input);
    let prepared = input
        .messages
        .into_iter()
        .map(|message| {
            let id = message.id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let parts_json = serde_json::to_string(&message.parts)
                .map_err(|e| format!("Failed to serialize message parts: {e}"))?;
            Ok((id, message.role, parts_json))
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
    if let Some((stored_fingerprint, first_seq, stored_message_count)) =
        sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT request_fingerprint, first_seq, message_count
             FROM agent_thread_message_appends
             WHERE thread_id = ? AND operation_id = ?",
        )
        .bind(thread_id)
        .bind(&input.operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to inspect agent thread append retry: {e}"))?
    {
        if stored_fingerprint != request_fingerprint {
            return Err(format!(
                "Agent thread append operation {} is already bound to different content",
                input.operation_id
            ));
        }
        let result =
            load_append_result(&mut tx, thread_id, first_seq, stored_message_count).await?;
        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit agent thread append retry: {e}"))?;
        return Ok(result);
    }

    let mut next_seq = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(seq) + 1, 0) FROM agent_thread_messages WHERE thread_id = ?",
    )
    .bind(thread_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| format!("Failed to allocate agent thread append seq: {e}"))?;
    let first_seq = next_seq;
    let mut appended = Vec::with_capacity(prepared.len());
    for (id, role, parts_json) in prepared {
        let existing_thread = sqlx::query_scalar::<_, String>(
            "SELECT thread_id FROM agent_thread_messages WHERE id = ?",
        )
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to validate agent message id {id}: {e}"))?;
        if existing_thread.is_some() {
            return Err(format!("Agent message id {id} already exists"));
        }
        sqlx::query(
            "INSERT INTO agent_thread_messages (id, thread_id, seq, role, parts_json)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(next_seq)
        .bind(&role)
        .bind(&parts_json)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to write agent thread append: {e}"))?;
        appended.push(
            sqlx::query_as::<_, AgentThreadMessage>(sqlx::AssertSqlSafe(format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_thread_messages WHERE id = ?"
            )))
            .bind(&id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| format!("Failed to load appended agent message: {e}"))?,
        );
        next_seq = next_seq
            .checked_add(1)
            .ok_or_else(|| "Agent thread message seq overflow".to_owned())?;
    }

    touch(&mut tx, thread_id, owner_user_id).await?;
    sqlx::query(
        "INSERT INTO agent_thread_message_appends
         (thread_id, operation_id, request_fingerprint, first_seq, message_count)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(&input.operation_id)
    .bind(&request_fingerprint)
    .bind(first_seq)
    .bind(message_count)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to record agent thread append: {e}"))?;
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread append: {e}"))?;
    Ok(appended)
}

async fn load_append_result(
    connection: &mut SqliteConnection,
    thread_id: &str,
    first_seq: i64,
    message_count: i64,
) -> Result<Vec<AgentThreadMessage>, String> {
    let end_seq = first_seq
        .checked_add(message_count)
        .ok_or_else(|| "Agent thread append receipt sequence overflow".to_owned())?;
    let expected_count = usize::try_from(message_count)
        .map_err(|_| "Agent thread append receipt has an invalid message count".to_owned())?;
    let messages = sqlx::query_as::<_, AgentThreadMessage>(sqlx::AssertSqlSafe(format!(
        "SELECT {MESSAGE_COLUMNS} FROM agent_thread_messages
         WHERE thread_id = ? AND seq >= ? AND seq < ? ORDER BY seq ASC"
    )))
    .bind(thread_id)
    .bind(first_seq)
    .bind(end_seq)
    .fetch_all(&mut *connection)
    .await
    .map_err(|e| format!("Failed to load agent thread append result: {e}"))?;
    let dense = messages.len() == expected_count
        && messages
            .iter()
            .enumerate()
            .all(|(index, message)| message.seq == first_seq + index as i64);
    if !dense {
        return Err("Agent thread append receipt points to an incomplete message range".into());
    }
    Ok(messages)
}

#[cfg(test)]
async fn append_test_messages(
    pool: &SqlitePool,
    thread_id: &str,
    mut messages: Vec<NewAgentThreadMessage>,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    for message in &mut messages {
        if message.role != "assistant" {
            continue;
        }
        let message_id = message
            .id
            .get_or_insert_with(|| format!("test-assistant-{}", Uuid::new_v4()));
        reserve_test_assistant_turn(pool, thread_id, message_id).await?;
    }
    append_messages(
        pool,
        thread_id,
        AppendAgentThreadMessagesInput {
            operation_id: format!("test-append-{}", Uuid::new_v4()),
            messages,
        },
        owner_user_id,
    )
    .await
}

#[cfg(test)]
async fn reserve_test_assistant_turn(
    pool: &SqlitePool,
    thread_id: &str,
    assistant_message_id: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO authored_state_turn_commits
         (thread_id, assistant_message_id, repository_id, branch_commit)
         VALUES (?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(format!("test-repository-{thread_id}"))
    .bind(format!("test-branch-{}", Uuid::new_v4()))
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("Failed to reserve test assistant turn: {error}"))
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

fn append_request_fingerprint(input: &AppendAgentThreadMessagesInput) -> String {
    let mut hash = Sha256::new();
    hash.update(b"luma.agent-thread-append.v1\0");
    hash.update((input.messages.len() as u64).to_be_bytes());
    for message in &input.messages {
        hash_append_field(&mut hash, message.id.as_deref().unwrap_or(""));
        hash.update([u8::from(message.id.is_some())]);
        hash_append_field(&mut hash, &message.role);
        hash_append_field(&mut hash, &canonical_json::to_string(&message.parts));
    }
    format!("sha256:{:x}", hash.finalize())
}

fn hash_append_field(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value.as_bytes());
}

/// Test-only relational deletion primitive. Production thread deletion must
/// go through `AuthoredDocuments` so Git worktrees and routing state retire as
/// one lifecycle operation. Messages cascade with the row.
#[cfg(test)]
async fn delete_thread(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let result = sqlx::query(
        "DELETE FROM agent_threads
         WHERE id = ? AND owner_user_id IS ?
           AND owner_user_id IS (
               SELECT active_uid FROM auth_write_admission
               WHERE singleton = 1 AND armed = 1 AND accepting = 1
                 AND maintenance = 0 AND remote_writes = 0
           )",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to delete agent thread: {e}"))?;

    if result.rows_affected() == 0 {
        return Err(thread_not_found(thread_id));
    }
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
         WHERE id = ? AND owner_user_id IS ? AND lifecycle_state = 'active'
           AND owner_user_id IS (
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
         WHERE id = ? AND owner_user_id IS ? AND lifecycle_state = 'active'
           AND owner_user_id IS (
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

    async fn legacy_graph_migration_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE agent_threads (
                id TEXT PRIMARY KEY,
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
             VALUES ('old-message', 'legacy', 0, 'user', '[{\"type\":\"text\",\"text\":\"hello\"}]');",
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
             VALUES ('old-message', 'legacy', 0, 'user', '[{\"type\":\"text\",\"text\":\"survive\"}]');",
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

    #[tokio::test]
    async fn authored_thread_routes_are_exact_and_null_safe() {
        let (_dir, pool) = test_pool().await;

        let invalid_track = CreateAgentThreadInput {
            agent_kind: "track_copilot".into(),
            subject_kind: Some("track".into()),
            subject_id: Some("track".into()),
            venue_id: Some("venue".into()),
            score_id: None,
            ..Default::default()
        };
        assert!(create_thread(&pool, invalid_track, None).await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_threads WHERE subject_id = 'track'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );

        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', NULL, 'Pattern')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('implementation', NULL, 'pattern', '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let null_subject_error = sqlx::query(
            "INSERT INTO agent_threads
                (id, agent_kind, subject_kind, subject_id, implementation_id)
             VALUES ('invalid-graph', 'pattern_graph', NULL, 'pattern', 'implementation')",
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(null_subject_error
            .to_string()
            .contains("exact track or pattern authored route"));

        let track = create_thread(&pool, track_thread("valid-track"), None)
            .await
            .unwrap();
        assert_eq!(track.subject_kind.as_deref(), Some("track"));
        let graph = create_thread(
            &pool,
            CreateAgentThreadInput {
                agent_kind: "pattern_graph".into(),
                subject_kind: Some("pattern".into()),
                subject_id: Some("pattern".into()),
                implementation_id: Some("implementation".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(graph.subject_kind.as_deref(), Some("pattern"));
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
    async fn transcript_append_replays_exact_result_and_rejects_operation_rebinding() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        let batch = vec![
            NewAgentThreadMessage {
                id: None,
                role: "user".into(),
                parts: json!([{"type": "text", "text": "question"}]),
            },
            NewAgentThreadMessage {
                id: Some("assistant-fixed".into()),
                role: "assistant".into(),
                parts: json!([{"type": "text", "text": "answer"}]),
            },
        ];
        reserve_test_assistant_turn(&pool, &thread.id, "assistant-fixed")
            .await
            .unwrap();
        let request = AppendAgentThreadMessagesInput {
            operation_id: "append-fixed".into(),
            messages: batch,
        };
        let first = append_messages(&pool, &thread.id, request.clone(), None)
            .await
            .unwrap();
        let retry = append_messages(&pool, &thread.id, request.clone(), None)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&retry).unwrap()
        );
        assert_eq!(
            list_messages(&pool, &thread.id, None).await.unwrap().len(),
            2
        );
        let receipt: (i64, i64) = sqlx::query_as(
            "SELECT first_seq, message_count FROM agent_thread_message_appends
             WHERE thread_id = ? AND operation_id = ?",
        )
        .bind(&thread.id)
        .bind(&request.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(receipt, (0, 2));

        let mismatch = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: request.operation_id,
                messages: vec![NewAgentThreadMessage {
                    id: Some("assistant-fixed".into()),
                    role: "assistant".into(),
                    parts: json!([{"type": "text", "text": "different"}]),
                }],
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(mismatch.contains("different content"));
        assert_eq!(
            list_messages(&pool, &thread.id, None).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn empty_append_does_not_create_a_receipt() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        let error = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "empty-append".into(),
                messages: Vec::new(),
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("at least one message"));
        let receipts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_thread_message_appends WHERE thread_id = ?",
        )
        .bind(&thread.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(receipts, 0);
    }

    #[tokio::test]
    async fn only_prepared_authored_turn_can_claim_an_assistant_message() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO authored_state_turn_commits
             (thread_id, assistant_message_id, repository_id, branch_commit,
              main_commit, status, conflicts_json)
             VALUES
             (?, 'assistant-prepared', 'repo', 'branch-prepared', NULL, 'prepared', NULL),
             (?, 'assistant-committed', 'repo', 'branch-committed', 'main-committed',
              'committed', NULL),
             (?, 'assistant-conflicted', 'repo', 'branch-conflicted', NULL,
              'conflicted', '[{\"path\":\"score.luma\"}]')",
        )
        .bind(&thread.id)
        .bind(&thread.id)
        .bind(&thread.id)
        .execute(&pool)
        .await
        .unwrap();

        let appended = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-prepared-assistant".into(),
                messages: vec![NewAgentThreadMessage {
                    id: Some("assistant-prepared".into()),
                    role: "assistant".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(appended[0].id, "assistant-prepared");

        for id in ["assistant-committed", "assistant-conflicted"] {
            let error = append_messages(
                &pool,
                &thread.id,
                AppendAgentThreadMessagesInput {
                    operation_id: format!("append-{id}"),
                    messages: vec![NewAgentThreadMessage {
                        id: Some(id.into()),
                        role: "assistant".into(),
                        parts: json!([]),
                    }],
                },
                None,
            )
            .await
            .unwrap_err();
            assert!(
                error.contains("assistant message requires a prepared authored turn"),
                "{error}"
            );
        }
        assert_eq!(
            list_messages(&pool, &thread.id, None).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn authored_turn_message_reservations_are_global_and_replay_safe() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        let other = create_thread(&pool, track_thread("track-2"), None)
            .await
            .unwrap();
        let error = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-unreserved-assistant".into(),
                messages: vec![NewAgentThreadMessage {
                    id: Some("unreserved-assistant".into()),
                    role: "assistant".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("assistant message requires a prepared authored turn"),
            "{error}"
        );
        assert!(list_messages(&pool, &thread.id, None)
            .await
            .unwrap()
            .is_empty());

        sqlx::query(
            "INSERT INTO authored_state_turn_commits
             (thread_id, assistant_message_id, repository_id, branch_commit)
             VALUES (?, 'reserved-assistant', 'repo', 'reserved-branch')",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(sqlx::query(
            "INSERT INTO authored_state_turn_commits
             (thread_id, assistant_message_id, repository_id, branch_commit)
             VALUES (?, 'reserved-assistant', 'other-repo', 'other-branch')",
        )
        .bind(&other.id)
        .execute(&pool)
        .await
        .is_err());

        append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-reserved-assistant".into(),
                messages: vec![NewAgentThreadMessage {
                    id: Some("reserved-assistant".into()),
                    role: "assistant".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO authored_state_turn_commits
             (thread_id, assistant_message_id, repository_id, branch_commit)
             VALUES (?, 'reserved-assistant', 'repo', 'reserved-branch')
             ON CONFLICT(thread_id, assistant_message_id) DO NOTHING",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .unwrap();
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

        reserve_test_assistant_turn(&pool, &thread.id, "after-edit")
            .await
            .unwrap();
        let appended = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-tail".into(),
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
    async fn concurrent_appends_assign_dense_contiguous_seqs() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();

        let mut handles = Vec::new();
        for i in 0..8 {
            let pool = pool.clone();
            let thread_id = thread.id.clone();
            handles.push(tokio::spawn(async move {
                append_test_messages(
                    &pool,
                    &thread_id,
                    vec![
                        msg("user", json!([{"type": "text", "text": format!("q{i}")}])),
                        msg(
                            "assistant",
                            json!([{"type": "text", "text": format!("a{i}")}]),
                        ),
                    ],
                    None,
                )
                .await
                .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        let messages = list_messages(&pool, &thread.id, None).await.unwrap();
        assert_eq!(messages.len(), 16);
        let seqs: Vec<i64> = messages.iter().map(|m| m.seq).collect();
        assert_eq!(seqs, (0..16).collect::<Vec<i64>>());

        // Each task's pair must land adjacent — the batch is one transaction.
        for pair in messages.chunks(2) {
            assert_eq!(pair[0].role, "user");
            assert_eq!(pair[1].role, "assistant");
        }
    }

    #[tokio::test]
    async fn active_transcript_and_append_receipts_are_immutable() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();

        let batch: Vec<NewAgentThreadMessage> = (0..5)
            .map(|i| msg("user", json!([{"type": "text", "text": format!("m{i}")}])))
            .collect();
        append_test_messages(&pool, &thread.id, batch, None)
            .await
            .unwrap();

        assert!(sqlx::query(
            "UPDATE agent_thread_messages SET parts_json = '[]'
             WHERE thread_id = ? AND seq = 3",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
        assert!(
            sqlx::query("DELETE FROM agent_thread_messages WHERE thread_id = ? AND seq >= 3",)
                .bind(&thread.id)
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(sqlx::query(
            "UPDATE agent_thread_message_appends SET first_seq = first_seq + 1
             WHERE thread_id = ?",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
        assert!(
            sqlx::query("DELETE FROM agent_thread_message_appends WHERE thread_id = ?",)
                .bind(&thread.id)
                .execute(&pool)
                .await
                .is_err()
        );

        let messages = list_messages(&pool, &thread.id, None).await.unwrap();
        assert_eq!(
            messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[tokio::test]
    async fn delete_cascades_to_messages() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        let other = create_thread(&pool, track_thread("track-2"), None)
            .await
            .unwrap();
        append_test_messages(&pool, &thread.id, vec![msg("user", json!([]))], None)
            .await
            .unwrap();
        append_test_messages(&pool, &other.id, vec![msg("user", json!([]))], None)
            .await
            .unwrap();

        mark_thread_deleting(&pool, &thread.id, None).await.unwrap();
        delete_thread(&pool, &thread.id, None).await.unwrap();

        assert!(get_thread(&pool, &thread.id, None).await.is_err());
        let orphans: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agent_thread_messages WHERE thread_id = ?")
                .bind(&thread.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(orphans.0, 0);
        let receipts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_thread_message_appends WHERE thread_id = ?",
        )
        .bind(&thread.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(receipts, 0);
        assert_eq!(
            list_messages(&pool, &other.id, None).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn deleting_is_terminal_for_normal_thread_operations_and_retryable_for_cleanup() {
        let (_dir, pool) = test_pool().await;
        admit(&pool, Some("alice")).await;
        let thread = create_thread(&pool, track_thread("track-1"), Some("alice"))
            .await
            .unwrap();
        append_test_messages(
            &pool,
            &thread.id,
            vec![msg("user", json!([]))],
            Some("alice"),
        )
        .await
        .unwrap();

        assert_eq!(
            mark_thread_deleting(&pool, &thread.id, Some("alice"))
                .await
                .unwrap(),
            ThreadDeletionTransition::Started
        );
        assert_eq!(
            mark_thread_deleting(&pool, &thread.id, Some("alice"))
                .await
                .unwrap(),
            ThreadDeletionTransition::Resuming
        );
        assert!(get_thread_row(&pool, &thread.id, Some("alice"))
            .await
            .is_err());
        assert_eq!(
            find_thread_row_including_deleting(&pool, &thread.id, Some("alice"))
                .await
                .unwrap()
                .unwrap()
                .id,
            thread.id
        );
        assert!(
            find_thread_row_including_deleting(&pool, &thread.id, Some("bob"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(list_threads(&pool, None, None, None, Some("alice"))
            .await
            .unwrap()
            .is_empty());
        assert!(append_test_messages(
            &pool,
            &thread.id,
            vec![msg("user", json!([]))],
            Some("alice"),
        )
        .await
        .is_err());
        assert!(
            rename_thread(&pool, &thread.id, Some("too late"), Some("alice"))
                .await
                .is_err()
        );
        assert!(
            sqlx::query("UPDATE agent_threads SET lifecycle_state = 'active' WHERE id = ?",)
                .bind(&thread.id)
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(sqlx::query(
            "INSERT INTO authored_state_thread_branches
             (thread_id, repository_id, branch_name) VALUES (?, 'repo', 'agents/threads/test')",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            "INSERT INTO authored_state_turn_commits
             (thread_id, assistant_message_id, repository_id, branch_commit)
             VALUES (?, 'message', 'repo', 'commit')",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            "INSERT INTO authored_state_worktrees
             (worktree_id, request_id, request_fingerprint, repository_id,
              owner_thread_id, branch_name, base_commit)
             VALUES ('worktree', 'request', 'fingerprint', 'repo', ?,
                     'agents/worktrees/test', 'commit')",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
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
