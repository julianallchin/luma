//! Tauri commands for venue operations

use tauri::State;

use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::venues as db;
use crate::database::Db;
use crate::models::venues::Venue;
use crate::services::sync;

#[tauri::command]
pub async fn get_venue(db: State<'_, Db>, id: i64) -> Result<Venue, String> {
    db::get_venue(&db.0, id).await
}

#[tauri::command]
pub async fn list_venues(db: State<'_, Db>) -> Result<Vec<Venue>, String> {
    db::list_venues(&db.0).await
}

#[tauri::command]
pub async fn create_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    let venue = db::create_venue(&db.0, name, description, uid).await?;

    if let Ok(Some(token)) = auth::get_current_access_token(&state_db.0).await {
        let venue_clone = venue.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = sync::push_venue(&venue_clone, &token).await {
                eprintln!("[sync] Failed to push venue: {}", e);
            }
        });
    }

    Ok(venue)
}

#[tauri::command]
pub async fn update_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    id: i64,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    let venue = db::update_venue(&db.0, id, name, description).await?;

    if let Ok(Some(token)) = auth::get_current_access_token(&state_db.0).await {
        let venue_clone = venue.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = sync::push_venue(&venue_clone, &token).await {
                eprintln!("[sync] Failed to push venue: {}", e);
            }
        });
    }

    Ok(venue)
}

#[tauri::command]
pub async fn delete_venue(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db::delete_venue(&db.0, id).await
}
