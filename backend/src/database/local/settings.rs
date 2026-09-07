//! Settings database operations

use sqlx::SqlitePool;
use std::collections::HashMap;

#[derive(sqlx::FromRow)]
struct SettingRow {
    key: String,
    value: String,
}

/// Fetch all settings as a key-value map
pub async fn get_all_settings(pool: &SqlitePool) -> Result<HashMap<String, String>, String> {
    let rows = sqlx::query_as::<_, SettingRow>("SELECT key, value FROM settings")
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to fetch settings: {}", e))?;

    Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
}

/// Update a single setting (upsert)
pub async fn update_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = ?",
    )
    .bind(key)
    .bind(value)
    .bind(value)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update setting: {}", e))?;

    Ok(())
}
