use sqlx::SqliteConnection;

use crate::models::patterns::PatternSummary;

const PATTERN_SUMMARY_SELECT: &str =
    "SELECT pattern.id, pattern.uid, pattern.score_id, pattern.name, pattern.description,
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

/// A pattern summary, or `None` when the id names nothing this caller sees.
pub async fn optional_pattern(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> Result<Option<PatternSummary>, String> {
    sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "{} WHERE pattern.id = ?",
        PATTERN_SUMMARY_SELECT
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to fetch pattern: {error}"))
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

// -----------------------------------------------------------------------------
// Community / sharing support
// -----------------------------------------------------------------------------
