//! Tauri commands for pattern operations

use tauri::State;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::patterns as db;
use crate::database::local::state::StateDb;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::queries as remote_queries;
use crate::database::Db;
use crate::models::node_graph::PatternArgDef;
use crate::models::patterns::PatternSummary;
use crate::services::cloud_sync::CloudSync;

/// Fire-and-forget background sync of a pattern + implementations to the cloud.
/// Does nothing if not authenticated. Logs errors but never fails the caller.
fn spawn_background_sync(pool: sqlx::SqlitePool, state_pool: sqlx::SqlitePool, pattern_id: String) {
    tokio::spawn(async move {
        let token = match auth::get_current_access_token(&state_pool).await {
            Ok(Some(t)) => t,
            _ => return, // not authenticated — skip
        };
        let uid = match auth::get_current_user_id(&state_pool).await {
            Ok(Some(u)) => u,
            _ => return,
        };
        let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
        let sync = CloudSync::new(&pool, &client, &token, &uid);
        if let Err(e) = sync.sync_pattern_with_children(&pattern_id).await {
            eprintln!("[auto-sync] Failed to sync pattern {}: {}", pattern_id, e);
        }
    });
}

#[tauri::command]
pub async fn get_pattern(db: State<'_, Db>, id: String) -> Result<PatternSummary, String> {
    db::get_pattern_pool(&db.0, &id).await
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
    spawn_background_sync(db.0.clone(), state_db.0.clone(), pattern.id.clone());
    Ok(pattern)
}

#[tauri::command]
pub async fn update_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    id: String,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    let pattern = db::update_pattern_pool(&db.0, &id, name, description).await?;
    spawn_background_sync(db.0.clone(), state_db.0.clone(), id);
    Ok(pattern)
}

#[tauri::command]
pub async fn set_pattern_category(
    db: State<'_, Db>,
    pattern_id: String,
    category_name: Option<String>,
) -> Result<(), String> {
    db::set_pattern_category_pool(&db.0, &pattern_id, category_name.as_deref()).await
}

#[tauri::command]
pub async fn get_pattern_graph(db: State<'_, Db>, id: String) -> Result<String, String> {
    db::get_pattern_graph_pool(&db.0, &id).await
}

#[tauri::command]
pub async fn get_pattern_args(db: State<'_, Db>, id: String) -> Result<Vec<PatternArgDef>, String> {
    db::get_pattern_args_pool(&db.0, &id).await
}

#[tauri::command]
pub async fn save_pattern_graph(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    id: String,
    graph_json: String,
) -> Result<(), String> {
    db::save_pattern_graph_pool(&db.0, &id, graph_json).await?;
    spawn_background_sync(db.0.clone(), state_db.0.clone(), id);
    Ok(())
}

#[tauri::command]
pub async fn delete_pattern(db: State<'_, Db>, id: String) -> Result<(), String> {
    db::delete_pattern_pool(&db.0, &id).await?;

    // Enqueue soft-delete for the sync push loop
    if let Err(e) = crate::sync::pending::enqueue_delete(&db.0, "patterns", &id, "id", 1).await {
        eprintln!("[delete_pattern] Failed to enqueue delete: {e}");
    }

    Ok(())
}

#[tauri::command]
pub async fn verify_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    id: String,
    verify: bool,
) -> Result<PatternSummary, String> {
    // 1. Get current user uid, verify pattern ownership
    let uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let pattern = db::get_pattern_pool(&db.0, &id).await?;
    if pattern.uid.as_deref() != Some(&uid) {
        return Err("You can only verify your own patterns".to_string());
    }

    // 2. Get access token
    let token = auth::get_current_access_token(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated - please sign in first".to_string())?;

    // 3. Fetch display_name from profiles
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let display_name = remote_queries::fetch_user_profile(&client, &uid, &token)
        .await
        .map_err(|e| format!("Failed to fetch profile: {}", e))?
        .unwrap_or_else(|| uid.clone());

    // 4. Set author_name and verified state
    db::set_author_name(&db.0, &id, &display_name).await?;
    db::set_verified(&db.0, &id, verify).await?;

    // 5. Sync pattern + implementations to cloud
    let sync = CloudSync::new(&db.0, &client, &token, &uid);
    sync.sync_pattern_with_children(&id)
        .await
        .map_err(|e| format!("Failed to sync pattern: {}", e))?;

    // 6. Return updated pattern
    db::get_pattern_pool(&db.0, &id).await
}

#[tauri::command]
pub async fn fork_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    source_pattern_id: String,
) -> Result<PatternSummary, String> {
    // 1. Read source pattern + graph_json
    let source = db::get_pattern_pool(&db.0, &source_pattern_id).await?;
    let graph_json = db::get_pattern_graph_pool(&db.0, &source_pattern_id).await?;

    // 2. Get current user uid
    let uid = auth::get_current_user_id(&state_db.0).await?;

    // 3. Create new pattern
    let fork_name = format!("{}_fork", source.name);
    let new_pattern =
        db::create_pattern_pool(&db.0, fork_name, source.description.clone(), uid).await?;

    // 4. Set forked_from_id (the source pattern's UUID)
    sqlx::query("UPDATE patterns SET forked_from_id = ? WHERE id = ?")
        .bind(&source.id)
        .bind(&new_pattern.id)
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to set forked_from_id: {}", e))?;

    // 5. Copy graph_json into new implementation
    db::save_pattern_graph_pool(&db.0, &new_pattern.id, graph_json).await?;

    // 6. Return the new pattern
    db::get_pattern_pool(&db.0, &new_pattern.id).await
}
