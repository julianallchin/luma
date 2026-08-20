//! Tauri commands for fixture operations

use tauri::{AppHandle, State};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::Db;
use crate::models::fixtures::{FixtureDefinition, FixtureEntry, FixtureNode, PatchedFixture};
use crate::services::fixtures as fixture_service;
use crate::services::fixtures::FixtureState;
use crate::services::groups::invalidate_venue_fixture_cache;

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
pub fn get_fixture_definition(app: AppHandle, path: String) -> Result<FixtureDefinition, String> {
    fixture_service::get_fixture_definition(&app, path)
}

#[tauri::command]
pub async fn patch_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: String,
    universe: i64,
    address: i64,
    num_channels: i64,
    manufacturer: String,
    model: String,
    mode_name: String,
    fixture_path: String,
    label: Option<String>,
) -> Result<PatchedFixture, String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    let fixture = fixtures_db::insert_fixture(
        &mut access,
        universe,
        address,
        num_channels,
        &manufacturer,
        &model,
        &mode_name,
        &fixture_path,
        label.as_deref(),
    )
    .await?;
    let patch = fixtures_db::get_patched_fixtures(&mut access).await?;
    access.commit().await?;
    fixture_service::update_artnet_patch(&app, patch);
    invalidate_venue_fixture_cache();
    Ok(fixture)
}

#[tauri::command]
pub async fn get_patch_hierarchy(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<Vec<FixtureNode>, String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    fixture_service::get_patch_hierarchy(&app, &mut access).await
}

#[tauri::command]
pub async fn move_patched_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: String,
    id: String,
    address: i64,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Fixture(&id)).await?;
    access.require_venue(&venue_id)?;
    require_changed(fixtures_db::update_fixture_address(&mut access, &id, address).await?)?;
    let patch = fixtures_db::get_patched_fixtures(&mut access).await?;
    access.commit().await?;
    fixture_service::update_artnet_patch(&app, patch);
    invalidate_venue_fixture_cache();
    Ok(())
}

#[tauri::command]
pub async fn move_patched_fixture_spatial(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: String,
    id: String,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Fixture(&id)).await?;
    access.require_venue(&venue_id)?;
    require_changed(
        fixtures_db::update_fixture_spatial(
            &mut access,
            &id,
            pos_x,
            pos_y,
            pos_z,
            rot_x,
            rot_y,
            rot_z,
        )
        .await?,
    )?;
    let patch = fixtures_db::get_patched_fixtures(&mut access).await?;
    access.commit().await?;
    fixture_service::update_artnet_patch(&app, patch);
    invalidate_venue_fixture_cache();
    Ok(())
}

#[tauri::command]
pub async fn remove_patched_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: String,
    id: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Fixture(&id)).await?;
    access.require_venue(&venue_id)?;
    require_changed(fixtures_db::delete_fixture(&mut access, &id).await?)?;
    let patch = fixtures_db::get_patched_fixtures(&mut access).await?;
    access.commit().await?;
    fixture_service::update_artnet_patch(&app, patch);
    invalidate_venue_fixture_cache();
    Ok(())
}

#[tauri::command]
pub async fn rename_patched_fixture(
    app: AppHandle,
    db: State<'_, Db>,
    venue_id: String,
    id: String,
    label: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Fixture(&id)).await?;
    access.require_venue(&venue_id)?;
    require_changed(fixtures_db::update_fixture_label(&mut access, &id, &label).await?)?;
    let patch = fixtures_db::get_patched_fixtures(&mut access).await?;
    access.commit().await?;
    fixture_service::update_artnet_patch(&app, patch);
    invalidate_venue_fixture_cache();
    Ok(())
}

fn require_changed(rows_affected: u64) -> Result<(), String> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err("Venue resource not found".into())
    }
}
