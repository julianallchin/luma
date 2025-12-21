use tauri::State;

use crate::database::StateDb;

const SUPABASE_SESSION_KEY: &str = "supabase_session";

#[tauri::command]
pub async fn get_session_item(
    key: String,
    state: State<'_, StateDb>,
) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM auth_session WHERE key = ?")
        .bind(&key)
        .fetch_optional(&state.0)
        .await
        .map_err(|err| format!("Failed to read session: {err}"))
}

#[tauri::command]
pub async fn set_session_item(
    key: String,
    value: String,
    state: State<'_, StateDb>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO auth_session (key, value) VALUES (?, ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(&key)
    .bind(&value)
    .execute(&state.0)
    .await
    .map_err(|err| format!("Failed to store session: {err}"))?;
    Ok(())
}

#[tauri::command]
pub async fn remove_session_item(
    key: String,
    state: State<'_, StateDb>,
) -> Result<(), String> {
    sqlx::query("DELETE FROM auth_session WHERE key = ?")
        .bind(&key)
        .execute(&state.0)
        .await
        .map_err(|err| format!("Failed to remove session: {err}"))?;
    Ok(())
}

#[tauri::command]
pub async fn log_session_from_state_db(
    state: State<'_, StateDb>,
) -> Result<(), String> {
    let session_json = sqlx::query_scalar::<_, String>(
        "SELECT value FROM auth_session WHERE key = ?",
    )
    .bind(SUPABASE_SESSION_KEY)
    .fetch_optional(&state.0)
    .await
    .map_err(|err| format!("Failed to read session: {err}"))?;

    let Some(session_json) = session_json else {
        println!(
            "[auth] No session found in state db for {}",
            SUPABASE_SESSION_KEY
        );
        return Ok(());
    };

    let session_value: serde_json::Value =
        serde_json::from_str(&session_json)
            .map_err(|err| format!("Failed to parse session json: {err}"))?;

    if let Some(user_value) = session_value.get("user") {
        println!("[auth] State db user: {}", user_value);
    } else {
        println!("[auth] State db session missing user");
    }

    Ok(())
}
