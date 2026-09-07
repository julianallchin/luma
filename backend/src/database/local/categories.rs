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
