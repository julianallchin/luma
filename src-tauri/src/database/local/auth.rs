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

pub async fn get_current_user_id(pool: &SqlitePool) -> Result<Option<String>, String> {
    let session_json = get_session_item(pool, SUPABASE_SESSION_KEY).await?;

    let Some(session_json) = session_json else {
        return Ok(None);
    };

    let session_value: serde_json::Value = serde_json::from_str(&session_json)
        .map_err(|err| format!("Failed to parse session json: {err}"))?;

    if let Some(user) = session_value.get("user") {
        if let Some(id) = user.get("id").and_then(|v| v.as_str()) {
            return Ok(Some(id.to_string()));
        }
    }

    Ok(None)
}

pub async fn get_current_access_token(pool: &SqlitePool) -> Result<Option<String>, String> {
    let session_json = get_session_item(pool, SUPABASE_SESSION_KEY).await?;

    let Some(session_json) = session_json else {
        return Ok(None);
    };

    let session_value: serde_json::Value = serde_json::from_str(&session_json)
        .map_err(|err| format!("Failed to parse session json: {err}"))?;

    // Check if token is expired or expiring within 60 seconds
    if let Some(expires_at) = session_value.get("expires_at").and_then(|v| v.as_i64()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        if expires_at - now < 60 {
            // Token expired or about to expire — try to refresh
            if let Some(refresh_token) = session_value.get("refresh_token").and_then(|v| v.as_str())
            {
                match refresh_session(pool, refresh_token).await {
                    Ok(new_token) => return Ok(Some(new_token)),
                    Err(e) => {
                        eprintln!("[auth] Token refresh failed: {}", e);
                        // Fall through to return stale token — caller will get a 401
                    }
                }
            }
        }
    }

    if let Some(token) = session_value.get("access_token").and_then(|v| v.as_str()) {
        return Ok(Some(token.to_string()));
    }

    Ok(None)
}

/// Refresh the Supabase session using a refresh_token.
/// Stores the new session and returns the new access_token.
async fn refresh_session(pool: &SqlitePool, refresh_token: &str) -> Result<String, String> {
    use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};

    let url = format!("{}/auth/v1/token?grant_type=refresh_token", SUPABASE_URL);
    let client = reqwest::Client::new();
    let res = client
        .post(&url)
        .header("apikey", SUPABASE_ANON_KEY)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {}", e))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("Refresh failed ({}): {}", status, body));
    }

    let new_session: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("Failed to parse refresh response: {}", e))?;

    let new_token = new_session
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Refresh response missing access_token".to_string())?
        .to_string();

    // Persist the refreshed session
    let session_str = serde_json::to_string(&new_session)
        .map_err(|e| format!("Failed to serialize refreshed session: {}", e))?;
    set_session_item(pool, SUPABASE_SESSION_KEY, &session_str).await?;

    eprintln!("[auth] Token refreshed successfully");
    Ok(new_token)
}
