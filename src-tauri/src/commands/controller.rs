use tauri::{AppHandle, State};

use crate::controller_manager::ControllerManager;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venues as venues_db;
use crate::database::Db;
use crate::models::midi::{ControllerState, ControllerStatus};
use crate::render_engine::RenderEngine;

// ============================================================================
// Device Connection
// ============================================================================

#[tauri::command]
pub fn controller_list_ports(
    controller: State<'_, ControllerManager>,
) -> Result<Vec<String>, String> {
    controller.list_ports()
}

#[tauri::command]
pub async fn controller_connect(
    app: AppHandle,
    controller: State<'_, ControllerManager>,
    db: State<'_, Db>,
    port_name: String,
    venue_id: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    controller.connect(&port_name, app)?;
    venues_db::set_controller_port(&mut access, Some(&port_name)).await?;
    access.commit().await
}

#[tauri::command]
pub async fn controller_disconnect(
    controller: State<'_, ControllerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    controller.disconnect()?;
    venues_db::set_controller_port(&mut access, None).await?;
    access.commit().await
}

#[tauri::command]
pub async fn controller_get_status(
    controller: State<'_, ControllerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<ControllerStatus, String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(controller.status())
}

/// Called when a venue loads. Restores the saved preferred port so
/// auto-reconnect works without the user opening the controller config.
#[tauri::command]
pub async fn controller_init_for_venue(
    app: AppHandle,
    controller: State<'_, ControllerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = venues_db::get_venue(&mut access).await?;
    controller.set_preferred_port(venue.controller_port, app);
    Ok(())
}

// ============================================================================
// Learn Mode
// ============================================================================

#[tauri::command]
pub async fn controller_start_learn(
    app: AppHandle,
    controller: State<'_, ControllerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    controller.start_learn(app)
}

#[tauri::command]
pub async fn controller_cancel_learn(
    controller: State<'_, ControllerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    controller.cancel_learn()
}

// ============================================================================
// Manual Layer State
// ============================================================================

#[tauri::command]
pub async fn controller_set_active(
    render_engine: State<'_, RenderEngine>,
    db: State<'_, Db>,
    venue_id: String,
    active: bool,
) -> Result<(), String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    render_engine.set_manual_active(active);
    Ok(())
}

#[tauri::command]
pub async fn controller_get_state(
    render_engine: State<'_, RenderEngine>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<ControllerState, String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(render_engine.get_manual_state_snapshot())
}
