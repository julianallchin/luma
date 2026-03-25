//! Tauri commands for fixture group operations

use tauri::{AppHandle, State};

use crate::database::local::groups as groups_db;
use crate::database::Db;
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{normalize_group_name, FixtureGroup, FixtureGroupNode, MovementConfig};
use crate::services::groups as groups_service;

// -----------------------------------------------------------------------------
// Group CRUD
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn create_group(
    db: State<'_, Db>,
    venue_id: String,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    // Check uniqueness of normalized name within the venue
    if let Some(ref n) = name {
        let normalized = normalize_group_name(n);
        if !normalized.is_empty() {
            let existing = groups_db::list_groups(&db.0, &venue_id).await?;
            for g in &existing {
                if let Some(ref existing_name) = g.name {
                    if normalize_group_name(existing_name) == normalized {
                        return Err(format!("A group with name '{}' already exists", normalized));
                    }
                }
            }
        }
    }
    groups_db::create_group(&db.0, &venue_id, name.as_deref(), axis_lr, axis_fb, axis_ab).await
}

#[tauri::command]
pub async fn get_group(db: State<'_, Db>, id: String) -> Result<FixtureGroup, String> {
    groups_db::get_group(&db.0, &id).await
}

#[tauri::command]
pub async fn list_groups(db: State<'_, Db>, venue_id: String) -> Result<Vec<FixtureGroup>, String> {
    groups_db::list_groups(&db.0, &venue_id).await
}

#[tauri::command]
pub async fn update_group(
    db: State<'_, Db>,
    id: String,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    // Check uniqueness of normalized name (excluding current group)
    if let Some(ref n) = name {
        let normalized = normalize_group_name(n);
        if !normalized.is_empty() {
            let current = groups_db::get_group(&db.0, &id).await?;
            let existing = groups_db::list_groups(&db.0, &current.venue_id).await?;
            for g in &existing {
                if g.id == id {
                    continue;
                }
                if let Some(ref existing_name) = g.name {
                    if normalize_group_name(existing_name) == normalized {
                        return Err(format!("A group with name '{}' already exists", normalized));
                    }
                }
            }
        }
    }
    groups_db::update_group(&db.0, &id, name.as_deref(), axis_lr, axis_fb, axis_ab).await
}

#[tauri::command]
pub async fn delete_group(db: State<'_, Db>, id: String) -> Result<(), String> {
    groups_db::delete_group(&db.0, &id).await
}

// -----------------------------------------------------------------------------
// Group Membership
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn add_fixture_to_group(
    db: State<'_, Db>,
    fixture_id: String,
    group_id: String,
) -> Result<(), String> {
    groups_db::add_fixture_to_group(&db.0, &fixture_id, &group_id).await
}

#[tauri::command]
pub async fn remove_fixture_from_group(
    db: State<'_, Db>,
    fixture_id: String,
    group_id: String,
) -> Result<(), String> {
    groups_db::remove_fixture_from_group(&db.0, &fixture_id, &group_id).await
}

#[tauri::command]
pub async fn get_fixtures_in_group(
    db: State<'_, Db>,
    group_id: String,
) -> Result<Vec<PatchedFixture>, String> {
    groups_db::get_fixtures_in_group(&db.0, &group_id).await
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
    venue_id: String,
) -> Result<Vec<FixtureGroupNode>, String> {
    groups_service::get_grouped_hierarchy(&app, &db.0, &venue_id).await
}

// -----------------------------------------------------------------------------
// Selection Query Preview
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn preview_selection_query(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: String,
    query: String,
    seed: Option<u64>,
) -> Result<Vec<PatchedFixture>, String> {
    let rng_seed = seed.unwrap_or(12345);
    let resource_path = groups_service::resolve_fixtures_root(&app)?;
    groups_service::resolve_selection_expression_with_path(
        &resource_path,
        &db.0,
        &venue_id,
        query.trim(),
        rng_seed,
    )
    .await
}

// -----------------------------------------------------------------------------
// Migration / Maintenance
// -----------------------------------------------------------------------------

/// Return all fixtures in a venue that are not assigned to any group.
#[tauri::command]
pub async fn get_ungrouped_fixtures(
    db: State<'_, Db>,
    venue_id: String,
) -> Result<Vec<PatchedFixture>, String> {
    groups_db::get_ungrouped_fixtures(&db.0, &venue_id).await
}

// -----------------------------------------------------------------------------
// Movement Config
// -----------------------------------------------------------------------------

#[tauri::command]
pub async fn update_movement_config(
    db: State<'_, Db>,
    group_id: String,
    config: Option<MovementConfig>,
) -> Result<FixtureGroup, String> {
    groups_db::update_movement_config(&db.0, &group_id, config.as_ref()).await
}
