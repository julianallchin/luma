use sqlx::SqlitePool;

use crate::models::implementations::Implementation;

/// Fetch an implementation by ID
pub async fn get_implementation(pool: &SqlitePool, id: i64) -> Result<Implementation, String> {
    sqlx::query_as::<_, Implementation>(
        "SELECT id, remote_id, uid, pattern_id, name, graph_json, created_at, updated_at
         FROM implementations WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch implementation: {}", e))
}

/// List all implementations
pub async fn list_implementations(pool: &SqlitePool) -> Result<Vec<Implementation>, String> {
    sqlx::query_as::<_, Implementation>(
        "SELECT id, remote_id, uid, pattern_id, name, graph_json, created_at, updated_at
         FROM implementations",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list implementations: {}", e))
}

/// List implementations for a specific pattern
pub async fn list_implementations_for_pattern(
    pool: &SqlitePool,
    pattern_id: i64,
) -> Result<Vec<Implementation>, String> {
    sqlx::query_as::<_, Implementation>(
        "SELECT id, remote_id, uid, pattern_id, name, graph_json, created_at, updated_at
         FROM implementations WHERE pattern_id = ?",
    )
    .bind(pattern_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list implementations for pattern: {}", e))
}

/// Set remote_id after syncing to cloud
pub async fn set_remote_id(pool: &SqlitePool, id: i64, remote_id: i64) -> Result<(), String> {
    sqlx::query("UPDATE implementations SET remote_id = ? WHERE id = ?")
        .bind(remote_id.to_string())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set implementation remote_id: {}", e))?;
    Ok(())
}
