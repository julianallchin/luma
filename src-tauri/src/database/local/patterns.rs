use uuid::Uuid;

use crate::models::node_graph::{Graph, PatternArgDef};
use crate::models::patterns::PatternSummary;

const PATTERN_SUMMARY_SELECT: &str =
    "SELECT id, uid, name, description, category_name, created_at, updated_at, is_verified, author_name, forked_from_id FROM patterns";

/// Core: fetch a pattern summary
pub async fn get_pattern_pool(pool: &sqlx::SqlitePool, id: &str) -> Result<PatternSummary, String> {
    let row = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "{} WHERE id = ?",
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
    let rows = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "{} ORDER BY updated_at DESC",
        PATTERN_SUMMARY_SELECT
    )))
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query patterns: {}\n", e))?;

    Ok(rows)
}

/// Core: create a pattern
pub async fn create_pattern_pool(
    pool: &sqlx::SqlitePool,
    name: String,
    description: Option<String>,
    uid: Option<String>,
) -> Result<PatternSummary, String> {
    let id = Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO patterns (id, name, description, uid) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(&description)
        .bind(&uid)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create pattern: {}\n", e))?;

    get_pattern_pool(pool, &id).await
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

/// Core: fetch a pattern graph
pub async fn get_pattern_graph_pool(pool: &sqlx::SqlitePool, id: &str) -> Result<String, String> {
    let result: Option<String> = sqlx::query_scalar(
        "SELECT graph_json FROM implementations WHERE pattern_id = ? ORDER BY id LIMIT 1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch pattern graph: {}\n", e))?;

    Ok(result.unwrap_or_else(|| "{\"nodes\":[],\"edges\":[],\"args\":[]}".to_string()))
}

/// Core: fetch pattern arg defs
pub async fn get_pattern_args_pool(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> Result<Vec<PatternArgDef>, String> {
    let graph_json = get_pattern_graph_pool(pool, id).await?;
    let graph: Graph = serde_json::from_str(&graph_json).unwrap_or(Graph {
        nodes: vec![],
        edges: vec![],
        args: vec![],
    });
    Ok(graph.args)
}

/// Core: save pattern graph. Derives uid from the parent pattern row.
pub async fn save_pattern_graph_pool(
    pool: &sqlx::SqlitePool,
    id: &str,
    graph_json: String,
) -> Result<(), String> {
    let uid: Option<String> = sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to fetch pattern: {}\n", e))?
        .ok_or_else(|| format!("Pattern {} not found", id))?;

    // Try to update existing implementation, otherwise insert new one
    let updated = sqlx::query(
        "UPDATE implementations SET graph_json = ?, uid = COALESCE(uid, ?) WHERE pattern_id = ?",
    )
    .bind(&graph_json)
    .bind(&uid)
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update pattern graph: {}\n", e))?;

    if updated.rows_affected() == 0 {
        let impl_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, uid, graph_json) VALUES (?, ?, ?, ?)",
        )
        .bind(&impl_id)
        .bind(id)
        .bind(&uid)
        .bind(&graph_json)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create implementation: {}\n", e))?;
    }

    Ok(())
}

/// Core: delete a pattern and its implementations
pub async fn delete_pattern_pool(pool: &sqlx::SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM implementations WHERE pattern_id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete implementations: {}", e))?;

    sqlx::query("DELETE FROM patterns WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete pattern: {}", e))?;

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
