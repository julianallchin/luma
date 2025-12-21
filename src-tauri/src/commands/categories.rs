//! Tauri commands for pattern category operations

use tauri::State;

use crate::database::local::categories as db;
use crate::database::Db;
use crate::models::patterns::PatternCategory;

#[tauri::command]
pub async fn list_pattern_categories(
    db: State<'_, Db>,
) -> Result<Vec<PatternCategory>, String> {
    db::list_pattern_categories_pool(&db.0).await
}

#[tauri::command]
pub async fn create_pattern_category(
    db: State<'_, Db>,
    name: String,
) -> Result<PatternCategory, String> {
    db::create_pattern_category_pool(&db.0, name).await
}
