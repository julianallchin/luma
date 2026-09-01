//! Retry state for the push scan.
//!
//! The only push state that is not derivable from the tables: how many times a
//! row has failed, why, and when to try again. It is keyed on `(table, record)`
//! and written *only* by failures — the scan never touches it, which is why the
//! backoff cannot be reset by the act of noticing a dirty row again (audit
//! T2.1).
//!
//! It carries no payload and no intent. Delete every row of it and push still
//! knows exactly what to send; it just forgets what already went wrong.

use sqlx::SqlitePool;

use super::error::SyncError;

/// Attempts after which a transient failure is treated as permanent.
const MAX_ATTEMPTS: i64 = 20;

/// A push subject that is not being retried.
///
/// Test-only for now: nothing in the app reads it, because surfacing dead
/// letters is still audit T2.2. The state is one query away when a UI exists.
#[cfg(test)]
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Blocked {
    pub table_name: String,
    pub record_id: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

/// Which of the two things an identity can owe the server this failure is
/// about. They are separate budgets: a refused deletion must not hand its
/// verdict to whatever row next occupies the same primary key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Subject {
    Row,
    Tombstone,
}

impl Subject {
    pub fn as_str(self) -> &'static str {
        match self {
            Subject::Row => "row",
            Subject::Tombstone => "tombstone",
        }
    }
}

/// Whether delivery can ever succeed as the row currently stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Try again later, with backoff.
    Transient,
    /// Nothing about a retry can change the outcome: the identity is one the
    /// remote column type cannot hold, an immutable row collided with different
    /// bytes, or the table is not registered. Quiet until the content changes.
    Permanent,
}

/// The SQL predicate a dirty row must also satisfy to be sent this cycle.
///
/// Expects the failure row left-joined as `failure`. `version_expr` is the
/// scanned table's `version` column, or the literal `NULL` for a table that has
/// none: a row whose version has moved since the failure was recorded is
/// different content and starts its budget over — the state-based equivalent of
/// "the payload changed" — and a table without versions never resets, which is
/// right for rows that cannot change.
pub fn ready_predicate(version_expr: &str) -> String {
    format!(
        "(failure.record_id IS NULL
      OR (failure.permanent = 0 AND failure.next_retry_at <= CURRENT_TIMESTAMP)
      OR (failure.seen_version IS NOT NULL AND failure.seen_version <> {version_expr}))"
    )
}

/// The same question for a subject with no version to compare: a tombstone is
/// ready when it is not permanent and its backoff has elapsed.
pub const TOMBSTONE_READY_PREDICATE: &str = "(failure.record_id IS NULL
      OR (failure.permanent = 0 AND failure.next_retry_at <= CURRENT_TIMESTAMP))";

/// Record a failed delivery. `version` is the row's `version` column where it
/// has one; `None` (immutable rows, tombstones) means the budget never resets.
pub async fn record_failure(
    pool: &SqlitePool,
    principal_key: &str,
    table_name: &str,
    record_id: &str,
    subject: Subject,
    version: Option<i64>,
    verdict: Verdict,
    error: &str,
) -> Result<(), SyncError> {
    let existing: Option<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT attempts, seen_version FROM sync_push_failures
         WHERE principal_key = ? AND table_name = ? AND record_id = ? AND subject = ?",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .bind(subject.as_str())
    .fetch_optional(pool)
    .await?;
    let attempts = match existing {
        Some((attempts, seen)) if seen == version => attempts + 1,
        // Absent, or recorded against different content.
        _ => 1,
    };
    let permanent = verdict == Verdict::Permanent || attempts >= MAX_ATTEMPTS;
    // min(2^n * 5, 300) seconds, the same curve the queue used.
    let backoff = std::cmp::min(
        5i64.saturating_mul(1i64 << std::cmp::min(attempts, 30)),
        300,
    );
    if permanent {
        eprintln!(
            "[sync] {table_name}.{record_id} ({}) is not deliverable: {error}",
            subject.as_str()
        );
    }
    sqlx::query(
        "INSERT INTO sync_push_failures
            (principal_key, table_name, record_id, subject, attempts, last_error,
             next_retry_at, seen_version, permanent)
         VALUES (?, ?, ?, ?, ?, ?, datetime('now', '+' || ? || ' seconds'), ?, ?)
         ON CONFLICT(principal_key, table_name, record_id, subject) DO UPDATE SET
             attempts = excluded.attempts,
             last_error = excluded.last_error,
             next_retry_at = excluded.next_retry_at,
             seen_version = excluded.seen_version,
             permanent = excluded.permanent",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .bind(subject.as_str())
    .bind(attempts)
    .bind(error)
    .bind(backoff)
    .bind(version)
    .bind(i64::from(permanent))
    .execute(pool)
    .await?;
    Ok(())
}

/// A non-terminal authored integration receipt: reschedule with the same capped
/// backoff, but never let the attempt budget make it permanent. A stale head or
/// an earlier pending proposal has to stay retryable until the server issues a
/// terminal receipt.
pub async fn defer_retry(
    pool: &SqlitePool,
    principal_key: &str,
    table_name: &str,
    record_id: &str,
    error: &str,
) -> Result<(), SyncError> {
    let attempts: Option<i64> = sqlx::query_scalar(
        "SELECT attempts FROM sync_push_failures
         WHERE principal_key = ? AND table_name = ? AND record_id = ? AND subject = 'row'",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .fetch_optional(pool)
    .await?;
    let attempts = (attempts.unwrap_or(0) + 1).min(MAX_ATTEMPTS - 1);
    let backoff = std::cmp::min(
        5i64.saturating_mul(1i64 << std::cmp::min(attempts, 30)),
        300,
    );
    sqlx::query(
        "INSERT INTO sync_push_failures
            (principal_key, table_name, record_id, subject, attempts, last_error,
             next_retry_at, seen_version, permanent)
         VALUES (?, ?, ?, 'row', ?, ?, datetime('now', '+' || ? || ' seconds'), NULL, 0)
         ON CONFLICT(principal_key, table_name, record_id, subject) DO UPDATE SET
             attempts = excluded.attempts,
             last_error = excluded.last_error,
             next_retry_at = excluded.next_retry_at,
             permanent = 0",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .bind(attempts)
    .bind(error)
    .bind(backoff)
    .execute(pool)
    .await?;
    Ok(())
}

/// Forget the failure history for a subject that has just been delivered.
pub async fn clear(
    pool: &SqlitePool,
    principal_key: &str,
    table_name: &str,
    record_id: &str,
    subject: Subject,
) -> Result<(), SyncError> {
    sqlx::query(
        "DELETE FROM sync_push_failures
         WHERE principal_key = ? AND table_name = ? AND record_id = ? AND subject = ?",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .bind(subject.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether this subject is currently held back, and by what.
#[cfg(test)]
pub async fn state_of(
    pool: &SqlitePool,
    principal_key: &str,
    table_name: &str,
    record_id: &str,
) -> Result<Option<(i64, bool, Option<String>)>, SyncError> {
    let row: Option<(i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT attempts, permanent, last_error FROM sync_push_failures
         WHERE principal_key = ? AND table_name = ? AND record_id = ?
         ORDER BY subject",
    )
    .bind(principal_key)
    .bind(table_name)
    .bind(record_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(attempts, permanent, error)| (attempts, permanent != 0, error)))
}

/// Everything push has given up on for this principal.
#[cfg(test)]
pub async fn blocked(pool: &SqlitePool, principal_key: &str) -> Result<Vec<Blocked>, SyncError> {
    Ok(sqlx::query_as::<_, Blocked>(
        "SELECT table_name, record_id, attempts, last_error
         FROM sync_push_failures
         WHERE principal_key = ? AND permanent = 1
         ORDER BY table_name, record_id",
    )
    .bind(principal_key)
    .fetch_all(pool)
    .await?)
}
