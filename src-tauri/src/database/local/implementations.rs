use sqlx::SqlitePool;

use crate::models::implementations::Implementation;

/// Fetch an implementation by ID
pub async fn get_implementation(pool: &SqlitePool, id: &str) -> Result<Implementation, String> {
    sqlx::query_as::<_, Implementation>(
        "SELECT id, uid, pattern_id, name, graph_json, created_at, updated_at
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
        "SELECT id, uid, pattern_id, name, graph_json, created_at, updated_at
         FROM implementations",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list implementations: {}", e))
}

/// List implementations for a specific pattern
pub async fn list_implementations_for_pattern(
    pool: &SqlitePool,
    pattern_id: &str,
) -> Result<Vec<Implementation>, String> {
    sqlx::query_as::<_, Implementation>(
        "SELECT id, uid, pattern_id, name, graph_json, created_at, updated_at
         FROM implementations WHERE pattern_id = ?",
    )
    .bind(pattern_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list implementations for pattern: {}", e))
}
