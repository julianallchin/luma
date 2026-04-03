use tauri::{AppHandle, State};

use crate::controller_manager::ControllerManager;
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
    controller.connect(&port_name, app)?;
    venues_db::set_controller_port(&db.0, &venue_id, Some(&port_name)).await?;
    Ok(())
}

#[tauri::command]
pub async fn controller_disconnect(
    controller: State<'_, ControllerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    controller.disconnect()?;
    venues_db::set_controller_port(&db.0, &venue_id, None).await?;
    Ok(())
}

#[tauri::command]
pub fn controller_get_status(
    controller: State<'_, ControllerManager>,
) -> Result<ControllerStatus, String> {
    Ok(controller.status())
}

/// Called when a venue loads. Restores the saved preferred port so
/// auto-reconnect works without the user opening the controller config.
#[tauri::command]
pub fn controller_init_for_venue(
    app: AppHandle,
    controller: State<'_, ControllerManager>,
    controller_port: Option<String>,
) -> Result<(), String> {
    controller.set_preferred_port(controller_port, app);
    Ok(())
}

// ============================================================================
// Learn Mode
// ============================================================================

#[tauri::command]
pub fn controller_start_learn(
    app: AppHandle,
    controller: State<'_, ControllerManager>,
) -> Result<(), String> {
    controller.start_learn(app)
}

#[tauri::command]
pub fn controller_cancel_learn(controller: State<'_, ControllerManager>) -> Result<(), String> {
    controller.cancel_learn()
}

// ============================================================================
// Manual Layer State
// ============================================================================

#[tauri::command]
pub fn controller_set_active(
    render_engine: State<'_, RenderEngine>,
    active: bool,
) -> Result<(), String> {
    render_engine.set_manual_active(active);
    Ok(())
}

#[tauri::command]
pub fn controller_get_state(
    render_engine: State<'_, RenderEngine>,
) -> Result<ControllerState, String> {
    Ok(render_engine.get_manual_state_snapshot())
}
