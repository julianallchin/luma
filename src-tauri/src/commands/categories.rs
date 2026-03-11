//! Tauri commands for pattern category operations

use tauri::State;

use crate::database::local::state::StateDb;
use crate::database::local::{auth, categories as db};
use crate::database::Db;
use crate::models::patterns::PatternCategory;

#[tauri::command]
pub async fn list_pattern_categories(db: State<'_, Db>) -> Result<Vec<PatternCategory>, String> {
    db::list_pattern_categories_pool(&db.0).await
}

#[tauri::command]
pub async fn create_pattern_category(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    name: String,
) -> Result<PatternCategory, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    db::create_pattern_category_pool(&db.0, name, uid).await
}
