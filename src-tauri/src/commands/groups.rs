//! Tauri commands for fixture group operations

use tauri::{AppHandle, State};

use crate::database::local::groups as groups_db;
use crate::database::Db;
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{FixtureGroup, FixtureGroupNode, SelectionQuery, PREDEFINED_TAGS};
use crate::services::groups as groups_service;

// -----------------------------------------------------------------------------
// Group CRUD
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn create_group(
    db: State<'_, Db>,
    venue_id: i64,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    groups_db::create_group(&db.0, venue_id, name.as_deref(), axis_lr, axis_fb, axis_ab).await
}

#[tauri::command]
pub async fn get_group(db: State<'_, Db>, id: i64) -> Result<FixtureGroup, String> {
    groups_db::get_group(&db.0, id).await
}

#[tauri::command]
pub async fn list_groups(db: State<'_, Db>, venue_id: i64) -> Result<Vec<FixtureGroup>, String> {
    groups_db::list_groups(&db.0, venue_id).await
}

#[tauri::command]
pub async fn update_group(
    db: State<'_, Db>,
    id: i64,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    groups_db::update_group(&db.0, id, name.as_deref(), axis_lr, axis_fb, axis_ab).await
}

#[tauri::command]
pub async fn delete_group(db: State<'_, Db>, id: i64) -> Result<(), String> {
    groups_db::delete_group(&db.0, id).await
}

// -----------------------------------------------------------------------------
// Group Membership
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn add_fixture_to_group(
    db: State<'_, Db>,
    fixture_id: String,
    group_id: i64,
) -> Result<(), String> {
    groups_db::add_fixture_to_group(&db.0, &fixture_id, group_id).await
}

#[tauri::command]
pub async fn remove_fixture_from_group(
    db: State<'_, Db>,
    fixture_id: String,
    group_id: i64,
) -> Result<(), String> {
    groups_db::remove_fixture_from_group(&db.0, &fixture_id, group_id).await
}

#[tauri::command]
pub async fn get_fixtures_in_group(
    db: State<'_, Db>,
    group_id: i64,
) -> Result<Vec<PatchedFixture>, String> {
    groups_db::get_fixtures_in_group(&db.0, group_id).await
}

#[tauri::command]
pub async fn get_groups_for_fixture(
    db: State<'_, Db>,
    fixture_id: String,
) -> Result<Vec<FixtureGroup>, String> {
    groups_db::get_groups_for_fixture(&db.0, &fixture_id).await
}

// -----------------------------------------------------------------------------
// Hierarchy
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn get_grouped_hierarchy(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
) -> Result<Vec<FixtureGroupNode>, String> {
    groups_service::get_grouped_hierarchy(&app, &db.0, venue_id).await
}

// -----------------------------------------------------------------------------
// Selection Query Preview
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn preview_selection_query(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: i64,
    query: String,
    seed: Option<u64>,
) -> Result<Vec<PatchedFixture>, String> {
    let rng_seed = seed.unwrap_or(12345);
    let trimmed = query.trim();
    if trimmed.starts_with('{') {
        if let Ok(selection_query) = serde_json::from_str::<SelectionQuery>(trimmed) {
            return groups_service::resolve_selection_query(
                &app,
                &db.0,
                venue_id,
                &selection_query,
                rng_seed,
            )
            .await;
        }
    }
    let resource_path = groups_service::resolve_fixtures_root(&app)?;
    groups_service::resolve_selection_expression_with_path(
        &resource_path,
        &db.0,
        venue_id,
        trimmed,
        rng_seed,
    )
    .await
}

// -----------------------------------------------------------------------------
// Migration / Maintenance
// -----------------------------------------------------------------------------

/// Ensure all fixtures in a venue are assigned to at least one group.
/// Assigns ungrouped fixtures to the default group.
#[tauri::command]
pub async fn ensure_fixtures_grouped(db: State<'_, Db>, venue_id: i64) -> Result<i64, String> {
    let ungrouped = groups_db::get_ungrouped_fixtures(&db.0, venue_id).await?;
    let count = ungrouped.len() as i64;

    if count > 0 {
        let default_group = groups_db::get_or_create_default_group(&db.0, venue_id).await?;
        for fixture in ungrouped {
            groups_db::add_fixture_to_group(&db.0, &fixture.id, default_group.id).await?;
        }
    }

    Ok(count)
}

// -----------------------------------------------------------------------------
// Group Tags
// -----------------------------------------------------------------------------

/// Get the list of predefined tags
#[tauri::command]
pub fn get_predefined_tags() -> Vec<String> {
    PREDEFINED_TAGS.iter().map(|s| s.to_string()).collect()
}

/// Add a tag to a group
#[tauri::command]
pub async fn add_tag_to_group(
    db: State<'_, Db>,
    group_id: i64,
    tag: String,
) -> Result<FixtureGroup, String> {
    // Validate tag is in predefined list
    if !PREDEFINED_TAGS.contains(&tag.as_str()) {
        return Err(format!("Invalid tag: {}. Must be one of: {:?}", tag, PREDEFINED_TAGS));
    }
    groups_db::add_tag_to_group(&db.0, group_id, &tag).await
}

/// Remove a tag from a group
#[tauri::command]
pub async fn remove_tag_from_group(
    db: State<'_, Db>,
    group_id: i64,
    tag: String,
) -> Result<FixtureGroup, String> {
    groups_db::remove_tag_from_group(&db.0, group_id, &tag).await
}

/// Set all tags for a group
#[tauri::command]
pub async fn set_group_tags(
    db: State<'_, Db>,
    group_id: i64,
    tags: Vec<String>,
) -> Result<FixtureGroup, String> {
    // Validate all tags
    for tag in &tags {
        if !PREDEFINED_TAGS.contains(&tag.as_str()) {
            return Err(format!("Invalid tag: {}. Must be one of: {:?}", tag, PREDEFINED_TAGS));
        }
    }
    groups_db::set_group_tags(&db.0, group_id, &tags).await
}
