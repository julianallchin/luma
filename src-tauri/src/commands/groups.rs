//! Tauri commands for fixture group operations

use tauri::{AppHandle, State};

use crate::database::local::groups as groups_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::Db;
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{normalize_group_name, FixtureGroup, FixtureGroupNode, MovementConfig};
use crate::services::groups as groups_service;
use crate::services::groups::invalidate_venue_fixture_cache;

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
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    // Check uniqueness of normalized name within the venue
    if let Some(ref n) = name {
        let normalized = normalize_group_name(n);
        if !normalized.is_empty() {
            let existing = groups_db::list_groups(&mut access).await?;
            for g in &existing {
                if let Some(ref existing_name) = g.name {
                    if normalize_group_name(existing_name) == normalized {
                        return Err(format!("A group with name '{}' already exists", normalized));
                    }
                }
            }
        }
    }
    let result =
        groups_db::create_group(&mut access, name.as_deref(), axis_lr, axis_fb, axis_ab).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(result)
}

#[tauri::command]
pub async fn get_group(db: State<'_, Db>, id: String) -> Result<FixtureGroup, String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Group(&id)).await?;
    groups_db::get_group(&mut access, &id).await
}

#[tauri::command]
pub async fn list_groups(db: State<'_, Db>, venue_id: String) -> Result<Vec<FixtureGroup>, String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    groups_db::list_groups(&mut access).await
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
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Group(&id)).await?;
    // Check uniqueness of normalized name (excluding current group)
    if let Some(ref n) = name {
        let normalized = normalize_group_name(n);
        if !normalized.is_empty() {
            let existing = groups_db::list_groups(&mut access).await?;
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
    let result =
        groups_db::update_group(&mut access, &id, name.as_deref(), axis_lr, axis_fb, axis_ab)
            .await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(result)
}

#[tauri::command]
pub async fn delete_group(db: State<'_, Db>, id: String) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Group(&id)).await?;
    require_changed(groups_db::delete_group(&mut access, &id).await?)?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(())
}

// -----------------------------------------------------------------------------
// Group Membership
// -----------------------------------------------------------------------------

/// Add a whole fixture (head_index = None) or a single head to a group.
#[tauri::command]
pub async fn add_fixture_to_group(
    db: State<'_, Db>,
    fixture_id: String,
    group_id: String,
    head_index: Option<i64>,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Group(&group_id)).await?;
    let head = head_index.unwrap_or(groups_db::WHOLE_FIXTURE);
    groups_db::add_member_to_group(&mut access, &fixture_id, &group_id, head).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(())
}

/// Remove a whole fixture (head_index = None, drops per-head rows too) or a
/// single head from a group. Removing a head from a whole-fixture membership
/// splits it into per-head rows for the remaining heads.
#[tauri::command]
pub async fn remove_fixture_from_group(
    app: AppHandle,
    db: State<'_, Db>,
    fixture_id: String,
    group_id: String,
    head_index: Option<i64>,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Group(&group_id)).await?;
    let result = match head_index {
        None => {
            groups_db::remove_member_from_group(&mut access, &fixture_id, &group_id, None).await
        }
        Some(head) => {
            let resource_path = groups_service::resolve_fixtures_root(&app)?;
            groups_service::remove_head_from_group(
                &resource_path,
                &mut access,
                &fixture_id,
                &group_id,
                head,
            )
            .await
        }
    }?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(result)
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
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    groups_service::get_grouped_hierarchy(&app, &mut access).await
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
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    let rng_seed = seed.unwrap_or(12345);
    let resource_path = groups_service::resolve_fixtures_root(&app)?;
    let resolved = groups_service::resolve_selection_expression_with_path(
        &resource_path,
        &mut access,
        query.trim(),
        rng_seed,
    )
    .await?;
    Ok(resolved.into_iter().map(|r| r.fixture).collect())
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
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    groups_db::get_ungrouped_fixtures(&mut access).await
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
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Group(&group_id)).await?;
    let group = groups_db::update_movement_config(&mut access, &group_id, config.as_ref()).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(group)
}

fn require_changed(rows_affected: u64) -> Result<(), String> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err("Venue resource not found".into())
    }
}
