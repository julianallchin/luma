//! Per-track per-preprocessor failure tracking with exponential backoff.
//!
//! Local-only — failure semantics are device-specific (a corrupt audio file on
//! one machine isn't necessarily corrupt on another, and bug-fix-via-retry
//! cycles shouldn't cross device boundaries). Lives in
//! `preprocessing_failures` (track_id, preprocessor) PK.
//!
//! On success: caller invokes [`clear`] so the next reconcile won't filter
//! the track out.
//!
//! On failure: caller invokes [`record`] which increments attempts and pushes
//! `next_retry_at` forward by 2^attempts minutes, capped at 24 hours.

use sqlx::SqlitePool;

/// Cap retry backoff at 24h — beyond this we accept the failure and only
/// retry on user-driven actions (rerun-from-UI, app restart with version
/// bump, etc.).
const MAX_BACKOFF_SECS: i64 = 24 * 60 * 60;

/// Record a failed run. Atomically increments `attempts` and sets
/// `next_retry_at = now + 2^attempts minutes` (capped).
pub async fn record(
    pool: &SqlitePool,
    track_id: &str,
    preprocessor: &str,
    version: u32,
    error: &str,
) -> Result<(), String> {
    // Compute attempts first so we can derive the backoff in the same SQL.
    let prior_attempts: i64 = sqlx::query_scalar(
        "SELECT attempts FROM preprocessing_failures
         WHERE track_id = ? AND preprocessor = ?",
    )
    .bind(track_id)
    .bind(preprocessor)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to read prior attempts: {e}"))?
    .unwrap_or(0);

    let new_attempts = prior_attempts.saturating_add(1);
    let backoff_secs = backoff_for(new_attempts);

    let res = sqlx::query(
        "INSERT INTO preprocessing_failures
            (track_id, preprocessor, version, attempts, last_error, last_attempt, next_retry_at)
         VALUES (?, ?, ?, ?, ?,
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                 strftime('%Y-%m-%dT%H:%M:%SZ','now', '+' || ? || ' seconds'))
         ON CONFLICT(track_id, preprocessor) DO UPDATE SET
             version       = excluded.version,
             attempts      = excluded.attempts,
             last_error    = excluded.last_error,
             last_attempt  = excluded.last_attempt,
             next_retry_at = excluded.next_retry_at",
    )
    .bind(track_id)
    .bind(preprocessor)
    .bind(version as i64)
    .bind(new_attempts)
    .bind(error)
    .bind(backoff_secs)
    .execute(pool)
    .await;

    match res {
        Ok(_) => Ok(()),
        // Track was deleted (typically by sync) between the run starting and
        // this insert. The FK enforces the invariant atomically — no
        // caller-side pre-check could close this race — so treat as a clean
        // cancellation. SQLite extended result code 787 = SQLITE_CONSTRAINT_FOREIGNKEY.
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("787") => Ok(()),
        Err(e) => Err(format!("Failed to record preprocessing failure: {e}")),
    }
}

/// Drop the failure row for a (track, preprocessor) pair. Call on a
/// successful run so the next reconcile considers the track healthy again.
pub async fn clear(pool: &SqlitePool, track_id: &str, preprocessor: &str) -> Result<(), String> {
    sqlx::query(
        "DELETE FROM preprocessing_failures
         WHERE track_id = ? AND preprocessor = ?",
    )
    .bind(track_id)
    .bind(preprocessor)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to clear preprocessing failure: {e}"))?;
    Ok(())
}

/// The error the last failed run reported, if the pair is currently in a failed
/// state. `None` means "no failure on record" — which, combined with a missing
/// artifact row, means the preprocessor simply hasn't run.
pub async fn last_error(
    pool: &SqlitePool,
    track_id: &str,
    preprocessor: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar(
        "SELECT last_error FROM preprocessing_failures
         WHERE track_id = ? AND preprocessor = ?",
    )
    .bind(track_id)
    .bind(preprocessor)
    .fetch_optional(pool)
    .await
    .map(|row: Option<Option<String>>| row.flatten())
    .map_err(|e| format!("Failed to read preprocessing failure: {e}"))
}

/// The first line of a step's error, which says what failed. A worker's error
/// goes on with everything the worker wrote to stderr (see
/// [`workers::failed`](crate::preprocessing::workers::failed)): the record
/// keeps all of it, and the import list and the agent show this line.
pub fn headline(error: &str) -> &str {
    error.lines().next().unwrap_or(error)
}

/// Exponential backoff in seconds: `min(2^attempts * 60, MAX_BACKOFF_SECS)`.
fn backoff_for(attempts: i64) -> i64 {
    let shift = attempts.clamp(0, 30);
    let raw = 60i64.saturating_mul(1i64 << shift);
    raw.min(MAX_BACKOFF_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_until_it_caps() {
        let mut previous = backoff_for(1);
        for attempts in 2..=64 {
            let next = backoff_for(attempts);
            assert!(
                next <= MAX_BACKOFF_SECS,
                "attempt {attempts} waits past the cap"
            );
            assert!(
                next == 2 * previous || next == MAX_BACKOFF_SECS,
                "attempt {attempts}: {previous}s then {next}s"
            );
            previous = next;
        }
        assert_eq!(
            previous, MAX_BACKOFF_SECS,
            "repeated failures reach the cap"
        );
    }
}
