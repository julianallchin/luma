//! Tauri commands for pattern operations

use tauri::State;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::patterns as db;
use crate::database::local::state::StateDb;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::queries as remote_queries;
use crate::database::Db;
use crate::models::patterns::{ForkPatternInput, ForkPatternResult, PatternSummary};
use crate::services::authored_documents::AuthoredDocuments;
use crate::services::graph_documents::{load_visible_graph_document, GraphDocument};
use crate::sync::orchestrator::SyncEngine;

#[tauri::command]
pub async fn get_pattern(db: State<'_, Db>, id: String) -> Result<PatternSummary, String> {
    db::get_pattern_pool(&db.0, &id).await
}

#[tauri::command]
pub async fn create_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    authored: State<'_, AuthoredDocuments>,
    engine: State<'_, SyncEngine>,
    request_id: String,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    let pattern = authored
        .create_pattern(&db.0, uid.as_deref(), &request_id, name, description)
        .await
        .map_err(|error| error.to_string())?;
    engine.push_notify.notify_one();
    Ok(pattern)
}

#[tauri::command]
pub async fn update_pattern(
    db: State<'_, Db>,
    engine: State<'_, SyncEngine>,
    id: String,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    let pattern = db::update_pattern_pool(&db.0, &id, name, description).await?;
    engine.push_notify.notify_one();
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
pub async fn get_pattern_graph_document(
    db: State<'_, Db>,
    id: String,
    implementation_id: Option<String>,
) -> Result<GraphDocument, String> {
    db::get_pattern_pool(&db.0, &id).await?;
    load_visible_graph_document(&db.0, &id, None, implementation_id.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    authored: State<'_, AuthoredDocuments>,
    engine: State<'_, SyncEngine>,
    id: String,
) -> Result<(), String> {
    let principal = auth::get_current_user_id(&state_db.0).await?;
    authored
        .archive_pattern(&db.0, principal.as_deref(), &id)
        .await
        .map_err(|error| error.to_string())?;
    engine.push_notify.notify_one();
    Ok(())
}

#[tauri::command]
pub async fn verify_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    engine: State<'_, SyncEngine>,
    id: String,
    verify: bool,
) -> Result<PatternSummary, String> {
    // 1. Get current user uid, verify pattern ownership
    let auth = auth::get_current_auth(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let uid = auth.principal.user_id;
    let token = auth.access_token;
    let pattern = db::get_pattern_pool(&db.0, &id).await?;
    if pattern.uid.as_deref() != Some(&uid) {
        return Err("You can only verify your own patterns".to_string());
    }

    // 2. Fetch display_name from profiles
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let display_name = remote_queries::fetch_user_profile(&client, &uid, &token)
        .await
        .map_err(|e| format!("Failed to fetch profile: {}", e))?
        .unwrap_or_else(|| uid.clone());

    // 3. Set author_name and verified state (updates updated_at → marks dirty)
    db::set_author_name(&db.0, &id, &display_name).await?;
    db::set_verified(&db.0, &id, verify).await?;

    // 4. Push immediately so other users see the verified state
    engine
        .run_push(&uid)
        .await
        .map_err(|e| format!("Failed to sync pattern: {}", e))?;

    // 5. Return updated pattern
    db::get_pattern_pool(&db.0, &id).await
}

#[tauri::command]
pub async fn fork_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    authored: State<'_, AuthoredDocuments>,
    engine: State<'_, SyncEngine>,
    input: ForkPatternInput,
) -> Result<ForkPatternResult, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    let result = authored
        .fork_pattern(&db.0, uid.as_deref(), input)
        .await
        .map_err(|error| error.to_string())?;
    engine.push_notify.notify_one();
    Ok(result)
}
