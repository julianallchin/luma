use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tauri::State;
use ts_rs::TS;

use crate::database::Db;
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternSummary {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

#[allow(dead_code)]
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternDetail {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub graph_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[tauri::command]
pub async fn list_patterns(db: State<'_, Db>) -> Result<Vec<PatternSummary>, String> {
    let rows = sqlx::query_as::<_, PatternSummary>(
        "SELECT id, name, description, created_at, updated_at FROM patterns ORDER BY updated_at DESC"
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
    let id = sqlx::query("INSERT INTO patterns (name, description) VALUES (?, ?)")
        .bind(&name)
        .bind(&description)
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to create pattern: {}", e))?
        .last_insert_rowid();

    let row = sqlx::query_as::<_, PatternSummary>(
        "SELECT id, name, description, created_at, updated_at FROM patterns WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch created pattern: {}", e))?;

    Ok(row)
}

#[tauri::command]
pub async fn get_pattern_graph(db: State<'_, Db>, id: i64) -> Result<String, String> {
    let row: (String,) = sqlx::query_as("SELECT graph_json FROM patterns WHERE id = ?")
        .bind(id)
        .fetch_one(&db.0)
        .await
        .map_err(|e| {
            eprintln!("[Rust] get_pattern_graph error: {}", e);
            format!("Failed to fetch pattern graph: {}", e)
        })?;

    Ok(row.0)
}

#[tauri::command]
pub async fn save_pattern_graph(
    db: State<'_, Db>,
    id: i64,
    graph_json: String,
) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET graph_json = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(&graph_json)
        .bind(id)
        .execute(&db.0)
        .await
        .map_err(|e| {
            eprintln!("[Rust] save_pattern_graph error: {}", e);
            format!("Failed to save pattern graph: {}", e)
        })?;

    Ok(())
}
