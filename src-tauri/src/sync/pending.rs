//! Pending operations queue for the push path.
//!
//! Mutations write to local SQLite first, then `enqueue()` adds a row to
//! `pending_ops`. A background worker flushes pending ops to Supabase,
//! retrying with exponential backoff on failure.
//!
//! Push order is FK-safe: ops are sorted by their table's topological
//! position in the sync registry before flushing. The queue itself doesn't
//! store the order — it's derived from the registry, the single source of
//! truth for parent/child relationships.

use sqlx::{SqliteConnection, SqlitePool};

use super::authored_remote::{
    ArchiveAuthoredDocumentInput, SubmitHeadProposalInput, ARCHIVE_AUTHORED_DOCUMENT_OP,
    INTEGRATE_HEAD_PROPOSAL_OP, SUBMIT_HEAD_PROPOSAL_OP,
};
use super::error::SyncError;
use super::registry;

const AUTHORED_HEAD_AUTHORITY_TABLE: &str = "authored_head_authority";
pub const INSERT_IMMUTABLE_OP: &str = "insert_immutable";
pub const EXPLICIT_UPSERT_OP: &str = "upsert_explicit";

/// Operations that exceed this many attempts are dead-lettered.
const MAX_ATTEMPTS: i64 = 20;

/// A single pending operation read from the queue.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PendingOp {
    pub id: i64,
    pub principal_key: String,
    pub op_type: String,
    pub table_name: String,
    pub record_id: String,
    pub payload_json: Option<String>,
    pub conflict_key: String,
    pub attempts: i64,
    /// Carried so `PendingOp` maps the whole queue row. Nothing in production
    /// reads it — failed-op reporting was a UI surface that no longer exists.
    #[allow(dead_code)]
    pub last_error: Option<String>,
}

/// Enqueue an upsert operation. If one is already queued for the same
/// (principal, table, record, op_type), the payload is replaced.
///
/// Re-enqueueing the *same* payload keeps the retry state. The dirty sweep
/// re-offers every unsynced row every ten seconds, so resetting `attempts`
/// unconditionally means a row that can never be delivered is retried forever:
/// its counter is zeroed faster than the backoff can grow, and the queue's
/// dead-letter predicate is unreachable. A changed payload is a different
/// operation and does start over.
pub async fn enqueue_upsert(
    pool: &SqlitePool,
    user_id: &str,
    table_name: &str,
    record_id: &str,
    payload_json: &str,
    conflict_key: &str,
) -> Result<(), SyncError> {
    if user_id.is_empty() {
        return Err(SyncError::AuthRequired);
    }
    let table = registry::get_table(table_name).ok_or_else(|| {
        SyncError::Parse(format!(
            "table {table_name:?} is not registered for relational sync"
        ))
    })?;
    if conflict_key != table.conflict_key {
        return Err(SyncError::Parse(format!(
            "sync conflict key {conflict_key:?} does not match {} for table {table_name:?}",
            table.conflict_key
        )));
    }
    let result = sqlx::query(
        "INSERT INTO pending_ops
            (principal_key, op_type, table_name, record_id, payload_json, conflict_key, next_retry_at)
         SELECT 'signed-in:' || admission.active_uid, 'upsert', ?, ?, ?, ?, CURRENT_TIMESTAMP
         FROM auth_write_admission AS admission
         WHERE admission.singleton = 1
           AND admission.armed = 1
           AND admission.accepting = 1
           AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND admission.active_uid = ?
         ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
           payload_json = excluded.payload_json,
           attempts = CASE WHEN pending_ops.payload_json IS excluded.payload_json
                           THEN pending_ops.attempts ELSE 0 END,
           last_error = CASE WHEN pending_ops.payload_json IS excluded.payload_json
                             THEN pending_ops.last_error ELSE NULL END,
           next_retry_at = CASE WHEN pending_ops.payload_json IS excluded.payload_json
                                THEN pending_ops.next_retry_at ELSE CURRENT_TIMESTAMP END",
    )
    .bind(table_name)
    .bind(record_id)
    .bind(payload_json)
    .bind(conflict_key)
    .bind(user_id)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "refusing to enqueue {table_name}.{record_id}: app database admission does not belong to {user_id}"
        )));
    }

    Ok(())
}

/// Enqueue a complete snapshot of mutable state whose delivery receipt lives
/// only in `pending_ops` (currently mutable thread metadata). Transcript heads
/// are server projections advanced by immutable append receipts.
pub async fn enqueue_explicit_upsert_on(
    connection: &mut SqliteConnection,
    user_id: &str,
    table_name: &str,
    record_id: &str,
    payload_json: &str,
    conflict_key: &str,
) -> Result<(), SyncError> {
    let table = registry::get_table(table_name).ok_or_else(|| {
        SyncError::Parse(format!(
            "table {table_name:?} is not registered for relational sync"
        ))
    })?;
    if registry::push_policy(table_name) != registry::PushPolicy::ExplicitUpsert {
        return Err(SyncError::Parse(format!(
            "table {table_name:?} is not registered as explicitly enqueued mutable state"
        )));
    }
    if conflict_key != table.conflict_key {
        return Err(SyncError::Parse(format!(
            "sync conflict key {conflict_key:?} does not match {} for table {table_name:?}",
            table.conflict_key
        )));
    }
    let payload: serde_json::Value =
        serde_json::from_str(payload_json).map_err(|error| SyncError::Parse(error.to_string()))?;
    if !table.payload_principal_matches(&payload, user_id) {
        return Err(SyncError::Local(format!(
            "explicit {table_name}.{record_id} payload is not owned by {user_id}"
        )));
    }
    enqueue_pending_on(
        connection,
        user_id,
        EXPLICIT_UPSERT_OP,
        table_name,
        record_id,
        payload_json,
        conflict_key,
        false,
    )
    .await
}

/// Atomically enqueue an immutable row from the transaction that creates it.
pub async fn enqueue_immutable_on(
    connection: &mut SqliteConnection,
    user_id: &str,
    table_name: &str,
    record_id: &str,
    payload_json: &str,
    conflict_key: &str,
) -> Result<(), SyncError> {
    let table = registry::get_table(table_name).ok_or_else(|| {
        SyncError::Parse(format!(
            "table {table_name:?} is not registered for relational sync"
        ))
    })?;
    if registry::push_policy(table_name) != registry::PushPolicy::ExplicitImmutable {
        return Err(SyncError::Parse(format!(
            "table {table_name:?} is not registered as explicitly enqueued immutable state"
        )));
    }
    if conflict_key != table.conflict_key {
        return Err(SyncError::Parse(format!(
            "sync conflict key {conflict_key:?} does not match {} for table {table_name:?}",
            table.conflict_key
        )));
    }
    let payload: serde_json::Value =
        serde_json::from_str(payload_json).map_err(|error| SyncError::Parse(error.to_string()))?;
    if !table.payload_principal_matches(&payload, user_id) {
        return Err(SyncError::Local(format!(
            "immutable {table_name}.{record_id} payload is not owned by {user_id}"
        )));
    }

    let result = sqlx::query(
        "INSERT INTO pending_ops
            (principal_key, op_type, table_name, record_id, payload_json, conflict_key, next_retry_at)
         SELECT 'signed-in:' || admission.active_uid, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP
         FROM auth_write_admission AS admission
         WHERE admission.singleton = 1 AND admission.armed = 1
           AND admission.accepting = 1 AND admission.maintenance = 0
           AND admission.remote_writes = 0 AND admission.active_uid = ?
         ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
           attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP
         WHERE pending_ops.payload_json = excluded.payload_json
           AND pending_ops.conflict_key = excluded.conflict_key",
    )
    .bind(INSERT_IMMUTABLE_OP)
    .bind(table_name)
    .bind(record_id)
    .bind(payload_json)
    .bind(conflict_key)
    .bind(user_id)
    .execute(connection)
    .await?;
    if result.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "immutable delivery identity {table_name}.{record_id} is already bound to different content or a different principal"
        )));
    }
    Ok(())
}

/// Queue publication of a locally durable, immutable head proposal in the
/// transaction that created it. The proposal id is its replay identity.
pub async fn enqueue_head_proposal_on(
    connection: &mut SqliteConnection,
    user_id: &str,
    input: &SubmitHeadProposalInput,
) -> Result<(), SyncError> {
    enqueue_authority_op_on(
        connection,
        user_id,
        SUBMIT_HEAD_PROPOSAL_OP,
        &input.proposal_id,
        &serde_json::to_string(input).map_err(|error| SyncError::Parse(error.to_string()))?,
    )
    .await
}

/// Queue a durable wake-up for the semantic integration worker. This payload
/// intentionally contains no precomputed expected head or merge result: those
/// would become stale while offline. The worker loads the current server head,
/// computes a typed merge, then calls `integrate_head_proposal` directly.
pub async fn enqueue_head_integration(
    pool: &SqlitePool,
    user_id: &str,
    proposal_id: &str,
) -> Result<(), SyncError> {
    let mut connection = pool.acquire().await?;
    enqueue_head_integration_on(&mut connection, user_id, proposal_id).await
}

pub async fn enqueue_head_integration_on(
    connection: &mut SqliteConnection,
    user_id: &str,
    proposal_id: &str,
) -> Result<(), SyncError> {
    let payload = serde_json::json!({ "proposal_id": proposal_id });
    enqueue_authority_op_on(
        connection,
        user_id,
        INTEGRATE_HEAD_PROPOSAL_OP,
        proposal_id,
        &payload.to_string(),
    )
    .await
}

pub async fn enqueue_authored_archive_on(
    connection: &mut SqliteConnection,
    user_id: &str,
    input: &ArchiveAuthoredDocumentInput,
) -> Result<(), SyncError> {
    enqueue_authority_op_on(
        connection,
        user_id,
        ARCHIVE_AUTHORED_DOCUMENT_OP,
        &input.archive_id,
        &serde_json::to_string(input).map_err(|error| SyncError::Parse(error.to_string()))?,
    )
    .await
}

async fn enqueue_authority_op_on(
    connection: &mut SqliteConnection,
    user_id: &str,
    op_type: &str,
    record_id: &str,
    payload_json: &str,
) -> Result<(), SyncError> {
    if user_id.is_empty() {
        return Err(SyncError::AuthRequired);
    }
    enqueue_pending_on(
        connection,
        user_id,
        op_type,
        AUTHORED_HEAD_AUTHORITY_TABLE,
        record_id,
        payload_json,
        "",
        true,
    )
    .await
}

async fn enqueue_pending_on(
    connection: &mut SqliteConnection,
    user_id: &str,
    op_type: &str,
    table_name: &str,
    record_id: &str,
    payload_json: &str,
    conflict_key: &str,
    immutable_payload: bool,
) -> Result<(), SyncError> {
    if user_id.is_empty() {
        return Err(SyncError::AuthRequired);
    }
    let update = if immutable_payload {
        "attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP
         WHERE pending_ops.payload_json = excluded.payload_json
           AND pending_ops.conflict_key = excluded.conflict_key"
    } else {
        // Same retry-state rule as `enqueue_upsert`: identical content is the
        // same operation and keeps its backoff, new content starts over.
        "payload_json = excluded.payload_json, conflict_key = excluded.conflict_key,
         attempts = CASE WHEN pending_ops.payload_json IS excluded.payload_json
                         THEN pending_ops.attempts ELSE 0 END,
         last_error = CASE WHEN pending_ops.payload_json IS excluded.payload_json
                           THEN pending_ops.last_error ELSE NULL END,
         next_retry_at = CASE WHEN pending_ops.payload_json IS excluded.payload_json
                              THEN pending_ops.next_retry_at ELSE CURRENT_TIMESTAMP END"
    };
    let sql = format!(
        "INSERT INTO pending_ops
            (principal_key, op_type, table_name, record_id, payload_json, conflict_key, next_retry_at)
         SELECT 'signed-in:' || admission.active_uid, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP
         FROM auth_write_admission AS admission
         WHERE admission.singleton = 1
           AND admission.armed = 1
           AND admission.accepting = 1
           AND admission.maintenance = 0
           AND admission.remote_writes = 0
           AND admission.active_uid = ?
         ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET {update}"
    );
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(op_type)
        .bind(table_name)
        .bind(record_id)
        .bind(payload_json)
        .bind(conflict_key)
        .bind(user_id)
        .execute(connection)
        .await?;
    if result.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "authored authority operation {op_type}.{record_id} is already bound to different content or a different principal"
        )));
    }
    Ok(())
}

/// Fetch all ops that are ready to be flushed (next_retry_at <= now),
/// sorted by registry topological position so parents flush before children.
/// Excludes dead-lettered ops (attempts >= MAX_ATTEMPTS). Capped at 1000.
pub async fn fetch_ready_ops(
    pool: &SqlitePool,
    expected_principal_key: &str,
) -> Result<Vec<PendingOp>, SyncError> {
    // Pull a generous window ordered by created_at, then re-sort by topo
    // position in Rust. The window is large enough that a backlog of
    // root-table ops can't be starved by older leaf-table ops, but small
    // enough to bound memory.
    let mut rows = sqlx::query_as::<_, PendingOp>(
        "SELECT op.id, op.principal_key, op.op_type, op.table_name, op.record_id,
                op.payload_json, op.conflict_key, op.attempts, op.last_error
         FROM pending_ops AS op
         JOIN auth_write_admission AS admission
           ON admission.singleton = 1
          AND admission.armed = 1
          AND admission.accepting = 1
          AND admission.maintenance = 0
          AND admission.remote_writes = 0
          AND op.principal_key = CASE
                WHEN admission.active_uid IS NULL THEN 'signed-out'
                ELSE 'signed-in:' || admission.active_uid
              END
         WHERE op.principal_key = ?
           AND op.next_retry_at <= CURRENT_TIMESTAMP
           AND op.attempts < ?
         ORDER BY op.created_at ASC, op.id ASC
         LIMIT 1000",
    )
    .bind(expected_principal_key)
    .bind(MAX_ATTEMPTS)
    .fetch_all(pool)
    .await?;

    rows.sort_by_key(|op| {
        (
            registry::topo_position(&op.table_name).unwrap_or(usize::MAX),
            op.id,
        )
    });
    rows.truncate(100);
    Ok(rows)
}

/// Remove a pending op after successful flush. The operation identity and the
/// live app-database admission must still agree after the remote await.
pub async fn remove_op(pool: &SqlitePool, op: &PendingOp) -> Result<(), SyncError> {
    let result = sqlx::query(
        "DELETE FROM pending_ops
         WHERE id = ? AND principal_key = ?
           AND principal_key = (
               SELECT CASE WHEN admission.active_uid IS NULL
                           THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END
               FROM auth_write_admission AS admission
               WHERE admission.singleton = 1
                 AND admission.armed = 1
                 AND admission.accepting = 1
                 AND admission.maintenance = 0
                 AND admission.remote_writes = 0
           )",
    )
    .bind(op.id)
    .bind(&op.principal_key)
    .execute(pool)
    .await?;
    require_transition(result.rows_affected(), op.id, "remove")
}

/// Record a failed attempt: increment counter, set error, compute next retry.
/// If the op has exceeded MAX_ATTEMPTS, it is dead-lettered (left in the
/// table but never fetched again until manually reset).
pub async fn record_failure(
    pool: &SqlitePool,
    op: &PendingOp,
    new_attempts: i64,
    error_message: &str,
) -> Result<(), SyncError> {
    if new_attempts >= MAX_ATTEMPTS {
        let result = sqlx::query(
            "UPDATE pending_ops SET attempts = ?, last_error = ?
             WHERE id = ? AND principal_key = ?
               AND principal_key = (
                   SELECT CASE WHEN admission.active_uid IS NULL
                               THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END
                   FROM auth_write_admission AS admission
                   WHERE admission.singleton = 1
                     AND admission.armed = 1
                     AND admission.accepting = 1
                     AND admission.maintenance = 0
                     AND admission.remote_writes = 0
               )",
        )
        .bind(new_attempts)
        .bind(error_message)
        .bind(op.id)
        .bind(&op.principal_key)
        .execute(pool)
        .await?;
        require_transition(result.rows_affected(), op.id, "record failure")?;
        eprintln!(
            "[sync] Dead-lettering op {} after {new_attempts} attempts: {error_message}",
            op.id
        );
        return Ok(());
    }

    // Exponential backoff: min(2^attempts * 5, 300) seconds.
    // Clamp shift to avoid overflow on high attempt counts.
    let shift = std::cmp::min(new_attempts, 30);
    let backoff_secs = std::cmp::min(5i64.saturating_mul(1i64 << shift), 300);

    let result = sqlx::query(
        "UPDATE pending_ops SET
           attempts = ?,
           last_error = ?,
           next_retry_at = datetime('now', '+' || ? || ' seconds')
         WHERE id = ? AND principal_key = ?
           AND principal_key = (
               SELECT CASE WHEN admission.active_uid IS NULL
                           THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END
               FROM auth_write_admission AS admission
               WHERE admission.singleton = 1
                 AND admission.armed = 1
                 AND admission.accepting = 1
                 AND admission.maintenance = 0
                 AND admission.remote_writes = 0
           )",
    )
    .bind(new_attempts)
    .bind(error_message)
    .bind(backoff_secs)
    .bind(op.id)
    .bind(&op.principal_key)
    .execute(pool)
    .await?;
    require_transition(result.rows_affected(), op.id, "record failure")
}

/// Integration wake-ups are durable liveness hints, not user-authored writes
/// that may be abandoned after a fixed retry budget. A stale head, an earlier
/// proposal, or a merge closure still uploading must remain retryable until a
/// terminal server receipt exists. Cap the backoff exponent but never reach
/// `MAX_ATTEMPTS`, which is the queue's dead-letter predicate.
pub async fn record_integration_retry(
    pool: &SqlitePool,
    op: &PendingOp,
    error_message: &str,
) -> Result<(), SyncError> {
    if op.op_type != INTEGRATE_HEAD_PROPOSAL_OP {
        return Err(SyncError::Local(format!(
            "pending operation {} is not an authored integration wake-up",
            op.id
        )));
    }
    let attempts = (op.attempts + 1).min(MAX_ATTEMPTS - 1);
    let shift = std::cmp::min(attempts, 30);
    let backoff_secs = std::cmp::min(5i64.saturating_mul(1i64 << shift), 300);
    let result = sqlx::query(
        "UPDATE pending_ops SET attempts = ?, last_error = ?,
             next_retry_at = datetime('now', '+' || ? || ' seconds')
         WHERE id = ? AND principal_key = ?
           AND principal_key = (
               SELECT CASE WHEN admission.active_uid IS NULL
                           THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END
               FROM auth_write_admission AS admission
               WHERE admission.singleton = 1
                 AND admission.armed = 1
                 AND admission.accepting = 1
                 AND admission.maintenance = 0
                 AND admission.remote_writes = 0
           )",
    )
    .bind(attempts)
    .bind(error_message)
    .bind(backoff_secs)
    .bind(op.id)
    .bind(&op.principal_key)
    .execute(pool)
    .await?;
    require_transition(result.rows_affected(), op.id, "reschedule integration")
}

/// Count pending operations for the active principal.
///
/// Test-only: the `get_sync_status` command that used to expose this was
/// deleted with the dispatch port, but the queue-admission behavior it asserts
/// is still worth covering.
#[cfg(test)]
pub async fn count_pending(pool: &SqlitePool) -> Result<i64, SyncError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pending_ops AS op
         JOIN auth_write_admission AS admission
           ON admission.singleton = 1
          AND admission.armed = 1
          AND admission.accepting = 1
          AND admission.maintenance = 0
          AND admission.remote_writes = 0
          AND op.principal_key = CASE
                WHEN admission.active_uid IS NULL THEN 'signed-out'
                ELSE 'signed-in:' || admission.active_uid
              END",
    )
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Failed operations (attempts > 0) for the active principal. Test-only, for
/// the same reason as [`count_pending`].
#[cfg(test)]
pub async fn list_failed(pool: &SqlitePool) -> Result<Vec<PendingOp>, SyncError> {
    let rows = sqlx::query_as::<_, PendingOp>(
        "SELECT op.id, op.principal_key, op.op_type, op.table_name, op.record_id,
                op.payload_json, op.conflict_key, op.attempts, op.last_error
         FROM pending_ops AS op
         JOIN auth_write_admission AS admission
           ON admission.singleton = 1
          AND admission.armed = 1
          AND admission.accepting = 1
          AND admission.maintenance = 0
          AND admission.remote_writes = 0
          AND op.principal_key = CASE
                WHEN admission.active_uid IS NULL THEN 'signed-out'
                ELSE 'signed-in:' || admission.active_uid
              END
         WHERE op.attempts > 0
         ORDER BY op.attempts DESC, op.created_at ASC",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Reset one op's retry timer. Test-only, for the same reason as
/// [`count_pending`].
#[cfg(test)]
pub async fn reset_retry(pool: &SqlitePool, op_id: i64) -> Result<(), SyncError> {
    let result = sqlx::query(
        "UPDATE pending_ops SET attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP
         WHERE id = ?
           AND principal_key = (
               SELECT CASE WHEN admission.active_uid IS NULL
                           THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END
               FROM auth_write_admission AS admission
               WHERE admission.singleton = 1
                 AND admission.armed = 1
                 AND admission.accepting = 1
                 AND admission.maintenance = 0
                 AND admission.remote_writes = 0
           )",
    )
    .bind(op_id)
    .execute(pool)
    .await?;
    require_transition(result.rows_affected(), op_id, "reset")
}

fn require_transition(rows_affected: u64, op_id: i64, action: &str) -> Result<(), SyncError> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(SyncError::Local(format!(
            "refusing to {action} pending operation {op_id}: it is not owned by the active app principal"
        )))
    }
}
