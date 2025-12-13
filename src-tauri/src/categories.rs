use tauri::State;

use crate::database::Db;
use crate::models::patterns::PatternCategory;

#[tauri::command]
pub async fn list_pattern_categories(db: State<'_, Db>) -> Result<Vec<PatternCategory>, String> {
    let rows = sqlx::query_as::<_, PatternCategory>(
        "SELECT id, name, created_at, updated_at FROM pattern_categories ORDER BY lower(name) ASC",
    )
    .fetch_all(&db.0)
    .await
    .map_err(|e| format!("Failed to query pattern categories: {}", e))?;

    Ok(rows)
}

#[tauri::command]
pub async fn create_pattern_category(
    db: State<'_, Db>,
    name: String,
) -> Result<PatternCategory, String> {
    let id = sqlx::query("INSERT INTO pattern_categories (name) VALUES (?)")
        .bind(&name)
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to create category: {}", e))?
        .last_insert_rowid();

    let row = sqlx::query_as::<_, PatternCategory>(
        "SELECT id, name, created_at, updated_at FROM pattern_categories WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch created category: {}", e))?;

    Ok(row)
}
