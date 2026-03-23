use uuid::Uuid;

use crate::models::patterns::PatternCategory;

/// Fetch a single pattern category by ID
pub async fn get_category(pool: &sqlx::SqlitePool, id: &str) -> Result<PatternCategory, String> {
    sqlx::query_as::<_, PatternCategory>(
        "SELECT id, uid, name, created_at, updated_at FROM pattern_categories WHERE id = ?",
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
        "SELECT id, uid, name, created_at, updated_at FROM pattern_categories ORDER BY lower(name) ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query pattern categories: {}", e))
}

/// Create a pattern category
pub async fn create_pattern_category_pool(
    pool: &sqlx::SqlitePool,
    name: String,
    uid: Option<String>,
) -> Result<PatternCategory, String> {
    let id = Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO pattern_categories (id, name, uid) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(&uid)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create category: {}", e))?;

    get_category(pool, &id).await
}

// -----------------------------------------------------------------------------
// Delta sync support
// -----------------------------------------------------------------------------

/// List dirty pattern categories for the current user
pub async fn list_dirty_categories(
    pool: &sqlx::SqlitePool,
    uid: &str,
) -> Result<Vec<PatternCategory>, String> {
    sqlx::query_as::<_, PatternCategory>(
        "SELECT id, uid, name, created_at, updated_at FROM pattern_categories WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list dirty categories: {}", e))
}

/// Mark a category as synced
pub async fn mark_category_synced(pool: &sqlx::SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query(
        "UPDATE pattern_categories SET synced_at = updated_at, version = version + 1 WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark category synced: {}", e))?;
    Ok(())
}
