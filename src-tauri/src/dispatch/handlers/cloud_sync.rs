//! Remote Supabase reads that are not part of the sync loop.

use std::collections::HashMap;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::queries::{search_patterns, SearchPatternRow};
use crate::dispatch::{AppServices, CommandError};

pub async fn search_patterns_remote(
    services: &AppServices,
    query: String,
    category_name: Option<String>,
    limit: Option<i32>,
    offset: Option<i32>,
) -> Result<Vec<SearchPatternRow>, CommandError> {
    let token = access_token(services).await?;
    search_patterns(
        &supabase(),
        &query,
        category_name.as_deref(),
        limit.unwrap_or(50),
        offset.unwrap_or(0),
        &token,
    )
    .await
    .map_err(|error| CommandError::Internal(format!("Failed to search patterns: {error}")))
}

/// Sparse by design: a uid whose profile has no display name is absent from the
/// map rather than present-and-empty, so callers read a miss as "unknown".
pub async fn get_display_names(
    services: &AppServices,
    uids: Vec<String>,
) -> Result<HashMap<String, String>, CommandError> {
    // Deliberately before the auth check: an empty request never errors out
    // just because the user is signed out.
    if uids.is_empty() {
        return Ok(HashMap::new());
    }
    let token = access_token(services).await?;

    #[derive(serde::Deserialize)]
    struct ProfileRow {
        id: String,
        display_name: Option<String>,
    }

    let filter = format!("id=in.({})&select=id,display_name", uids.join(","));
    let rows: Vec<ProfileRow> = supabase()
        .select("profiles", &filter, &token)
        .await
        .map_err(|error| CommandError::Internal(format!("Failed to fetch profiles: {error:?}")))?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.display_name.map(|name| (row.id, name)))
        .collect())
}

/// The access token of the verified session, or the message the frontend keys
/// its signed-out state off.
async fn access_token(services: &AppServices) -> Result<String, CommandError> {
    let auth = auth::get_current_auth(&services.state_db.0)
        .await?
        .ok_or_else(|| {
            CommandError::Unauthorized("Not authenticated - please sign in first".into())
        })?;
    Ok(auth.access_token)
}

fn supabase() -> SupabaseClient {
    SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string())
}
