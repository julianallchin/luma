//! Sync state: tracks the last-pull timestamp per (user, table).

use sqlx::SqlitePool;

use super::error::SyncError;

/// Get the last-pulled-at timestamp for a table and user.
/// Returns epoch if the table has never been pulled.
pub async fn get_last_pulled_at(
    pool: &SqlitePool,
    uid: &str,
    table_name: &str,
) -> Result<String, SyncError> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT last_pulled_at FROM sync_state WHERE uid = ? AND table_name = ?",
    )
    .bind(uid)
    .bind(table_name)
    .fetch_optional(pool)
    .await?;

    Ok(row.unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()))
}

/// Update the last-pulled-at timestamp for a table and user.
pub async fn set_last_pulled_at(
    pool: &SqlitePool,
    uid: &str,
    table_name: &str,
    timestamp: &str,
) -> Result<(), SyncError> {
    sqlx::query(
        "INSERT INTO sync_state (uid, table_name, last_pulled_at) VALUES (?, ?, ?)
         ON CONFLICT(uid, table_name) DO UPDATE SET last_pulled_at = excluded.last_pulled_at",
    )
    .bind(uid)
    .bind(table_name)
    .bind(timestamp)
    .execute(pool)
    .await?;

    Ok(())
}
