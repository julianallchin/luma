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
//!
//! A failure holds a track back only from the version that failed: once a
//! preprocessor's version is bumped, reconcile retries the track at once, and
//! the new version's failures back off from the first attempt.

use sqlx::SqlitePool;

/// Cap retry backoff at 24h — beyond this we accept the failure and only
/// retry on user-driven actions (rerun-from-UI, app restart with version
/// bump, etc.).
const MAX_BACKOFF_SECS: i64 = 24 * 60 * 60;

/// Record a failed run of `version`. Increments `attempts`, counting from
/// zero when the failure on record is from another version, and sets
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
         WHERE track_id = ? AND preprocessor = ? AND version = ?",
    )
    .bind(track_id)
    .bind(preprocessor)
    .bind(version as i64)
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

/// Exponential backoff in seconds: `min(2^attempts * 60, MAX_BACKOFF_SECS)`.
fn backoff_for(attempts: i64) -> i64 {
    let shift = attempts.clamp(0, 30);
    let raw = 60i64.saturating_mul(1i64 << shift);
    raw.min(MAX_BACKOFF_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocessing::preprocessor::Preprocessor;
    use crate::preprocessing::workers::mert::MertPreprocessor;

    /// A migrated library holding one track with local audio and no analysis.
    async fn library() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::database::local::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, track_hash, title, file_path)
             VALUES ('t1', 'hash', 'Song', '/audio/song.wav')",
        )
        .execute(&db.0)
        .await
        .unwrap();
        (directory, db.0)
    }

    /// How long the failure on record holds the track back, in seconds.
    async fn backoff(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "SELECT strftime('%s', next_retry_at) - strftime('%s', last_attempt)
               FROM preprocessing_failures WHERE track_id = 't1'",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_version_bump_retries_tracks_the_old_version_failed_on() {
        let (_directory, pool) = library().await;
        let step = MertPreprocessor;
        record(
            &pool,
            "t1",
            step.name(),
            step.version() - 1,
            "old worker broke",
        )
        .await
        .unwrap();
        assert_eq!(
            step.list_pending(&pool).await.unwrap(),
            ["t1"],
            "a failure of the old version holds the track back"
        );

        record(&pool, "t1", step.name(), step.version(), "new worker broke")
            .await
            .unwrap();
        assert!(
            step.list_pending(&pool).await.unwrap().is_empty(),
            "a failure of the current version doesn't hold the track back"
        );
    }

    #[tokio::test]
    async fn a_new_versions_failures_back_off_from_the_first_attempt() {
        let (_directory, pool) = library().await;
        record(&pool, "t1", "step", 1, "boom").await.unwrap();
        let first = backoff(&pool).await;
        record(&pool, "t1", "step", 1, "boom").await.unwrap();
        assert!(
            backoff(&pool).await > first,
            "a repeat failure doesn't wait longer"
        );

        record(&pool, "t1", "step", 2, "boom").await.unwrap();
        assert_eq!(
            backoff(&pool).await,
            first,
            "the new version's first failure waits like a repeat"
        );
    }

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
