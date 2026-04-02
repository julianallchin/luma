use tauri::{AppHandle, State};

use crate::controller_manager::ControllerManager;
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
pub fn controller_connect(
    app: AppHandle,
    controller: State<'_, ControllerManager>,
    port_name: String,
) -> Result<(), String> {
    controller.connect(&port_name, app)
}

#[tauri::command]
pub fn controller_disconnect(controller: State<'_, ControllerManager>) -> Result<(), String> {
    controller.disconnect()
}

#[tauri::command]
pub fn controller_get_status(
    controller: State<'_, ControllerManager>,
) -> Result<ControllerStatus, String> {
    Ok(controller.status())
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
