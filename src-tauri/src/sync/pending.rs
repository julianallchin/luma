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

use sqlx::SqlitePool;

use super::error::SyncError;
use super::registry;

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
    pub last_error: Option<String>,
}

/// Enqueue an upsert operation. If one is already queued for the same
/// (principal, table, record, op_type), the payload is replaced.
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
           attempts = 0,
           last_error = NULL,
           next_retry_at = CURRENT_TIMESTAMP",
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
         ORDER BY op.created_at ASC
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

/// Count pending operations (for status reporting).
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

/// List failed operations (attempts > 0) for UI display.
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

/// Reset retry timer on a specific op (manual retry from UI).
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
