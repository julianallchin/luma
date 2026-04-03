//! Tauri commands for auth session storage

use tauri::State;

use crate::database::local::state::StateDb;
use crate::database::Db;

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

/// Wipe all synced data from the local database (used on sign-out).
/// Deletes in reverse tier order to respect foreign key constraints.
#[tauri::command]
pub async fn wipe_database(db: State<'_, Db>) -> Result<(), String> {
    // Reverse tier order: tier 3 → 0, then supporting tables
    let tables = [
        "track_scores",
        "venue_implementation_overrides",
        "fixture_group_members",
        "implementations",
        "scores",
        "track_beats",
        "track_roots",
        "track_stems",
        "midi_bindings",
        "cues",
        "fixtures",
        "patterns",
        "fixture_groups",
        "midi_modifiers",
        "tracks",
        "venues",
        "pending_ops",
        "sync_state",
    ];
    for table in tables {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {}", table)))
            .execute(&db.0)
            .await
            .map_err(|e| format!("Failed to wipe {}: {}", table, e))?;
    }
    println!("[auth] Database wiped on sign-out");
    Ok(())
}
