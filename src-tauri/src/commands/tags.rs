//! Tauri commands for fixture tag operations

use tauri::State;

use crate::database::local::tags as tags_db;
use crate::database::Db;
use crate::models::fixtures::PatchedFixture;
use crate::models::tags::FixtureTag;
use crate::services::tags as tags_service;

// -----------------------------------------------------------------------------
// Tag CRUD
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn create_tag(
    db: State<'_, Db>,
    venue_id: i64,
    name: String,
    category: String,
) -> Result<FixtureTag, String> {
    tags_db::create_tag(&db.0, venue_id, &name, &category, false).await
}

#[tauri::command]
pub async fn list_tags_for_venue(
    db: State<'_, Db>,
    venue_id: i64,
) -> Result<Vec<FixtureTag>, String> {
    tags_db::list_tags(&db.0, venue_id).await
}

#[tauri::command]
pub async fn get_tag(db: State<'_, Db>, tag_id: i64) -> Result<FixtureTag, String> {
    tags_db::get_tag(&db.0, tag_id).await
}

#[tauri::command]
pub async fn update_tag(
    db: State<'_, Db>,
    tag_id: i64,
    name: String,
    category: String,
) -> Result<FixtureTag, String> {
    tags_db::update_tag(&db.0, tag_id, &name, &category).await
}

#[tauri::command]
pub async fn delete_tag(db: State<'_, Db>, tag_id: i64) -> Result<(), String> {
    tags_db::delete_tag(&db.0, tag_id).await
}

// -----------------------------------------------------------------------------
// Tag Assignments
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn assign_tag_to_fixture(
    db: State<'_, Db>,
    fixture_id: String,
    tag_id: i64,
) -> Result<(), String> {
    tags_db::assign_tag_to_fixture(&db.0, &fixture_id, tag_id).await
}

#[tauri::command]
pub async fn remove_tag_from_fixture(
    db: State<'_, Db>,
    fixture_id: String,
    tag_id: i64,
) -> Result<(), String> {
    tags_db::remove_tag_from_fixture(&db.0, &fixture_id, tag_id).await
}

#[tauri::command]
pub async fn get_tags_for_fixture(
    db: State<'_, Db>,
    fixture_id: String,
) -> Result<Vec<FixtureTag>, String> {
    tags_db::get_tags_for_fixture(&db.0, &fixture_id).await
}

#[tauri::command]
pub async fn get_fixtures_with_tag(
    db: State<'_, Db>,
    tag_id: i64,
) -> Result<Vec<PatchedFixture>, String> {
    tags_db::get_fixtures_with_tag(&db.0, tag_id).await
}

#[tauri::command]
pub async fn batch_assign_tag(
    db: State<'_, Db>,
    fixture_ids: Vec<String>,
    tag_id: i64,
) -> Result<(), String> {
    tags_db::batch_assign_tag(&db.0, &fixture_ids, tag_id).await
}

// -----------------------------------------------------------------------------
// Tag Auto-Generation
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn regenerate_spatial_tags(db: State<'_, Db>, venue_id: i64) -> Result<(), String> {
    tags_service::regenerate_spatial_tags(&db.0, venue_id).await
}

#[tauri::command]
pub async fn initialize_venue_tags(db: State<'_, Db>, venue_id: i64) -> Result<(), String> {
    tags_service::initialize_venue_tags(&db.0, venue_id).await
}
