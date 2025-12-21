//! Tauri commands for fixture operations

use tauri::{AppHandle, State};

use crate::database::Db;
use crate::fixtures::models::{FixtureDefinition, FixtureEntry, FixtureNode, PatchedFixture};
use crate::services::fixtures as fixture_service;
use crate::services::fixtures::FixtureState;

#[tauri::command]
pub async fn initialize_fixtures(
    app: AppHandle,
    state: State<'_, FixtureState>,
) -> Result<usize, String> {
    fixture_service::initialize_fixtures(&app, &state).await
}

#[tauri::command]
pub fn search_fixtures(
    query: String,
    offset: usize,
    limit: usize,
    state: State<'_, FixtureState>,
) -> Result<Vec<FixtureEntry>, String> {
    fixture_service::search_fixtures(query, offset, limit, &state)
}

#[tauri::command]
pub fn get_fixture_definition(
    app: AppHandle,
    path: String,
) -> Result<FixtureDefinition, String> {
    fixture_service::get_fixture_definition(&app, path)
}

#[tauri::command]
pub async fn patch_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
    universe: i64,
    address: i64,
    num_channels: i64,
    manufacturer: String,
    model: String,
    mode_name: String,
    fixture_path: String,
    label: Option<String>,
) -> Result<PatchedFixture, String> {
    fixture_service::patch_fixture(
        &app,
        &db.0,
        venue_id,
        universe,
        address,
        num_channels,
        manufacturer,
        model,
        mode_name,
        fixture_path,
        label,
    )
    .await
}

#[tauri::command]
pub async fn get_patched_fixtures(
    db: State<'_, Db>,
    venue_id: i64,
) -> Result<Vec<PatchedFixture>, String> {
    fixture_service::get_patched_fixtures(&db.0, venue_id).await
}

#[tauri::command]
pub async fn get_patch_hierarchy(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
) -> Result<Vec<FixtureNode>, String> {
    fixture_service::get_patch_hierarchy(&app, &db.0, venue_id).await
}

#[tauri::command]
pub async fn move_patched_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
    id: String,
    address: i64,
) -> Result<(), String> {
    fixture_service::move_patched_fixture(&app, &db.0, venue_id, id, address).await
}

#[tauri::command]
pub async fn move_patched_fixture_spatial(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
    id: String,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<(), String> {
    fixture_service::move_patched_fixture_spatial(
        &app, &db.0, venue_id, id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z,
    )
    .await
}

#[tauri::command]
pub async fn remove_patched_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
    id: String,
) -> Result<(), String> {
    fixture_service::remove_patched_fixture(&app, &db.0, venue_id, id).await
}

#[tauri::command]
pub async fn rename_patched_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
    id: String,
    label: String,
) -> Result<(), String> {
    fixture_service::rename_patched_fixture(&app, &db.0, venue_id, id, label).await
}
