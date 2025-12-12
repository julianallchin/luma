use tauri::State;

use crate::database::{Db, ProjectDb};
use crate::models::patterns::PatternSummary;
use crate::models::schema::{Graph, PatternArgDef};

#[tauri::command]
pub async fn get_pattern(db: State<'_, Db>, id: i64) -> Result<PatternSummary, String> {
    let row = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch pattern: {}", e))?;

    Ok(row)
}

#[tauri::command]
pub async fn list_patterns(db: State<'_, Db>) -> Result<Vec<PatternSummary>, String> {
    let rows = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         ORDER BY p.updated_at DESC"
    )
    .fetch_all(&db.0)
    .await
    .map_err(|e| format!("Failed to query patterns: {}", e))?;

    Ok(rows)
}

#[tauri::command]
pub async fn create_pattern(
    db: State<'_, Db>,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    // 1. Create the pattern definition in the Global DB
    let id = sqlx::query("INSERT INTO patterns (name, description) VALUES (?, ?)")
        .bind(&name)
        .bind(&description)
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to create pattern: {}", e))?
        .last_insert_rowid();

    let row = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch created pattern: {}", e))?;

    Ok(row)
}

#[tauri::command]
pub async fn set_pattern_category(
    db: State<'_, Db>,
    pattern_id: i64,
    category_id: Option<i64>,
) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET category_id = ? WHERE id = ?")
        .bind(category_id)
        .bind(pattern_id)
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to set pattern category: {}", e))?;

    Ok(())
}


#[tauri::command]
pub async fn get_pattern_graph(
    _db: State<'_, Db>, // Global DB not needed for graph content anymore
    project_db: State<'_, ProjectDb>,
    id: i64,
) -> Result<String, String> {
    // 1. Get Project DB lock
    let lock = project_db.0.lock().await;
    let pool = lock.as_ref().ok_or("No project currently open")?;

    // 2. Check if implementation exists in Project DB
    let result: Option<(String,)> =
        sqlx::query_as("SELECT graph_json FROM implementations WHERE pattern_id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                eprintln!("[Rust] get_pattern_graph error: {}", e);
                format!("Failed to fetch pattern graph: {}", e)
            })?;

    match result {
        Some(row) => Ok(row.0),
        None => Ok("{\"nodes\":[],\"edges\":[],\"args\":[]}".to_string()), // Default empty graph
    }
}

#[tauri::command]
pub async fn get_pattern_args(
    _db: State<'_, Db>,
    project_db: State<'_, ProjectDb>,
    id: i64,
) -> Result<Vec<PatternArgDef>, String> {
    let graph_json = get_pattern_graph(_db, project_db, id).await?;
    let graph: Graph = serde_json::from_str(&graph_json).unwrap_or(Graph {
        nodes: vec![],
        edges: vec![],
        args: vec![],
    });
    Ok(graph.args)
}

#[tauri::command]
pub async fn save_pattern_graph(
    _db: State<'_, Db>,
    project_db: State<'_, ProjectDb>,
    id: i64,
    graph_json: String,
) -> Result<(), String> {
    // 1. Get Project DB lock
    let lock = project_db.0.lock().await;
    let pool = lock.as_ref().ok_or("No project currently open")?;

    // 2. Upsert into implementations table
    sqlx::query(
        "INSERT INTO implementations (pattern_id, graph_json, updated_at) 
         VALUES (?, ?, datetime('now'))
         ON CONFLICT(pattern_id) DO UPDATE SET graph_json = ?, updated_at = datetime('now')",
    )
    .bind(id)
    .bind(&graph_json)
    .bind(&graph_json)
    .execute(pool)
    .await
    .map_err(|e| {
        eprintln!("[Rust] save_pattern_graph error: {}", e);
        format!("Failed to save pattern graph: {}", e)
    })?;

    Ok(())
}
