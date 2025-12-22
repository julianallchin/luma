use crate::models::patterns::PatternCategory;

/// Fetch a single pattern category by ID
pub async fn get_category(pool: &sqlx::SqlitePool, id: i64) -> Result<PatternCategory, String> {
    sqlx::query_as::<_, PatternCategory>(
        "SELECT id, remote_id, uid, name, created_at, updated_at FROM pattern_categories WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch category: {}", e))
}

/// List all pattern categories
pub async fn list_pattern_categories_pool(
    pool: &sqlx::SqlitePool,
) -> Result<Vec<PatternCategory>, String> {
    sqlx::query_as::<_, PatternCategory>(
        "SELECT id, remote_id, uid, name, created_at, updated_at FROM pattern_categories ORDER BY lower(name) ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query pattern categories: {}", e))
}

/// Create a pattern category
pub async fn create_pattern_category_pool(
    pool: &sqlx::SqlitePool,
    name: String,
) -> Result<PatternCategory, String> {
    let id = sqlx::query("INSERT INTO pattern_categories (name) VALUES (?)")
        .bind(&name)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create category: {}", e))?
        .last_insert_rowid();

    get_category(pool, id).await
}

/// Set remote_id after syncing to cloud
pub async fn set_remote_id(pool: &sqlx::SqlitePool, id: i64, remote_id: i64) -> Result<(), String> {
    sqlx::query("UPDATE pattern_categories SET remote_id = ? WHERE id = ?")
        .bind(remote_id.to_string())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set category remote_id: {}", e))?;
    Ok(())
}

/// Clear remote_id (e.g., after deleting from cloud)
pub async fn clear_remote_id(pool: &sqlx::SqlitePool, id: i64) -> Result<(), String> {
    sqlx::query("UPDATE pattern_categories SET remote_id = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear category remote_id: {}", e))?;
    Ok(())
}
