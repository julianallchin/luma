use crate::models::patterns::PatternCategory;

/// Core: list pattern categories
pub async fn list_pattern_categories_pool(
    pool: &sqlx::SqlitePool,
) -> Result<Vec<PatternCategory>, String> {
    let rows = sqlx::query_as::<_, PatternCategory>(
        "SELECT id, name, created_at, updated_at FROM pattern_categories ORDER BY lower(name) ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query pattern categories: {}", e))?;

    Ok(rows)
}

/// Core: create a pattern category
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

    let row = sqlx::query_as::<_, PatternCategory>(
        "SELECT id, name, created_at, updated_at FROM pattern_categories WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch created category: {}", e))?;

    Ok(row)
}
