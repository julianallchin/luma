use sqlx::SqlitePool;

const SUPABASE_SESSION_KEY: &str = "supabase_session";

pub async fn get_session_item(pool: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM auth_session WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|err| format!("Failed to read session: {err}"))
}

pub async fn set_session_item(pool: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO auth_session (key, value) VALUES (?, ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .map_err(|err| format!("Failed to store session: {err}"))?;
    Ok(())
}

pub async fn remove_session_item(pool: &SqlitePool, key: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM auth_session WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await
        .map_err(|err| format!("Failed to remove session: {err}"))?;
    Ok(())
}

pub async fn log_supabase_session(pool: &SqlitePool) -> Result<(), String> {
    let session_json = get_session_item(pool, SUPABASE_SESSION_KEY).await?;

    let Some(session_json) = session_json else {
        println!(
            "[auth] No session found in state db for {}",
            SUPABASE_SESSION_KEY
        );
        return Ok(());
    };

    let session_value: serde_json::Value = serde_json::from_str(&session_json)
        .map_err(|err| format!("Failed to parse session json: {err}"))?;

    if let Some(user_value) = session_value.get("user") {
        println!("[auth] State db user: {}", user_value);
    } else {
        println!("[auth] State db session missing user");
    }

    Ok(())
}
