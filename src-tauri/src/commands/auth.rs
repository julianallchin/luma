//! Tauri commands for auth session storage

use tauri::State;

use crate::database::local::state::StateDb;

#[tauri::command]
pub async fn get_session_item(
    key: String,
    state: State<'_, StateDb>,
) -> Result<Option<String>, String> {
    crate::database::local::auth::get_session_item(&state.0, &key).await
}

#[tauri::command]
pub async fn set_session_item(
    key: String,
    value: String,
    state: State<'_, StateDb>,
) -> Result<(), String> {
    crate::database::local::auth::set_session_item(&state.0, &key, &value).await
}

#[tauri::command]
pub async fn remove_session_item(key: String, state: State<'_, StateDb>) -> Result<(), String> {
    crate::database::local::auth::remove_session_item(&state.0, &key).await
}

#[tauri::command]
pub async fn log_session_from_state_db(state: State<'_, StateDb>) -> Result<(), String> {
    crate::database::local::auth::log_supabase_session(&state.0).await
}
