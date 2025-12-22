use crate::models::node_graph::{Graph, PatternArgDef};
use crate::models::patterns::PatternSummary;

/// Core: fetch a pattern summary
pub async fn get_pattern_pool(pool: &sqlx::SqlitePool, id: i64) -> Result<PatternSummary, String> {
    let row = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.remote_id, p.uid, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch pattern: {}\n", e))?;

    Ok(row)
}

/// Core: list patterns
pub async fn list_patterns_pool(pool: &sqlx::SqlitePool) -> Result<Vec<PatternSummary>, String> {
    let rows = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.remote_id, p.uid, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         ORDER BY p.updated_at DESC",
    )
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
    // remote_id starts as NULL - populated after successful cloud sync with Supabase's BIGINT id
    let id =
        sqlx::query("INSERT INTO patterns (name, description, uid) VALUES (?, ?, ?)")
            .bind(&name)
            .bind(&description)
            .bind(&uid)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to create pattern: {}\n", e))?
            .last_insert_rowid();

    get_pattern_pool(pool, id).await
}

/// Core: set pattern category
pub async fn set_pattern_category_pool(
    pool: &sqlx::SqlitePool,
    pattern_id: i64,
    category_id: Option<i64>,
) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET category_id = ? WHERE id = ?")
        .bind(category_id)
        .bind(pattern_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern category: {}\n", e))?;

    Ok(())
}

/// Core: fetch a pattern graph
pub async fn get_pattern_graph_pool(pool: &sqlx::SqlitePool, id: i64) -> Result<String, String> {
    let default_graph: Option<String> = sqlx::query_scalar(
        "SELECT i.graph_json
         FROM implementations i
         JOIN patterns p ON p.default_implementation_id = i.id
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch default implementation: {}\n", e))?;

    if let Some(graph_json) = default_graph {
        return Ok(graph_json);
    }

    let result: Option<String> = sqlx::query_scalar(
        "SELECT graph_json FROM implementations WHERE pattern_id = ? ORDER BY id",
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
    id: i64,
) -> Result<Vec<PatternArgDef>, String> {
    let graph_json = get_pattern_graph_pool(pool, id).await?;
    let graph: Graph = serde_json::from_str(&graph_json).unwrap_or(Graph {
        nodes: vec![],
        edges: vec![],
        args: vec![],
    });
    Ok(graph.args)
}

/// Core: save pattern graph
pub async fn save_pattern_graph_pool(
    pool: &sqlx::SqlitePool,
    id: i64,
    graph_json: String,
) -> Result<(), String> {
    let default_id: Option<Option<i64>> =
        sqlx::query_scalar("SELECT default_implementation_id FROM patterns WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to fetch pattern default implementation: {}\n", e))?;

    if let Some(Some(default_id)) = default_id {
        sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = ?")
            .bind(&graph_json)
            .bind(default_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to update pattern graph: {}\n", e))?;
        return Ok(());
    }

    let implementation_id =
        sqlx::query("INSERT INTO implementations (pattern_id, graph_json) VALUES (?, ?)")
            .bind(id)
            .bind(&graph_json)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to create implementation: {}\n", e))?
            .last_insert_rowid();

    sqlx::query("UPDATE patterns SET default_implementation_id = ? WHERE id = ?")
        .bind(implementation_id)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set default implementation: {}\n", e))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Sync support
// -----------------------------------------------------------------------------

/// Set remote_id after syncing to cloud
pub async fn set_remote_id(pool: &sqlx::SqlitePool, id: i64, remote_id: i64) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET remote_id = ? WHERE id = ?")
        .bind(remote_id.to_string())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern remote_id: {}", e))?;
    Ok(())
}

/// Clear remote_id (e.g., after deleting from cloud)
pub async fn clear_remote_id(pool: &sqlx::SqlitePool, id: i64) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET remote_id = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear pattern remote_id: {}", e))?;
    Ok(())
}
