//! Durable agent-thread storage (local-only; see the migration header for why
//! this table is excluded from sync and from `wipe_database`).

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, CreateAgentThreadInput,
    NewAgentThreadMessage,
};

const THREAD_COLUMNS: &str =
    "id, agent_kind, subject_kind, subject_id, venue_id, score_id, title, created_at, updated_at";
const MESSAGE_COLUMNS: &str = "id, thread_id, seq, role, parts_json, created_at";

/// Create a thread. The id is always generated here — thread identity is opaque
/// and never supplied by the caller.
pub async fn create_thread(
    pool: &SqlitePool,
    input: CreateAgentThreadInput,
) -> Result<AgentThread, String> {
    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO agent_threads (id, agent_kind, subject_kind, subject_id, venue_id, score_id, title)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.agent_kind)
    .bind(&input.subject_kind)
    .bind(&input.subject_id)
    .bind(&input.venue_id)
    .bind(&input.score_id)
    .bind(&input.title)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create agent thread: {}", e))?;

    get_thread_row(pool, &id).await
}

/// Fetch a thread row without its messages.
pub async fn get_thread_row(pool: &SqlitePool, thread_id: &str) -> Result<AgentThread, String> {
    sqlx::query_as::<_, AgentThread>(sqlx::AssertSqlSafe(format!(
        "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE id = ?"
    )))
    .bind(thread_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load agent thread: {}", e))?
    .ok_or_else(|| format!("Agent thread not found: {}", thread_id))
}

/// Fetch a thread together with its full ordered message history.
pub async fn get_thread(pool: &SqlitePool, thread_id: &str) -> Result<AgentThreadDetail, String> {
    let thread = get_thread_row(pool, thread_id).await?;
    let messages = list_messages(pool, thread_id).await?;
    Ok(AgentThreadDetail { thread, messages })
}

/// All messages of a thread, in seq order.
pub async fn list_messages(
    pool: &SqlitePool,
    thread_id: &str,
) -> Result<Vec<AgentThreadMessage>, String> {
    sqlx::query_as::<_, AgentThreadMessage>(sqlx::AssertSqlSafe(format!(
        "SELECT {MESSAGE_COLUMNS} FROM agent_thread_messages WHERE thread_id = ? ORDER BY seq ASC"
    )))
    .bind(thread_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load agent thread messages: {}", e))
}

/// List threads, most recently updated first. Both filters are optional and
/// independent: `agent_kind` narrows by agent, `subject` by (kind, id) pair.
pub async fn list_threads(
    pool: &SqlitePool,
    agent_kind: Option<&str>,
    subject_kind: Option<&str>,
    subject_id: Option<&str>,
) -> Result<Vec<AgentThread>, String> {
    let mut sql = format!("SELECT {THREAD_COLUMNS} FROM agent_threads WHERE 1 = 1");
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
    for value in [agent_kind, subject_kind, subject_id].into_iter().flatten() {
        query = query.bind(value.to_string());
    }

    query
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list agent threads: {}", e))
}

/// Append messages to the tail of a thread.
///
/// `seq` is assigned by the database inside a write transaction (`BEGIN
/// IMMEDIATE` + a `MAX(seq) + 1` subquery in the INSERT itself), so concurrent
/// appenders can never read the same high-water mark. Callers must not compute
/// or pass a seq; `UNIQUE (thread_id, seq)` is the backstop.
pub async fn append_messages(
    pool: &SqlitePool,
    thread_id: &str,
    messages: Vec<NewAgentThreadMessage>,
) -> Result<Vec<AgentThreadMessage>, String> {
    if messages.is_empty() {
        return Ok(Vec::new());
    }

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent message append: {}", e))?;

    let mut ids: Vec<String> = Vec::with_capacity(messages.len());

    for message in messages {
        let id = message.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let parts_json = serde_json::to_string(&message.parts)
            .map_err(|e| format!("Failed to serialize message parts: {}", e))?;

        sqlx::query(
            "INSERT INTO agent_thread_messages (id, thread_id, seq, role, parts_json)
             SELECT ?, ?, COALESCE((SELECT MAX(seq) + 1 FROM agent_thread_messages WHERE thread_id = ?), 0), ?, ?",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(thread_id)
        .bind(&message.role)
        .bind(&parts_json)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to append agent thread message: {}", e))?;

        ids.push(id);
    }

    touch(&mut tx, thread_id).await?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent message append: {}", e))?;

    let mut appended = Vec::with_capacity(ids.len());
    for id in ids {
        appended.push(
            sqlx::query_as::<_, AgentThreadMessage>(sqlx::AssertSqlSafe(format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_thread_messages WHERE id = ?"
            )))
            .bind(&id)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Failed to reload appended message: {}", e))?,
        );
    }
    Ok(appended)
}

/// Drop the tail of a thread from `seq` (inclusive) — the edit-and-resend path.
/// Returns the number of messages removed.
pub async fn truncate_from_seq(
    pool: &SqlitePool,
    thread_id: &str,
    seq: i64,
) -> Result<u64, String> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread truncate: {}", e))?;

    let deleted = sqlx::query("DELETE FROM agent_thread_messages WHERE thread_id = ? AND seq >= ?")
        .bind(thread_id)
        .bind(seq)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to truncate agent thread: {}", e))?
        .rows_affected();

    touch(&mut tx, thread_id).await?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread truncate: {}", e))?;

    Ok(deleted)
}

/// Clear a thread's history while keeping the thread row (and therefore its id,
/// and therefore its Python workspace identity — resetting that is the caller's
/// job). Returns the number of messages removed.
pub async fn reset_thread(pool: &SqlitePool, thread_id: &str) -> Result<u64, String> {
    truncate_from_seq(pool, thread_id, 0).await
}

/// Delete a thread. Messages go with it via `ON DELETE CASCADE`.
pub async fn delete_thread(pool: &SqlitePool, thread_id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM agent_threads WHERE id = ?")
        .bind(thread_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete agent thread: {}", e))?;
    Ok(())
}

/// Rename a thread. `updated_at` is bumped by the table trigger.
pub async fn rename_thread(
    pool: &SqlitePool,
    thread_id: &str,
    title: Option<&str>,
) -> Result<AgentThread, String> {
    sqlx::query("UPDATE agent_threads SET title = ? WHERE id = ?")
        .bind(title)
        .bind(thread_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to rename agent thread: {}", e))?;

    get_thread_row(pool, thread_id).await
}

/// Bump `updated_at` on the parent thread after a message-table mutation. The
/// value written is irrelevant — the `agent_threads_updated_at` trigger stamps
/// the current time — but the UPDATE is what fires it.
async fn touch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
) -> Result<(), String> {
    sqlx::query("UPDATE agent_threads SET updated_at = updated_at WHERE id = ?")
        .bind(thread_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| format!("Failed to touch agent thread: {}", e))?;
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

        (dir, pool)
    }

    fn track_thread(subject_id: &str) -> CreateAgentThreadInput {
        CreateAgentThreadInput {
            agent_kind: "track_copilot".into(),
            subject_kind: Some("track".into()),
            subject_id: Some(subject_id.into()),
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

    #[tokio::test]
    async fn two_threads_for_one_subject_are_independent() {
        let (_dir, pool) = test_pool().await;

        let a = create_thread(&pool, track_thread("track-1")).await.unwrap();
        let b = create_thread(&pool, track_thread("track-1")).await.unwrap();
        assert_ne!(a.id, b.id);

        append_messages(
            &pool,
            &a.id,
            vec![msg("user", json!([{"type": "text", "text": "a"}]))],
        )
        .await
        .unwrap();

        assert_eq!(get_thread(&pool, &a.id).await.unwrap().messages.len(), 1);
        assert_eq!(get_thread(&pool, &b.id).await.unwrap().messages.len(), 0);

        let listed = list_threads(&pool, Some("track_copilot"), Some("track"), Some("track-1"))
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(list_threads(&pool, Some("pattern_graph"), None, None)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn full_tool_history_survives_round_trip() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1")).await.unwrap();

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

        let appended = append_messages(
            &pool,
            &thread.id,
            vec![NewAgentThreadMessage {
                id: Some("msg-fixed-id".into()),
                role: "assistant".into(),
                parts: parts.clone(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(appended[0].id, "msg-fixed-id");

        let loaded = get_thread(&pool, &thread.id).await.unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].role, "assistant");
        assert_eq!(loaded.messages[0].seq, 0);
        assert_eq!(loaded.messages[0].parts, parts);
    }

    #[tokio::test]
    async fn concurrent_appends_assign_dense_contiguous_seqs() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1")).await.unwrap();

        let mut handles = Vec::new();
        for i in 0..8 {
            let pool = pool.clone();
            let thread_id = thread.id.clone();
            handles.push(tokio::spawn(async move {
                append_messages(
                    &pool,
                    &thread_id,
                    vec![
                        msg("user", json!([{"type": "text", "text": format!("q{i}")}])),
                        msg(
                            "assistant",
                            json!([{"type": "text", "text": format!("a{i}")}]),
                        ),
                    ],
                )
                .await
                .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        let messages = list_messages(&pool, &thread.id).await.unwrap();
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
    async fn truncate_from_seq_removes_only_the_tail() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1")).await.unwrap();

        let batch: Vec<NewAgentThreadMessage> = (0..5)
            .map(|i| msg("user", json!([{"type": "text", "text": format!("m{i}")}])))
            .collect();
        append_messages(&pool, &thread.id, batch).await.unwrap();

        let deleted = truncate_from_seq(&pool, &thread.id, 3).await.unwrap();
        assert_eq!(deleted, 2);

        let messages = list_messages(&pool, &thread.id).await.unwrap();
        assert_eq!(
            messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        // Appending after a truncate continues densely from the new tail.
        append_messages(&pool, &thread.id, vec![msg("user", json!([]))])
            .await
            .unwrap();
        let messages = list_messages(&pool, &thread.id).await.unwrap();
        assert_eq!(messages.last().unwrap().seq, 3);
    }

    #[tokio::test]
    async fn reset_empties_messages_but_keeps_the_thread() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1")).await.unwrap();
        append_messages(
            &pool,
            &thread.id,
            vec![msg("user", json!([])), msg("assistant", json!([]))],
        )
        .await
        .unwrap();

        assert_eq!(reset_thread(&pool, &thread.id).await.unwrap(), 2);

        let loaded = get_thread(&pool, &thread.id).await.unwrap();
        assert_eq!(loaded.thread.id, thread.id);
        assert!(loaded.messages.is_empty());

        // A reset thread restarts its seq at 0.
        append_messages(&pool, &thread.id, vec![msg("user", json!([]))])
            .await
            .unwrap();
        assert_eq!(list_messages(&pool, &thread.id).await.unwrap()[0].seq, 0);
    }

    #[tokio::test]
    async fn delete_cascades_to_messages() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1")).await.unwrap();
        let other = create_thread(&pool, track_thread("track-2")).await.unwrap();
        append_messages(&pool, &thread.id, vec![msg("user", json!([]))])
            .await
            .unwrap();
        append_messages(&pool, &other.id, vec![msg("user", json!([]))])
            .await
            .unwrap();

        delete_thread(&pool, &thread.id).await.unwrap();

        assert!(get_thread(&pool, &thread.id).await.is_err());
        let orphans: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agent_thread_messages WHERE thread_id = ?")
                .bind(&thread.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(orphans.0, 0);
        assert_eq!(list_messages(&pool, &other.id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rename_sets_and_clears_the_title() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1")).await.unwrap();
        assert_eq!(thread.title, None);

        let renamed = rename_thread(&pool, &thread.id, Some("Strobe pass"))
            .await
            .unwrap();
        assert_eq!(renamed.title.as_deref(), Some("Strobe pass"));

        let cleared = rename_thread(&pool, &thread.id, None).await.unwrap();
        assert_eq!(cleared.title, None);
    }
}
