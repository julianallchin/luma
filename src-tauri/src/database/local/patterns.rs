use tauri::State;

use crate::database::Db;
use crate::models::patterns::PatternSummary;
use crate::models::schema::{Graph, PatternArgDef};

/// Core: fetch a pattern summary
pub async fn get_pattern_pool(pool: &sqlx::SqlitePool, id: i64) -> Result<PatternSummary, String> {
    let row = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch pattern: {}", e))?;

    Ok(row)
}

/// Tauri: fetch a pattern summary
#[tauri::command]
pub async fn get_pattern(db: State<'_, Db>, id: i64) -> Result<PatternSummary, String> {
    get_pattern_pool(&db.0, id).await
}

/// Core: list patterns
pub async fn list_patterns_pool(pool: &sqlx::SqlitePool) -> Result<Vec<PatternSummary>, String> {
    let rows = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         ORDER BY p.updated_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query patterns: {}", e))?;

    Ok(rows)
}

/// Tauri: list patterns
#[tauri::command]
pub async fn list_patterns(db: State<'_, Db>) -> Result<Vec<PatternSummary>, String> {
    list_patterns_pool(&db.0).await
}

/// Core: create a pattern
pub async fn create_pattern_pool(
    pool: &sqlx::SqlitePool,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    let id = sqlx::query("INSERT INTO patterns (name, description) VALUES (?, ?)")
        .bind(&name)
        .bind(&description)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create pattern: {}", e))?
        .last_insert_rowid();

    get_pattern_pool(pool, id).await
}

/// Tauri: create a pattern
#[tauri::command]
pub async fn create_pattern(
    db: State<'_, Db>,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    create_pattern_pool(&db.0, name, description).await
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
        .map_err(|e| format!("Failed to set pattern category: {}", e))?;

    Ok(())
}

/// Tauri: set pattern category
#[tauri::command]
pub async fn set_pattern_category(
    db: State<'_, Db>,
    pattern_id: i64,
    category_id: Option<i64>,
) -> Result<(), String> {
    set_pattern_category_pool(&db.0, pattern_id, category_id).await
}

/// Core: fetch a pattern graph
pub async fn get_pattern_graph_pool(pool: &sqlx::SqlitePool, id: i64) -> Result<String, String> {
    let default_graph: Option<(String,)> = sqlx::query_as(
        "SELECT i.graph_json
         FROM implementations i
         JOIN patterns p ON p.default_implementation_id = i.id
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch default implementation: {}", e))?;

    if let Some((graph_json,)) = default_graph {
        return Ok(graph_json);
    }

    let result: Option<(String,)> =
        sqlx::query_as("SELECT graph_json FROM implementations WHERE pattern_id = ? ORDER BY id")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to fetch pattern graph: {}", e))?;

    Ok(result
        .map(|row| row.0)
        .unwrap_or_else(|| "{\"nodes\":[],\"edges\":[],\"args\":[]}".to_string()))
}

/// Tauri: fetch a pattern graph
#[tauri::command]
pub async fn get_pattern_graph(db: State<'_, Db>, id: i64) -> Result<String, String> {
    get_pattern_graph_pool(&db.0, id).await
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

/// Tauri: fetch pattern arg defs
#[tauri::command]
pub async fn get_pattern_args(db: State<'_, Db>, id: i64) -> Result<Vec<PatternArgDef>, String> {
    get_pattern_args_pool(&db.0, id).await
}

/// Core: save pattern graph
pub async fn save_pattern_graph_pool(
    pool: &sqlx::SqlitePool,
    id: i64,
    graph_json: String,
) -> Result<(), String> {
    let default_id: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT default_implementation_id FROM patterns WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to fetch pattern default implementation: {}", e))?;

    if let Some((Some(default_id),)) = default_id {
        sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = ?")
            .bind(&graph_json)
            .bind(default_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to update pattern graph: {}", e))?;
        return Ok(());
    }

    let implementation_id =
        sqlx::query("INSERT INTO implementations (pattern_id, graph_json) VALUES (?, ?)")
            .bind(id)
            .bind(&graph_json)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to create implementation: {}", e))?
            .last_insert_rowid();

    sqlx::query("UPDATE patterns SET default_implementation_id = ? WHERE id = ?")
        .bind(implementation_id)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set default implementation: {}", e))?;

    Ok(())
}

/// Tauri: save pattern graph
#[tauri::command]
pub async fn save_pattern_graph(
    db: State<'_, Db>,
    id: i64,
    graph_json: String,
) -> Result<(), String> {
    save_pattern_graph_pool(&db.0, id, graph_json).await
}
