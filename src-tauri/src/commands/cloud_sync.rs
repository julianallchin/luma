//! Tauri commands for remote Supabase queries (non-sync).

use tauri::State;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::remote::common::SupabaseClient;

/// Helper to get access token and user ID, or return error
async fn require_auth(state_db: &StateDb) -> Result<(String, String), String> {
    let token = auth::get_current_access_token(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated - please sign in first".to_string())?;
    let uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated - please sign in first".to_string())?;
    Ok((token, uid))
}

/// Search for patterns used in scores (remote search via RPC)
#[tauri::command]
pub async fn search_patterns_remote(
    state_db: State<'_, StateDb>,
    query: String,
    category_name: Option<String>,
    limit: Option<i32>,
    offset: Option<i32>,
) -> Result<Vec<crate::database::remote::queries::SearchPatternRow>, String> {
    let (token, _uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    crate::database::remote::queries::search_patterns(
        &client,
        &query,
        category_name.as_deref(),
        limit.unwrap_or(50),
        offset.unwrap_or(0),
        &token,
    )
    .await
    .map_err(|e| format!("Failed to search patterns: {}", e))
}

/// Look up display names for a list of user IDs from the profiles table.
/// Returns a map of uid -> display_name.
#[tauri::command]
pub async fn get_display_names(
    state_db: State<'_, StateDb>,
    uids: Vec<String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    if uids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let (token, _) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    // Build PostgREST filter: id=in.(uid1,uid2,...)
    let ids_csv = uids.join(",");
    let query = format!("id=in.({})", ids_csv);

    #[derive(serde::Deserialize)]
    struct ProfileRow {
        id: String,
        display_name: Option<String>,
    }

    let rows: Vec<ProfileRow> = client
        .select(
            "profiles",
            &format!("{}&select=id,display_name", query),
            &token,
        )
        .await
        .map_err(|e| format!("Failed to fetch profiles: {:?}", e))?;

    let mut map = std::collections::HashMap::new();
    for row in rows {
        if let Some(name) = row.display_name {
            map.insert(row.id, name);
        }
    }
    Ok(map)
}
