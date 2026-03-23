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

// -----------------------------------------------------------------------------
// Delta sync support
// -----------------------------------------------------------------------------

/// List dirty implementations for the current user
pub async fn list_dirty_implementations(
    pool: &SqlitePool,
    uid: &str,
) -> Result<Vec<Implementation>, String> {
    sqlx::query_as::<_, Implementation>(
        "SELECT id, uid, pattern_id, name, graph_json, created_at, updated_at
         FROM implementations WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list dirty implementations: {}", e))
}

/// Mark an implementation as synced
pub async fn mark_implementation_synced(pool: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query(
        "UPDATE implementations SET synced_at = updated_at, version = version + 1 WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark implementation synced: {}", e))?;
    Ok(())
}
