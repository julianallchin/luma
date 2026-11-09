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
//! A failure says something only about the version of the step that failed,
//! in the Python environment it failed in. Startup reconcile first forgets the
//! failures of other versions ([`forget_other_versions`]) and those recorded
//! before the environment was last installed ([`forget_before`]). A step whose
//! version changed, or a repaired environment, retries every track at once,
//! backs off from the first attempt if it fails again, and [`last_error`] no
//! longer reports the old errors.

use std::time::SystemTime;

use sqlx::SqlitePool;

/// Cap retry backoff at 24h: reconcile retries a failure at the cap once a
/// day, and at once after the step's version changes or the Python environment
/// is reinstalled.
const MAX_BACKOFF_SECS: i64 = 24 * 60 * 60;

/// Record a failed run. Increments `attempts` and sets
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

/// Holds for a failure row `f` that still holds its track back. The one
/// statement of the rule: reconcile does not queue a track for a step in
/// backoff ([`pending_tracks`]), and a track queued for another step skips it,
/// with its dependents ([`held_back`]).
///
/// [`pending_tracks`]: crate::preprocessing::preprocessor::pending_tracks
pub(crate) const IN_BACKOFF: &str = "f.next_retry_at > strftime('%Y-%m-%dT%H:%M:%SZ','now')";

/// The steps whose last failure on the track is still in backoff, each with
/// that failure's error.
pub async fn held_back(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let rows: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT f.preprocessor, f.last_error FROM preprocessing_failures f
          WHERE f.track_id = ? AND {IN_BACKOFF}"
    )))
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to read the steps in backoff for track {track_id}: {e}"))?;
    Ok(rows.into_iter().collect())
}

/// One step held back on one track, as [`all_held_back`] lists it.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct HeldStep {
    pub track_id: String,
    pub preprocessor: String,
    pub last_error: String,
}

/// Every step held back on any track, by track: what the status
/// bar lists as paused after failures.
pub async fn all_held_back(pool: &SqlitePool) -> Result<Vec<HeldStep>, String> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT f.track_id, f.preprocessor, f.last_error
           FROM preprocessing_failures f
          WHERE {IN_BACKOFF}
          ORDER BY f.track_id, f.preprocessor"
    )))
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to read the steps in backoff: {e}"))
}

/// Forget every failure recorded before `installed`, when the Python
/// environment the workers run in was last installed. Such a failure says
/// nothing about the environment now running, so a repaired environment
/// retries every track at once.
///
/// An install time later than now means the clock was ahead when setup ran,
/// or has been set back since. Every failure recorded until the clock catches
/// up would then look older than the install and be forgotten at every
/// launch, so nothing is forgotten.
pub async fn forget_before(pool: &SqlitePool, installed: SystemTime) -> Result<(), String> {
    if installed > SystemTime::now() {
        return Ok(());
    }
    let seconds = installed
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    sqlx::query(
        "DELETE FROM preprocessing_failures
          WHERE last_attempt < strftime('%Y-%m-%dT%H:%M:%SZ', ?, 'unixepoch')",
    )
    .bind(i64::try_from(seconds).unwrap_or(i64::MAX))
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to forget failures from an earlier environment: {e}"))?;
    Ok(())
}

/// Forget `preprocessor`'s failures recorded by any version but `version`.
pub async fn forget_other_versions(
    pool: &SqlitePool,
    preprocessor: &str,
    version: u32,
) -> Result<(), String> {
    sqlx::query("DELETE FROM preprocessing_failures WHERE preprocessor = ? AND version != ?")
        .bind(preprocessor)
        .bind(version as i64)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to forget earlier {preprocessor} failures: {e}"))?;
    Ok(())
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
    use crate::preprocessing::preprocessor::Preprocessor;
    use crate::preprocessing::workers::roots::RootsPreprocessor;

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
        let step = RootsPreprocessor;
        let old = step.version() - 1;
        record(&pool, "t1", step.name(), old, "old worker broke")
            .await
            .unwrap();

        forget_other_versions(&pool, step.name(), step.version())
            .await
            .unwrap();
        assert_eq!(
            step.list_pending(&pool).await.unwrap(),
            ["t1"],
            "a failure of the old version holds the track back"
        );
        assert_eq!(
            last_error(&pool, "t1", step.name()).await.unwrap(),
            None,
            "the old version's error is reported as the step's"
        );

        record(&pool, "t1", step.name(), step.version(), "new worker broke")
            .await
            .unwrap();
        forget_other_versions(&pool, step.name(), step.version())
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

        forget_other_versions(&pool, "step", 2).await.unwrap();
        record(&pool, "t1", "step", 2, "boom").await.unwrap();
        assert_eq!(
            backoff(&pool).await,
            first,
            "the new version's first failure waits like a repeat"
        );
    }

    #[tokio::test]
    async fn failures_from_before_the_environment_was_installed_are_forgotten() {
        let (_directory, pool) = library().await;
        record(&pool, "t1", "old", 1, "boom").await.unwrap();
        sqlx::query(
            "UPDATE preprocessing_failures
                SET last_attempt = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-2 hours')
              WHERE preprocessor = 'old'",
        )
        .execute(&pool)
        .await
        .unwrap();
        record(&pool, "t1", "new", 1, "boom").await.unwrap();
        let now = SystemTime::now();
        let hour = std::time::Duration::from_secs(3600);

        forget_before(&pool, now - hour).await.unwrap();
        assert_eq!(
            last_error(&pool, "t1", "old").await.unwrap(),
            None,
            "a failure from before the environment was installed is kept"
        );
        assert!(
            last_error(&pool, "t1", "new").await.unwrap().is_some(),
            "a failure in the environment now installed was forgotten"
        );

        // A clock set back since the install: nothing is older than it.
        forget_before(&pool, now + hour).await.unwrap();
        assert!(
            last_error(&pool, "t1", "new").await.unwrap().is_some(),
            "an install in the future forgot a failure"
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
