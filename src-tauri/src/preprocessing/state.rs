//! CRUD helpers for the `preprocessing_runs` table.
//!
//! This table is the single source of truth for "has preprocessor X been
//! successfully run for track Y at version V?". Bumping a preprocessor's
//! `version()` invalidates every previously-recorded row at the old version,
//! which is how shipped algorithm updates trigger automatic backfills.

use sqlx::SqlitePool;

/// Mark a (track, preprocessor) as currently running. Upserts a row at status
/// `running` with `started_at = now`, clearing any previous `completed_at` /
/// `error`. Caller is expected to follow up with [`mark_run_completed`] or
/// [`mark_run_failed`].
pub async fn upsert_run_started(
    pool: &SqlitePool,
    track_id: &str,
    name: &str,
    version: u32,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO preprocessing_runs (track_id, preprocessor, version, status,
                                          started_at, completed_at, error)
         VALUES (?, ?, ?, 'running', strftime('%Y-%m-%dT%H:%M:%SZ','now'), NULL, NULL)
         ON CONFLICT(track_id, preprocessor) DO UPDATE SET
             version = excluded.version,
             status = 'running',
             started_at = excluded.started_at,
             completed_at = NULL,
             error = NULL",
    )
    .bind(track_id)
    .bind(name)
    .bind(version as i64)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark run started: {e}"))?;
    Ok(())
}

/// Mark a (track, preprocessor) as successfully completed at the given version.
pub async fn mark_run_completed(
    pool: &SqlitePool,
    track_id: &str,
    name: &str,
    version: u32,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO preprocessing_runs (track_id, preprocessor, version, status,
                                          started_at, completed_at, error)
         VALUES (?, ?, ?, 'completed',
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'), NULL)
         ON CONFLICT(track_id, preprocessor) DO UPDATE SET
             version = excluded.version,
             status = 'completed',
             completed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
             error = NULL",
    )
    .bind(track_id)
    .bind(name)
    .bind(version as i64)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark run completed: {e}"))?;
    Ok(())
}

/// Mark a (track, preprocessor) run as failed with an error message.
pub async fn mark_run_failed(
    pool: &SqlitePool,
    track_id: &str,
    name: &str,
    version: u32,
    error: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO preprocessing_runs (track_id, preprocessor, version, status,
                                          started_at, completed_at, error)
         VALUES (?, ?, ?, 'failed',
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?)
         ON CONFLICT(track_id, preprocessor) DO UPDATE SET
             version = excluded.version,
             status = 'failed',
             completed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
             error = excluded.error",
    )
    .bind(track_id)
    .bind(name)
    .bind(version as i64)
    .bind(error)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark run failed: {e}"))?;
    Ok(())
}

/// Has the named preprocessor completed for this track at exactly this version?
pub async fn has_completed_run(
    pool: &SqlitePool,
    track_id: &str,
    name: &str,
    version: u32,
) -> Result<bool, String> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM preprocessing_runs
         WHERE track_id = ? AND preprocessor = ? AND version = ? AND status = 'completed'
         LIMIT 1",
    )
    .bind(track_id)
    .bind(name)
    .bind(version as i64)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to query preprocessing_runs: {e}"))?;
    Ok(row.is_some())
}

/// Given an expected `(name, version)` set, return the preprocessor names that
/// are NOT currently completed at the expected version for this track.
pub async fn list_stale(
    pool: &SqlitePool,
    track_id: &str,
    expected: &[(&str, u32)],
) -> Result<Vec<String>, String> {
    let mut stale = Vec::new();
    for (name, version) in expected {
        if !has_completed_run(pool, track_id, name, *version).await? {
            stale.push((*name).to_string());
        }
    }
    Ok(stale)
}
