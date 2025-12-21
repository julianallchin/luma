//! Tauri commands for venue operations

use tauri::State;

use crate::database::local::venues as db;
use crate::database::Db;
use crate::models::venues::Venue;

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
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    db::create_venue(&db.0, name, description).await
}

#[tauri::command]
pub async fn update_venue(
    db: State<'_, Db>,
    id: i64,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    db::update_venue(&db.0, id, name, description).await
}

#[tauri::command]
pub async fn delete_venue(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db::delete_venue(&db.0, id).await
}
