//! Durable agent-thread storage (local-only; see the migration header for why
//! this table is excluded from sync and from `wipe_database`). Every operation
//! receives its trusted principal separately from caller-controlled payloads:
//! `Some(uid)` can access only that owner's rows, while `None` can access only
//! legacy/signed-out rows whose owner is SQL `NULL`.

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, CreateAgentThreadInput,
    NewAgentThreadMessage,
};

const THREAD_COLUMNS: &str =
    "id, owner_user_id, agent_kind, subject_kind, subject_id, venue_id, score_id, title, created_at, updated_at";
const MESSAGE_COLUMNS: &str = "id, thread_id, seq, role, parts_json, created_at";

fn thread_not_found(thread_id: &str) -> String {
    format!("Agent thread not found: {thread_id}")
}

/// Create a thread. The id is always generated here — thread identity is opaque
/// and never supplied by the caller.
pub async fn create_thread(
    pool: &SqlitePool,
    input: CreateAgentThreadInput,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO agent_threads (id, owner_user_id, agent_kind, subject_kind, subject_id, venue_id, score_id, title)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(owner_user_id)
    .bind(&input.agent_kind)
    .bind(&input.subject_kind)
    .bind(&input.subject_id)
    .bind(&input.venue_id)
    .bind(&input.score_id)
    .bind(&input.title)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create agent thread: {}", e))?;

    get_thread_row(pool, &id, owner_user_id).await
}

/// Fetch a thread row without its messages.
pub async fn get_thread_row(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<AgentThread, String> {
    let row = match owner_user_id {
        Some(owner_user_id) => {
            sqlx::query_as::<_, AgentThread>(sqlx::AssertSqlSafe(format!(
                "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE id = ? AND owner_user_id = ?"
            )))
            .bind(thread_id)
            .bind(owner_user_id)
            .fetch_optional(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, AgentThread>(sqlx::AssertSqlSafe(format!(
                "SELECT {THREAD_COLUMNS} FROM agent_threads WHERE id = ? AND owner_user_id IS NULL"
            )))
            .bind(thread_id)
            .fetch_optional(pool)
            .await
        }
    }
    .map_err(|e| format!("Failed to load agent thread: {e}"))?;

    row.ok_or_else(|| thread_not_found(thread_id))
}

/// Fetch a thread together with its full ordered message history.
pub async fn get_thread(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<AgentThreadDetail, String> {
    let thread = get_thread_row(pool, thread_id, owner_user_id).await?;
    let messages = list_messages(pool, thread_id, owner_user_id).await?;
    Ok(AgentThreadDetail { thread, messages })
}

/// All messages of a thread, in seq order.
pub async fn list_messages(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    get_thread_row(pool, thread_id, owner_user_id).await?;

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
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThread>, String> {
    let mut sql = format!("SELECT {THREAD_COLUMNS} FROM agent_threads WHERE ");
    if owner_user_id.is_some() {
        sql.push_str("owner_user_id = ?");
    } else {
        sql.push_str("owner_user_id IS NULL");
    }
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
    if let Some(owner_user_id) = owner_user_id {
        query = query.bind(owner_user_id.to_string());
    }
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
    owner_user_id: Option<&str>,
) -> Result<Vec<AgentThreadMessage>, String> {
    if messages.is_empty() {
        get_thread_row(pool, thread_id, owner_user_id).await?;
        return Ok(Vec::new());
    }

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent message append: {}", e))?;

    ensure_thread_access(&mut tx, thread_id, owner_user_id).await?;

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

    touch(&mut tx, thread_id, owner_user_id).await?;

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
    owner_user_id: Option<&str>,
) -> Result<u64, String> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| format!("Failed to begin agent thread truncate: {}", e))?;

    ensure_thread_access(&mut tx, thread_id, owner_user_id).await?;

    let deleted = sqlx::query("DELETE FROM agent_thread_messages WHERE thread_id = ? AND seq >= ?")
        .bind(thread_id)
        .bind(seq)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to truncate agent thread: {}", e))?
        .rows_affected();

    touch(&mut tx, thread_id, owner_user_id).await?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit agent thread truncate: {}", e))?;

    Ok(deleted)
}

/// Clear a thread's history while keeping the thread row (and therefore its id,
/// and therefore its Python workspace identity — resetting that is the caller's
/// job). Returns the number of messages removed.
pub async fn reset_thread(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<u64, String> {
    truncate_from_seq(pool, thread_id, 0, owner_user_id).await
}

/// Delete a thread. Messages go with it via `ON DELETE CASCADE`.
pub async fn delete_thread(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let result = match owner_user_id {
        Some(owner_user_id) => {
            sqlx::query("DELETE FROM agent_threads WHERE id = ? AND owner_user_id = ?")
                .bind(thread_id)
                .bind(owner_user_id)
                .execute(pool)
                .await
        }
        None => {
            sqlx::query("DELETE FROM agent_threads WHERE id = ? AND owner_user_id IS NULL")
                .bind(thread_id)
                .execute(pool)
                .await
        }
    }
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
    let result = match owner_user_id {
        Some(owner_user_id) => {
            sqlx::query("UPDATE agent_threads SET title = ? WHERE id = ? AND owner_user_id = ?")
                .bind(title)
                .bind(thread_id)
                .bind(owner_user_id)
                .execute(pool)
                .await
        }
        None => {
            sqlx::query("UPDATE agent_threads SET title = ? WHERE id = ? AND owner_user_id IS NULL")
                .bind(title)
                .bind(thread_id)
                .execute(pool)
                .await
        }
    }
    .map_err(|e| format!("Failed to rename agent thread: {e}"))?;

    if result.rows_affected() == 0 {
        return Err(thread_not_found(thread_id));
    }

    get_thread_row(pool, thread_id, owner_user_id).await
}

async fn ensure_thread_access(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let found = match owner_user_id {
        Some(owner_user_id) => {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM agent_threads WHERE id = ? AND owner_user_id = ?",
            )
            .bind(thread_id)
            .bind(owner_user_id)
            .fetch_optional(&mut **tx)
            .await
        }
        None => {
            sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM agent_threads WHERE id = ? AND owner_user_id IS NULL",
            )
            .bind(thread_id)
            .fetch_optional(&mut **tx)
            .await
        }
    }
    .map_err(|e| format!("Failed to authorize agent thread: {e}"))?;

    found.map(|_| ()).ok_or_else(|| thread_not_found(thread_id))
}

/// Bump `updated_at` on the parent thread after a message-table mutation. The
/// value written is irrelevant — the `agent_threads_updated_at` trigger stamps
/// the current time — but the UPDATE is what fires it.
async fn touch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let result = match owner_user_id {
        Some(owner_user_id) => sqlx::query(
            "UPDATE agent_threads SET updated_at = updated_at WHERE id = ? AND owner_user_id = ?",
        )
        .bind(thread_id)
        .bind(owner_user_id)
        .execute(&mut **tx)
        .await,
        None => sqlx::query(
            "UPDATE agent_threads SET updated_at = updated_at WHERE id = ? AND owner_user_id IS NULL",
        )
        .bind(thread_id)
        .execute(&mut **tx)
        .await,
    }
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
    async fn principal_isolation_covers_reads_and_mutations() {
        let (_dir, pool) = test_pool().await;
        let alice = create_thread(&pool, track_thread("track-1"), Some("alice"))
            .await
            .unwrap();
        let bob = create_thread(&pool, track_thread("track-1"), Some("bob"))
            .await
            .unwrap();

        assert_eq!(alice.owner_user_id.as_deref(), Some("alice"));
        assert_eq!(bob.owner_user_id.as_deref(), Some("bob"));
        assert_eq!(
            list_threads(&pool, None, None, None, Some("alice"))
                .await
                .unwrap()
                .iter()
                .map(|thread| thread.id.as_str())
                .collect::<Vec<_>>(),
            vec![alice.id.as_str()]
        );
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

        append_messages(
            &pool,
            &alice.id,
            vec![msg("user", json!([{"type": "text", "text": "private"}]))],
            Some("alice"),
        )
        .await
        .unwrap();

        for wrong_principal in [Some("bob"), None] {
            assert!(get_thread_row(&pool, &alice.id, wrong_principal)
                .await
                .is_err());
            assert!(get_thread(&pool, &alice.id, wrong_principal).await.is_err());
            assert!(list_messages(&pool, &alice.id, wrong_principal)
                .await
                .is_err());
            assert!(
                append_messages(&pool, &alice.id, Vec::new(), wrong_principal)
                    .await
                    .is_err()
            );
        }

        assert!(append_messages(
            &pool,
            &alice.id,
            vec![msg("assistant", json!([]))],
            Some("bob"),
        )
        .await
        .is_err());
        assert!(truncate_from_seq(&pool, &alice.id, 0, Some("bob"))
            .await
            .is_err());
        assert!(reset_thread(&pool, &alice.id, Some("bob")).await.is_err());
        assert!(rename_thread(&pool, &alice.id, Some("stolen"), Some("bob"))
            .await
            .is_err());
        assert!(delete_thread(&pool, &alice.id, Some("bob")).await.is_err());

        let unchanged = get_thread(&pool, &alice.id, Some("alice")).await.unwrap();
        assert_eq!(unchanged.thread.title, None);
        assert_eq!(unchanged.messages.len(), 1);
    }

    #[tokio::test]
    async fn legacy_null_threads_belong_only_to_the_signed_out_principal() {
        let (_dir, pool) = test_pool().await;
        sqlx::query(
            "INSERT INTO agent_threads (id, agent_kind, subject_kind, subject_id)
             VALUES ('legacy-thread', 'track_copilot', 'track', 'track-1')",
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
    async fn two_threads_for_one_subject_are_independent() {
        let (_dir, pool) = test_pool().await;

        let a = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        let b = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        assert_ne!(a.id, b.id);

        append_messages(
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

        let appended = append_messages(
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
    async fn truncate_from_seq_removes_only_the_tail() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();

        let batch: Vec<NewAgentThreadMessage> = (0..5)
            .map(|i| msg("user", json!([{"type": "text", "text": format!("m{i}")}])))
            .collect();
        append_messages(&pool, &thread.id, batch, None)
            .await
            .unwrap();

        let deleted = truncate_from_seq(&pool, &thread.id, 3, None).await.unwrap();
        assert_eq!(deleted, 2);

        let messages = list_messages(&pool, &thread.id, None).await.unwrap();
        assert_eq!(
            messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        // Appending after a truncate continues densely from the new tail.
        append_messages(&pool, &thread.id, vec![msg("user", json!([]))], None)
            .await
            .unwrap();
        let messages = list_messages(&pool, &thread.id, None).await.unwrap();
        assert_eq!(messages.last().unwrap().seq, 3);
    }

    #[tokio::test]
    async fn reset_empties_messages_but_keeps_the_thread() {
        let (_dir, pool) = test_pool().await;
        let thread = create_thread(&pool, track_thread("track-1"), None)
            .await
            .unwrap();
        append_messages(
            &pool,
            &thread.id,
            vec![msg("user", json!([])), msg("assistant", json!([]))],
            None,
        )
        .await
        .unwrap();

        assert_eq!(reset_thread(&pool, &thread.id, None).await.unwrap(), 2);

        let loaded = get_thread(&pool, &thread.id, None).await.unwrap();
        assert_eq!(loaded.thread.id, thread.id);
        assert!(loaded.messages.is_empty());

        // A reset thread restarts its seq at 0.
        append_messages(&pool, &thread.id, vec![msg("user", json!([]))], None)
            .await
            .unwrap();
        assert_eq!(
            list_messages(&pool, &thread.id, None).await.unwrap()[0].seq,
            0
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
        append_messages(&pool, &thread.id, vec![msg("user", json!([]))], None)
            .await
            .unwrap();
        append_messages(&pool, &other.id, vec![msg("user", json!([]))], None)
            .await
            .unwrap();

        delete_thread(&pool, &thread.id, None).await.unwrap();

        assert!(get_thread(&pool, &thread.id, None).await.is_err());
        let orphans: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agent_thread_messages WHERE thread_id = ?")
                .bind(&thread.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(orphans.0, 0);
        assert_eq!(
            list_messages(&pool, &other.id, None).await.unwrap().len(),
            1
        );
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
