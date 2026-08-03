//! Durable principal-bound agent threads and immutable transcript nodes. Every
//! operation receives its trusted principal separately from caller-controlled payloads:
//! `Some(uid)` can access only that owner's rows, while `None` can access only
//! legacy/signed-out rows whose owner is SQL `NULL`.

use sha2::{Digest, Sha256};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::HashSet;
use uuid::Uuid;

use crate::canonical_json;
use crate::database::local::auth::principal_key;
#[cfg(test)]
use crate::models::agent_threads::NewAgentThreadMessage;
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AgentThreadDetail, AgentThreadMessage,
    AgentThreadTranscriptHead, AppendAgentThreadMessagesInput, CreateAgentThreadInput,
};
use crate::sync::pending;

const THREAD_COLUMNS: &str =
    "id, owner_user_id, agent_kind, subject_kind, subject_id, implementation_id, venue_id, score_id, forked_from_thread_id, forked_at_message_id, title, created_at, updated_at";

fn thread_not_found(thread_id: &str) -> String {
    format!("Agent thread not found: {thread_id}")
}

async fn enqueue_thread_snapshot(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let Some(user_id) = owner_user_id else {
        return Ok(());
    };
    let payload: String = sqlx::query_scalar(
        "SELECT json_object(
             'id', id,
             'owner_user_id', owner_user_id,
             'agent_kind', agent_kind,
             'subject_kind', subject_kind,
             'subject_id', subject_id,
             'implementation_id', implementation_id,
             'venue_id', venue_id,
             'score_id', score_id,
             'title', title,
             'lifecycle_state', lifecycle_state,
             'forked_from_thread_id', forked_from_thread_id,
             'forked_at_message_id', forked_at_message_id,
             'created_at', created_at,
             'updated_at', updated_at
         )
         FROM agent_threads WHERE id = ? AND owner_user_id = ?",
    )
    .bind(thread_id)
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| format!("Failed to serialize agent thread for sync: {error}"))?;
    pending::enqueue_explicit_upsert_on(
        connection,
        user_id,
        "agent_threads",
        thread_id,
        &payload,
        "id",
    )
    .await
    .map_err(|error| format!("Failed to enqueue agent thread sync: {error}"))
}

async fn enqueue_message_node(
    connection: &mut SqliteConnection,
    message_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let Some(user_id) = owner_user_id else {
        return Ok(());
    };
    let payload: String = sqlx::query_scalar(
        "SELECT json_object(
             'id', id,
             'owner_user_id', owner_user_id,
             'principal_key', principal_key,
             'created_in_thread_id', created_in_thread_id,
             'parent_message_id', parent_message_id,
             'depth', depth,
             'role', role,
             'parts_json', parts_json,
             'created_at', created_at
         )
         FROM agent_thread_messages
         WHERE id = ? AND owner_user_id = ?",
    )
    .bind(message_id)
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| format!("Failed to serialize agent message for sync: {error}"))?;
    pending::enqueue_immutable_on(
        connection,
        user_id,
        "agent_thread_messages",
        message_id,
        &payload,
        "id",
    )
    .await
    .map_err(|error| format!("Failed to enqueue agent message sync: {error}"))
}

async fn enqueue_append_receipt(
    connection: &mut SqliteConnection,
    thread_id: &str,
    operation_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let Some(user_id) = owner_user_id else {
        return Ok(());
    };
    let payload: String = sqlx::query_scalar(
        "SELECT json_object(
             'thread_id', thread_id,
             'owner_user_id', owner_user_id,
             'principal_key', principal_key,
             'operation_id', operation_id,
             'request_fingerprint', request_fingerprint,
             'base_head_message_id', base_head_message_id,
             'first_message_id', first_message_id,
             'result_head_message_id', result_head_message_id,
             'message_count', message_count,
             'created_at', created_at
         )
         FROM agent_thread_message_appends
         WHERE thread_id = ? AND operation_id = ? AND owner_user_id = ?",
    )
    .bind(thread_id)
    .bind(operation_id)
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| format!("Failed to serialize agent append receipt for sync: {error}"))?;
    pending::enqueue_immutable_on(
        connection,
        user_id,
        "agent_thread_message_appends",
        &format!("{thread_id}:{operation_id}"),
        &payload,
        "thread_id,operation_id",
    )
    .await
    .map_err(|error| format!("Failed to enqueue agent append receipt sync: {error}"))
}

async fn enqueue_deletion_receipt(
    connection: &mut SqliteConnection,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let Some(user_id) = owner_user_id else {
        return Ok(());
    };
    let payload: String = sqlx::query_scalar(
        "SELECT json_object(
             'thread_id', thread_id,
             'owner_user_id', owner_user_id,
             'principal_key', principal_key,
             'document_id', document_id,
             'deleted_at', deleted_at
         )
         FROM agent_thread_deletions
         WHERE thread_id = ? AND owner_user_id = ?",
    )
    .bind(thread_id)
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| format!("Failed to serialize agent thread deletion for sync: {error}"))?;
    pending::enqueue_immutable_on(
        connection,
        user_id,
        "agent_thread_deletions",
        thread_id,
        &payload,
        "thread_id",
    )
    .await
    .map_err(|error| format!("Failed to enqueue agent thread deletion sync: {error}"))
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
    enqueue_thread_snapshot(&mut transaction, id, owner_user_id).await?;
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
         (id, owner_user_id, agent_kind, subject_kind, subject_id,
          implementation_id, venue_id, score_id, forked_from_thread_id,
          forked_at_message_id, title)
         SELECT ?, admission.active_uid, ?, ?, ?, ?, ?, ?, ?, ?, ?
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
    .bind(owner_user_id)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to create agent thread fork: {e}"))?;

    if let Some(message_id) = at_message_id {
        let moved = sqlx::query(
            "UPDATE agent_thread_transcript_heads
             SET head_message_id = ?, message_count = ?,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE thread_id = ? AND owner_user_id IS ?
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
    enqueue_thread_snapshot(connection, new_thread_id, owner_user_id).await?;
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
        "SELECT deletion.document_id FROM agent_thread_deletions deletion
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
    document_id: &str,
) -> Result<bool, String> {
    let principal_key = principal_key(owner_user_id);
    // The deleting projection owns the terminal timestamp. A remote thread
    // row may arrive before its immutable deletion receipt; reusing the
    // server-preserved `updated_at` makes crash/startup recovery synthesize
    // the exact same receipt instead of an immutable identity collision.
    let deleted_at: String = sqlx::query_scalar(
        "SELECT updated_at FROM agent_threads
         WHERE id = ? AND owner_user_id IS ? AND lifecycle_state = 'deleting'",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| format!("Failed to load agent thread deletion timestamp: {error}"))?
    .ok_or_else(|| format!("Agent thread is not deleting: {thread_id}"))?;
    let inserted = sqlx::query(
        "INSERT INTO agent_thread_deletions
         (thread_id, owner_user_id, principal_key, document_id, deleted_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(thread_id) DO NOTHING",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .bind(&principal_key)
    .bind(document_id)
    .bind(&deleted_at)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to record agent thread deletion: {error}"))?;
    if inserted.rows_affected() == 0 {
        let exact = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM agent_thread_deletions
             WHERE thread_id = ? AND owner_user_id IS ?
               AND principal_key = ? AND document_id = ? AND deleted_at = ?",
        )
        .bind(thread_id)
        .bind(owner_user_id)
        .bind(&principal_key)
        .bind(document_id)
        .bind(&deleted_at)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| format!("Failed to verify agent thread deletion retry: {error}"))?
        .is_some();
        if !exact {
            return Err(format!(
                "Agent thread deletion receipt identity collision: {thread_id}"
            ));
        }
    } else {
        enqueue_deletion_receipt(connection, thread_id, owner_user_id).await?;
    }
    Ok(inserted.rows_affected() == 1)
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

    enqueue_thread_snapshot(&mut tx, thread_id, owner_user_id).await?;
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread deletion: {e}"))?;
    Ok(transition)
}

/// Transaction-local lifecycle assertion for mutations that project authored
/// state. It prevents another process from publishing a prepared revision
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
             WHERE head.thread_id = ? AND head.owner_user_id IS ?
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
         WHERE thread_id = ? AND owner_user_id IS ?",
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
         WHERE thread_id = ? AND owner_user_id IS ?",
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
    // Bind the operation to the caller's request before filling omitted IDs.
    // That makes response-loss retries replay the originally generated nodes
    // instead of producing a new fingerprint on every invocation.
    let request_fingerprint = append_request_fingerprint(&input);
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

    if let Some((stored_fingerprint, base_head, first_id, result_head, stored_count)) =
        sqlx::query_as::<_, (String, Option<String>, String, String, i64)>(
            "SELECT request_fingerprint, base_head_message_id, first_message_id,
                    result_head_message_id, message_count
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
        let messages = load_append_result(
            &mut tx,
            thread_id,
            base_head.as_deref(),
            &first_id,
            &result_head,
            stored_count,
        )
        .await?;
        for message in &messages {
            enqueue_message_node(&mut tx, &message.id, owner_user_id).await?;
        }
        enqueue_append_receipt(&mut tx, thread_id, &input.operation_id, owner_user_id).await?;
        enqueue_thread_snapshot(&mut tx, thread_id, owner_user_id).await?;
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

    let first_message_id = prepared[0].0.clone();
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
             (id, owner_user_id, principal_key, created_in_thread_id, parent_message_id,
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
         WHERE thread_id = ? AND owner_user_id IS ?
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
    sqlx::query(
        "INSERT INTO agent_thread_message_appends
         (thread_id, owner_user_id, principal_key, operation_id,
          request_fingerprint, base_head_message_id,
          first_message_id, result_head_message_id, message_count)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(owner_user_id)
    .bind(&principal_key)
    .bind(&input.operation_id)
    .bind(&request_fingerprint)
    .bind(current_head.as_deref())
    .bind(&first_message_id)
    .bind(&result_head)
    .bind(message_count)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to record agent thread append: {e}"))?;
    for message in &appended {
        enqueue_message_node(&mut tx, &message.id, owner_user_id).await?;
    }
    enqueue_append_receipt(&mut tx, thread_id, &input.operation_id, owner_user_id).await?;
    enqueue_thread_snapshot(&mut tx, thread_id, owner_user_id).await?;
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread append: {e}"))?;
    Ok(AgentThreadAppendOutcome::Appended {
        previous_head_message_id: current_head,
        head_message_id: result_head,
        messages: appended,
    })
}

fn transcript_head_moved_error(expected: Option<&str>, current: Option<&str>) -> String {
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

#[cfg(test)]
async fn reserve_test_assistant_turn(
    pool: &SqlitePool,
    thread_id: &str,
    assistant_message_id: &str,
) -> Result<(), String> {
    let (owner, track_id, venue_id, score_id): (Option<String>, String, String, String) =
        sqlx::query_as(
            "SELECT owner_user_id, subject_id, venue_id, score_id
         FROM agent_threads WHERE id = ?",
        )
        .bind(thread_id)
        .fetch_one(pool)
        .await
        .map_err(|error| format!("Failed to load test thread route: {error}"))?;
    let document_id = format!("test-document-{thread_id}");
    let base_revision_id = format!("test-base-{thread_id}");
    let revision_id = format!("test-revision-{assistant_message_id}");
    let principal_key = principal_key(owner.as_deref());
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin test turn reservation: {error}"))?;
    sqlx::query(
        "INSERT INTO authored_documents
         (document_id, document_kind, principal_key, subject_id,
          track_id, venue_id, score_id)
         VALUES (?, 'track_score', ?, ?, ?, ?, ?)
         ON CONFLICT(document_id) DO NOTHING",
    )
    .bind(&document_id)
    .bind(&principal_key)
    .bind(&track_id)
    .bind(&track_id)
    .bind(&venue_id)
    .bind(&score_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("Failed to create test authored document: {error}"))?;
    sqlx::query(
        "INSERT INTO authored_revisions
         (revision_id, document_id, principal_key, parent_count,
          content_hash, operation_kind, message, author_name, author_email, authored_at)
         VALUES (?, ?, ?, 0, ?, 'test_base', 'test base',
                 'Test', 'test@luma.local', '2026-01-01T00:00:00Z')
         ON CONFLICT(principal_key, document_id, revision_id) DO NOTHING",
    )
    .bind(&base_revision_id)
    .bind(&document_id)
    .bind(&principal_key)
    .bind(format!("sha256:base-{thread_id}"))
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("Failed to create test authored base revision: {error}"))?;
    sqlx::query(
        "INSERT INTO authored_revisions
         (revision_id, document_id, principal_key, parent_count,
          content_hash, operation_kind, operation_id,
          message, author_name, author_email, authored_at, thread_id)
         VALUES (?, ?, ?, 1, ?, 'agent_turn_prepare', ?, 'test preparation',
                 'Test', 'test@luma.local', '2026-01-01T00:00:00Z', ?)",
    )
    .bind(&revision_id)
    .bind(&document_id)
    .bind(&principal_key)
    .bind(format!("sha256:{assistant_message_id}"))
    .bind(assistant_message_id)
    .bind(thread_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("Failed to create test authored revision: {error}"))?;
    sqlx::query(
        "INSERT INTO authored_revision_parents
         (principal_key, document_id, revision_id, parent_order, parent_revision_id)
         VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&principal_key)
    .bind(&document_id)
    .bind(&revision_id)
    .bind(&base_revision_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("Failed to parent test authored revision: {error}"))?;
    sqlx::query(
        "INSERT INTO authored_turn_preparations
         (thread_id, assistant_message_id, owner_user_id, principal_key, document_id,
          prepared_revision_id)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(owner.as_deref())
    .bind(&principal_key)
    .bind(&document_id)
    .bind(&revision_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("Failed to reserve test assistant turn: {error}"))?;
    tx.commit()
        .await
        .map_err(|error| format!("Failed to commit test assistant turn: {error}"))
}

#[cfg(test)]
async fn create_test_agent_turn_result(
    pool: &SqlitePool,
    thread_id: &str,
    assistant_message_id: &str,
) -> Result<String, String> {
    let (principal_key, document_id, prepared_revision_id, base_revision_id): (
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT preparation.principal_key, preparation.document_id,
                preparation.prepared_revision_id, parent.parent_revision_id
         FROM authored_turn_preparations preparation
         JOIN authored_revision_parents parent
           ON parent.principal_key = preparation.principal_key
          AND parent.document_id = preparation.document_id
          AND parent.revision_id = preparation.prepared_revision_id
          AND parent.parent_order = 0
         WHERE preparation.thread_id = ? AND preparation.assistant_message_id = ?",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .fetch_one(pool)
    .await
    .map_err(|error| format!("Failed to load test turn preparation: {error}"))?;
    let result_revision_id = format!("test-result-{assistant_message_id}");
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin test turn result: {error}"))?;
    sqlx::query(
        "INSERT INTO authored_revisions
         (revision_id, document_id, principal_key, parent_count, content_hash,
          operation_kind, operation_id, message, author_name, author_email,
          authored_at, thread_id, assistant_message_id)
         VALUES (?, ?, ?, 2, ?, 'agent_turn', ?, 'test assistant result',
                 'Test', 'test@luma.local', '2026-01-01T00:00:00Z', ?, ?)",
    )
    .bind(&result_revision_id)
    .bind(&document_id)
    .bind(&principal_key)
    .bind(format!("sha256:result-{assistant_message_id}"))
    .bind(assistant_message_id)
    .bind(thread_id)
    .bind(assistant_message_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("Failed to create test turn result: {error}"))?;
    for (parent_order, parent_revision_id) in
        [(0_i64, base_revision_id), (1_i64, prepared_revision_id)]
    {
        sqlx::query(
            "INSERT INTO authored_revision_parents
             (principal_key, document_id, revision_id, parent_order, parent_revision_id)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&principal_key)
        .bind(&document_id)
        .bind(&result_revision_id)
        .bind(parent_order)
        .bind(parent_revision_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| format!("Failed to parent test turn result: {error}"))?;
    }
    tx.commit()
        .await
        .map_err(|error| format!("Failed to commit test turn result: {error}"))?;
    Ok(result_revision_id)
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
    // The operation ID names the mutation. Its optimistic base is stored in
    // the immutable receipt, but is intentionally not part of request
    // identity: after a successful append the live head has moved, and an
    // exact retry must still replay the original result.
    hash.update(b"luma.agent-thread-append.v2\0");
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
/// go through `AuthoredDocuments` so execution resources and routing state
/// retire as one lifecycle operation. Immutable transcript nodes and receipts
/// deliberately survive the lifecycle row.
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
    enqueue_thread_snapshot(&mut transaction, thread_id, owner_user_id).await?;
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

    #[tokio::test]
    async fn signed_in_transcript_writes_enqueue_trace_delivery_atomically() {
        let (_dir, pool) = test_pool().await;
        admit(&pool, Some("alice")).await;
        let thread = create_thread(&pool, track_thread("track-1"), Some("alice"))
            .await
            .unwrap();

        let initial_ops: Vec<(String, String)> = sqlx::query_as(
            "SELECT op_type, table_name FROM pending_ops
             WHERE principal_key = 'signed-in:alice'
             ORDER BY table_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            initial_ops,
            vec![("upsert_explicit".into(), "agent_threads".into())]
        );

        append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "sync-trace-append".into(),
                expected_head_message_id: None,
                messages: vec![NewAgentThreadMessage {
                    id: Some("sync-trace-message".into()),
                    role: "user".into(),
                    parts: json!([{"type": "text", "text": "durable"}]),
                }],
            },
            Some("alice"),
        )
        .await
        .unwrap();

        let ops: Vec<(String, String)> = sqlx::query_as(
            "SELECT op_type, table_name FROM pending_ops
             WHERE principal_key = 'signed-in:alice'
             ORDER BY table_name, op_type",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            ops,
            vec![
                (
                    "insert_immutable".into(),
                    "agent_thread_message_appends".into()
                ),
                ("insert_immutable".into(), "agent_thread_messages".into()),
                ("upsert_explicit".into(), "agent_threads".into()),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE table_name = 'agent_thread_transcript_heads'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
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

        mark_thread_deleting(&pool, &source.id, None).await.unwrap();
        delete_thread(&pool, &source.id, None).await.unwrap();
        assert_eq!(list_messages(&pool, &fork.id, None).await.unwrap().len(), 3);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_thread_messages")
                .fetch_one(&pool)
                .await
                .unwrap(),
            4
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
        assert!(
            find_thread_row_including_deleting(&pool, "invalid-fork", None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn transcript_cas_loser_writes_no_nodes_or_receipt() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        append_test_messages(
            &pool,
            &thread.id,
            vec![NewAgentThreadMessage {
                id: Some("base-message".into()),
                role: "user".into(),
                parts: json!([]),
            }],
            None,
        )
        .await
        .unwrap();
        let expected = transcript_head(&pool, &thread.id, None)
            .await
            .unwrap()
            .head_message_id;

        let winner = append_messages_at_head(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "cas-winner".into(),
                expected_head_message_id: expected.clone(),
                messages: vec![NewAgentThreadMessage {
                    id: Some("winner-message".into()),
                    role: "user".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .unwrap();
        assert!(matches!(winner, AgentThreadAppendOutcome::Appended { .. }));

        let loser = append_messages_at_head(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "cas-loser".into(),
                expected_head_message_id: expected,
                messages: vec![NewAgentThreadMessage {
                    id: Some("loser-message".into()),
                    role: "user".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            loser,
            AgentThreadAppendOutcome::HeadMoved {
                expected_head_message_id: Some(ref id),
                current_head_message_id: Some(ref current),
            } if id == "base-message" && current == "winner-message"
        ));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_messages WHERE id = 'loser-message'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_message_appends
                 WHERE thread_id = ? AND operation_id = 'cas-loser'",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );

        let public_error = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "public-stale-loser".into(),
                expected_head_message_id: Some("base-message".into()),
                messages: vec![NewAgentThreadMessage {
                    id: Some("public-stale-message".into()),
                    role: "user".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(public_error.contains("reload the conversation"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_messages WHERE id = 'public-stale-message'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
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
            expected_head_message_id: None,
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
        let receipt: (Option<String>, i64) = sqlx::query_as(
            "SELECT base_head_message_id, message_count FROM agent_thread_message_appends
             WHERE thread_id = ? AND operation_id = ?",
        )
        .bind(&thread.id)
        .bind(&request.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(receipt, (None, 2));

        let mismatch = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: request.operation_id,
                expected_head_message_id: None,
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
                expected_head_message_id: None,
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
        let error = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-unprepared-assistant".into(),
                expected_head_message_id: None,
                messages: vec![NewAgentThreadMessage {
                    id: Some("assistant-unprepared".into()),
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

        reserve_test_assistant_turn(&pool, &thread.id, "assistant-prepared")
            .await
            .unwrap();

        let appended = append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-prepared-assistant".into(),
                expected_head_message_id: None,
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
        reserve_test_assistant_turn(&pool, &thread.id, "reserved-assistant")
            .await
            .unwrap();
        assert!(sqlx::query(
            "INSERT INTO authored_turn_preparations
             (thread_id, assistant_message_id, owner_user_id, principal_key,
              document_id, prepared_revision_id)
             SELECT ?, assistant_message_id, owner_user_id, principal_key,
                    document_id, prepared_revision_id
             FROM authored_turn_preparations
             WHERE thread_id = ? AND assistant_message_id = 'reserved-assistant'",
        )
        .bind(&other.id)
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());

        append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-reserved-assistant".into(),
                expected_head_message_id: None,
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
            "INSERT INTO authored_turn_preparations
             (thread_id, assistant_message_id, owner_user_id, principal_key,
              document_id, prepared_revision_id)
             SELECT thread_id, assistant_message_id, owner_user_id, principal_key,
                    document_id, prepared_revision_id
             FROM authored_turn_preparations
             WHERE thread_id = ? AND assistant_message_id = 'reserved-assistant'
             ON CONFLICT(thread_id, assistant_message_id) DO NOTHING",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn authored_turn_outcome_requires_the_persisted_assistant_and_is_immutable() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        reserve_test_assistant_turn(&pool, &thread.id, "assistant-outcome")
            .await
            .unwrap();
        let result_revision_id =
            create_test_agent_turn_result(&pool, &thread.id, "assistant-outcome")
                .await
                .unwrap();

        let outcome_sql = "INSERT INTO authored_turn_outcomes
             (thread_id, assistant_message_id, owner_user_id, principal_key,
              document_id, prepared_revision_id, status, result_revision_id)
             SELECT thread_id, assistant_message_id, owner_user_id, principal_key,
                    document_id, prepared_revision_id, 'committed', ?
             FROM authored_turn_preparations
             WHERE thread_id = ? AND assistant_message_id = 'assistant-outcome'";
        assert!(sqlx::query(outcome_sql)
            .bind(&result_revision_id)
            .bind(&thread.id)
            .execute(&pool)
            .await
            .is_err());

        append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "append-assistant-outcome".into(),
                expected_head_message_id: None,
                messages: vec![NewAgentThreadMessage {
                    id: Some("assistant-outcome".into()),
                    role: "assistant".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .unwrap();
        sqlx::query(outcome_sql)
            .bind(&result_revision_id)
            .bind(&thread.id)
            .execute(&pool)
            .await
            .unwrap();

        assert!(sqlx::query(
            "UPDATE authored_turn_outcomes SET status = 'conflicted'
             WHERE thread_id = ? AND assistant_message_id = 'assistant-outcome'",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            "DELETE FROM authored_turn_outcomes
             WHERE thread_id = ? AND assistant_message_id = 'assistant-outcome'",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
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

        reserve_test_assistant_turn(&pool, &thread.id, "after-edit")
            .await
            .unwrap();
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
        let receipt_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_thread_message_appends")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(node_count, 1);
        assert_eq!(receipt_count, 1);
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
             WHERE created_in_thread_id = ? AND depth = 3",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            "DELETE FROM agent_thread_messages
                 WHERE created_in_thread_id = ? AND depth >= 3",
        )
        .bind(&thread.id)
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            "UPDATE agent_thread_message_appends
             SET result_head_message_id = first_message_id
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
    async fn immutable_transcript_trace_survives_thread_deletion() {
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
        let nodes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_thread_messages
             WHERE created_in_thread_id = ?",
        )
        .bind(&thread.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(nodes, 1);
        let receipts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_thread_message_appends WHERE thread_id = ?",
        )
        .bind(&thread.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(receipts, 1);
        assert_eq!(
            list_messages(&pool, &other.id, None).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn trusted_pull_can_hydrate_a_complete_trace_after_thread_deletion() {
        let (_dir, pool) = test_pool().await;
        admit(&pool, Some("alice")).await;
        let thread = create_thread(&pool, track_thread("track-1"), Some("alice"))
            .await
            .unwrap();
        reserve_test_assistant_turn(&pool, &thread.id, "remote-assistant")
            .await
            .unwrap();
        let (document_id, revision_id): (String, String) = sqlx::query_as(
            "SELECT document_id, prepared_revision_id
             FROM authored_turn_preparations
             WHERE thread_id = ? AND assistant_message_id = 'remote-assistant'",
        )
        .bind(&thread.id)
        .fetch_one(&pool)
        .await
        .unwrap();

        mark_thread_deleting(&pool, &thread.id, Some("alice"))
            .await
            .unwrap();
        delete_thread(&pool, &thread.id, Some("alice"))
            .await
            .unwrap();

        let mut transaction = pool.begin().await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut transaction)
            .await
            .unwrap();
        assert!(sqlx::query(
            "INSERT INTO agent_thread_messages
             (id, owner_user_id, principal_key, created_in_thread_id,
              parent_message_id, depth, role, parts_json)
             VALUES ('foreign-message', 'bob', 'signed-in:bob', ?,
                     NULL, 0, 'user', '[]')",
        )
        .bind(&thread.id)
        .execute(&mut *transaction)
        .await
        .is_err());
        assert!(sqlx::query(
            "INSERT INTO agent_thread_messages
             (id, owner_user_id, principal_key, created_in_thread_id,
              parent_message_id, depth, role, parts_json)
             VALUES ('orphan-message', 'alice', 'signed-in:alice', ?,
                     'missing-parent', 1, 'user', '[]')",
        )
        .bind(&thread.id)
        .execute(&mut *transaction)
        .await
        .is_err());
        sqlx::query(
            "INSERT INTO agent_thread_messages
             (id, owner_user_id, principal_key, created_in_thread_id,
              parent_message_id, depth, role, parts_json)
             VALUES ('remote-assistant', 'alice', 'signed-in:alice', ?,
                     NULL, 0, 'assistant', '[{\"type\":\"text\",\"text\":\"restored\"}]')",
        )
        .bind(&thread.id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_thread_message_appends
             (thread_id, owner_user_id, principal_key, operation_id,
              request_fingerprint, base_head_message_id, first_message_id,
              result_head_message_id, message_count)
             VALUES (?, 'alice', 'signed-in:alice', 'remote-operation',
                     'sha256:remote', NULL, 'remote-assistant',
                     'remote-assistant', 1)",
        )
        .bind(&thread.id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        let result_revision_id = "test-result-remote-assistant";
        sqlx::query(
            "INSERT INTO authored_revisions
             (revision_id, document_id, principal_key, parent_count,
              content_hash, operation_kind, operation_id, message,
              author_name, author_email, authored_at, thread_id,
              assistant_message_id)
             VALUES (?, ?, 'signed-in:alice', 2, 'sha256:remote-result',
                     'agent_turn', 'remote-assistant', 'test result',
                     'Test', 'test@luma.local', '2026-01-01T00:00:01Z',
                     ?, 'remote-assistant')",
        )
        .bind(result_revision_id)
        .bind(&document_id)
        .bind(&thread.id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO authored_revision_parents
             (principal_key, document_id, revision_id, parent_order, parent_revision_id)
             SELECT 'signed-in:alice', ?, ?, 0, parent_revision_id
             FROM authored_revision_parents
             WHERE revision_id = ? AND parent_order = 0",
        )
        .bind(&document_id)
        .bind(result_revision_id)
        .bind(&revision_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO authored_revision_parents
             (principal_key, document_id, revision_id, parent_order, parent_revision_id)
             VALUES ('signed-in:alice', ?, ?, 1, ?)",
        )
        .bind(&document_id)
        .bind(result_revision_id)
        .bind(&revision_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO authored_turn_outcomes
             (thread_id, assistant_message_id, owner_user_id, principal_key,
              document_id, prepared_revision_id, status, result_revision_id)
             VALUES (?, 'remote-assistant', 'alice', 'signed-in:alice',
                     ?, ?, 'committed', ?)",
        )
        .bind(&thread.id)
        .bind(&document_id)
        .bind(&revision_id)
        .bind(result_revision_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_thread_deletions
             (thread_id, owner_user_id, principal_key, document_id)
             VALUES (?, 'alice', 'signed-in:alice', ?)",
        )
        .bind(&thread.id)
        .bind(&document_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_messages
                 WHERE id = 'remote-assistant' AND owner_user_id = 'alice'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM authored_turn_outcomes
                 WHERE assistant_message_id = 'remote-assistant'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_deletions WHERE thread_id = ?",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
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
        assert!(
            reserve_test_assistant_turn(&pool, &thread.id, "too-late-assistant")
                .await
                .is_err()
        );
        assert!(fork_thread_with_id(
            &pool,
            "too-late-fork",
            &thread.id,
            None,
            None,
            Some("alice"),
        )
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
