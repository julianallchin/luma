//! Tauri commands for pattern operations

use tauri::State;

use crate::database::local::auth;
use crate::database::local::patterns as db;
use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::models::patterns::PatternSummary;
use crate::models::schema::PatternArgDef;
use crate::services::sync;

#[tauri::command]
pub async fn get_pattern(db: State<'_, Db>, id: i64) -> Result<PatternSummary, String> {
    db::get_pattern_pool(&db.0, id).await
}

#[tauri::command]
pub async fn list_patterns(db: State<'_, Db>) -> Result<Vec<PatternSummary>, String> {
    db::list_patterns_pool(&db.0).await
}

#[tauri::command]
pub async fn create_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    let pattern = db::create_pattern_pool(&db.0, name, description, uid).await?;

    if let Ok(Some(token)) = auth::get_current_access_token(&state_db.0).await {
        let pattern_clone = pattern.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = sync::push_pattern(&pattern_clone, &token).await {
                eprintln!("[sync] Failed to push pattern: {}", e);
            }
        });
    }

    Ok(pattern)
}

#[tauri::command]
pub async fn set_pattern_category(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    pattern_id: i64,
    category_id: Option<i64>,
) -> Result<(), String> {
    db::set_pattern_category_pool(&db.0, pattern_id, category_id).await?;

    if let Ok(Some(token)) = auth::get_current_access_token(&state_db.0).await {
        if let Ok(pattern) = db::get_pattern_pool(&db.0, pattern_id).await {
            tauri::async_runtime::spawn(async move {
                if let Err(e) = sync::push_pattern(&pattern, &token).await {
                    eprintln!("[sync] Failed to push pattern: {}", e);
                }
            });
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn get_pattern_graph(db: State<'_, Db>, id: i64) -> Result<String, String> {
    db::get_pattern_graph_pool(&db.0, id).await
}

#[tauri::command]
pub async fn get_pattern_args(db: State<'_, Db>, id: i64) -> Result<Vec<PatternArgDef>, String> {
    db::get_pattern_args_pool(&db.0, id).await
}

#[tauri::command]
pub async fn save_pattern_graph(
    db: State<'_, Db>,
    id: i64,
    graph_json: String,
) -> Result<(), String> {
    db::save_pattern_graph_pool(&db.0, id, graph_json).await
}
