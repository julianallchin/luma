use sqlx::SqliteConnection;

use crate::models::patterns::PatternSummary;

const PATTERN_SUMMARY_SELECT: &str =
    "SELECT pattern.id, pattern.uid, pattern.name, pattern.description,
            pattern.category_name, pattern.created_at, pattern.updated_at,
            pattern.is_verified, pattern.author_name, pattern.forked_from_id
     FROM patterns pattern
     JOIN auth_visible_patterns visible ON visible.pattern_id = pattern.id";

/// Core: fetch a pattern summary
pub async fn get_pattern_pool(pool: &sqlx::SqlitePool, id: &str) -> Result<PatternSummary, String> {
    let row = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "{} WHERE pattern.id = ?",
        PATTERN_SUMMARY_SELECT
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch pattern: {}\n", e))?;

    Ok(row)
}

/// Core: list patterns
pub async fn list_patterns_pool(pool: &sqlx::SqlitePool) -> Result<Vec<PatternSummary>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open pattern list: {error}"))?;
    list_patterns_for_connection(&mut connection).await
}

pub(crate) async fn list_patterns_for_connection(
    connection: &mut SqliteConnection,
) -> Result<Vec<PatternSummary>, String> {
    let rows = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "{} ORDER BY pattern.updated_at DESC",
        PATTERN_SUMMARY_SELECT
    )))
    .fetch_all(connection)
    .await
    .map_err(|e| format!("Failed to query patterns: {}\n", e))?;

    Ok(rows)
}

/// Core: update pattern name and description
pub async fn update_pattern_pool(
    pool: &sqlx::SqlitePool,
    id: &str,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    sqlx::query("UPDATE patterns SET name = ?, description = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(&name)
        .bind(&description)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update pattern: {}\n", e))?;

    get_pattern_pool(pool, id).await
}

/// Core: set pattern category by name
pub async fn set_pattern_category_pool(
    pool: &sqlx::SqlitePool,
    pattern_id: &str,
    category_name: Option<&str>,
) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET category_name = ? WHERE id = ?")
        .bind(category_name)
        .bind(pattern_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern category: {}\n", e))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Community / sharing support
// -----------------------------------------------------------------------------

/// Set verified state
pub async fn set_verified(pool: &sqlx::SqlitePool, id: &str, verified: bool) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET is_verified = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(verified)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern verified state: {}", e))?;
    Ok(())
}

/// Set author_name
pub async fn set_author_name(pool: &sqlx::SqlitePool, id: &str, name: &str) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET author_name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern author_name: {}", e))?;
    Ok(())
}
