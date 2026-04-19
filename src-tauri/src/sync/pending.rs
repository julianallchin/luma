//! Pending operations queue for the push path.
//!
//! Mutations write to local SQLite first, then `enqueue()` adds a row to
//! `pending_ops`. A background worker flushes pending ops to Supabase,
//! retrying with exponential backoff on failure.

use sqlx::SqlitePool;

use super::error::SyncError;

/// Operations that exceed this many attempts are dead-lettered.
const MAX_ATTEMPTS: i64 = 20;

/// A single pending operation read from the queue.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PendingOp {
    pub id: i64,
    pub op_type: String,
    pub table_name: String,
    pub record_id: String,
    pub payload_json: Option<String>,
    pub conflict_key: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

/// Enqueue an upsert operation. If one is already queued for the same
/// (table, record, op_type), the payload is replaced (deduplication).
pub async fn enqueue_upsert(
    pool: &SqlitePool,
    table_name: &str,
    record_id: &str,
    payload_json: &str,
    conflict_key: &str,
    tier: u8,
) -> Result<(), SyncError> {
    sqlx::query(
        "INSERT INTO pending_ops (op_type, table_name, record_id, payload_json, conflict_key, tier, next_retry_at)
         VALUES ('upsert', ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(table_name, record_id, op_type) DO UPDATE SET
           payload_json = excluded.payload_json,
           attempts = 0,
           last_error = NULL,
           next_retry_at = CURRENT_TIMESTAMP",
    )
    .bind(table_name)
    .bind(record_id)
    .bind(payload_json)
    .bind(conflict_key)
    .bind(tier as i64)
    .execute(pool)
    .await?;

    Ok(())
}

/// Enqueue a delete operation (soft-delete on remote).
pub async fn enqueue_delete(
    pool: &SqlitePool,
    table_name: &str,
    record_id: &str,
    conflict_key: &str,
    tier: u8,
) -> Result<(), SyncError> {
    sqlx::query(
        "INSERT INTO pending_ops (op_type, table_name, record_id, payload_json, conflict_key, tier, next_retry_at)
         VALUES ('delete', ?, ?, NULL, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(table_name, record_id, op_type) DO UPDATE SET
           attempts = 0,
           last_error = NULL,
           next_retry_at = CURRENT_TIMESTAMP",
    )
    .bind(table_name)
    .bind(record_id)
    .bind(conflict_key)
    .bind(tier as i64)
    .execute(pool)
    .await?;

    Ok(())
}

/// Fetch all ops that are ready to be flushed (next_retry_at <= now),
/// ordered by tier (FK dependency order) then creation time.
/// Excludes dead-lettered ops (attempts >= MAX_ATTEMPTS).
pub async fn fetch_ready_ops(pool: &SqlitePool) -> Result<Vec<PendingOp>, SyncError> {
    let rows = sqlx::query_as::<_, PendingOp>(
        "SELECT id, op_type, table_name, record_id, payload_json,
                conflict_key, attempts, last_error
         FROM pending_ops
         WHERE next_retry_at <= CURRENT_TIMESTAMP AND attempts < ?
         ORDER BY tier ASC, created_at ASC
         LIMIT 1000",
    )
    .bind(MAX_ATTEMPTS)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Remove a pending op after successful flush.
pub async fn remove_op(pool: &SqlitePool, op_id: i64) -> Result<(), SyncError> {
    sqlx::query("DELETE FROM pending_ops WHERE id = ?")
        .bind(op_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a failed attempt: increment counter, set error, compute next retry.
/// If the op has exceeded MAX_ATTEMPTS, it is dead-lettered (left in the
/// table but never fetched again until manually reset).
pub async fn record_failure(
    pool: &SqlitePool,
    op_id: i64,
    new_attempts: i64,
    error_message: &str,
) -> Result<(), SyncError> {
    if new_attempts >= MAX_ATTEMPTS {
        eprintln!(
            "[sync] Dead-lettering op {op_id} after {new_attempts} attempts: {error_message}"
        );
        sqlx::query("UPDATE pending_ops SET attempts = ?, last_error = ? WHERE id = ?")
            .bind(new_attempts)
            .bind(error_message)
            .bind(op_id)
            .execute(pool)
            .await?;
        return Ok(());
    }

    // Exponential backoff: min(2^attempts * 5, 300) seconds.
    // Clamp shift to avoid overflow on high attempt counts.
    let shift = std::cmp::min(new_attempts, 30);
    let backoff_secs = std::cmp::min(5i64.saturating_mul(1i64 << shift), 300);

    sqlx::query(
        "UPDATE pending_ops SET
           attempts = ?,
           last_error = ?,
           next_retry_at = datetime('now', '+' || ? || ' seconds')
         WHERE id = ?",
    )
    .bind(new_attempts)
    .bind(error_message)
    .bind(backoff_secs)
    .bind(op_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Count pending operations (for status reporting).
pub async fn count_pending(pool: &SqlitePool) -> Result<i64, SyncError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_ops")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// List failed operations (attempts > 0) for UI display.
pub async fn list_failed(pool: &SqlitePool) -> Result<Vec<PendingOp>, SyncError> {
    let rows = sqlx::query_as::<_, PendingOp>(
        "SELECT id, op_type, table_name, record_id, payload_json,
                conflict_key, attempts, last_error
         FROM pending_ops
         WHERE attempts > 0
         ORDER BY attempts DESC, created_at ASC",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Reset retry timer on a specific op (manual retry from UI).
pub async fn reset_retry(pool: &SqlitePool, op_id: i64) -> Result<(), SyncError> {
    sqlx::query(
        "UPDATE pending_ops SET attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(op_id)
    .execute(pool)
    .await?;
    Ok(())
}
