// Remote CRUD operations for patterns table

use super::common::{SupabaseClient, SyncError};
use crate::models::patterns::PatternSummary;
use serde::Serialize;

/// Payload for upserting a pattern to Supabase
#[derive(Serialize)]
struct PatternPayload<'a> {
    uid: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category_id: Option<i64>, // Cloud category ID (from category's remote_id)
}

/// Insert or update a pattern in Supabase
///
/// If the pattern has no remote_id, performs an INSERT and returns the generated cloud ID.
/// If the pattern has a remote_id, performs an UPDATE using that ID.
///
/// Returns the cloud ID (either newly generated or existing remote_id).
///
/// # Arguments
/// * `client` - Supabase client
/// * `pattern` - The pattern to sync
/// * `category_remote_id` - The cloud ID of the category (from category's remote_id), if any
/// * `access_token` - User's access token
///
/// # FK Resolution
/// If the pattern has a category, the category must be synced first to get its remote_id.
pub async fn upsert_pattern(
    client: &SupabaseClient,
    pattern: &PatternSummary,
    category_remote_id: Option<i64>,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = pattern
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = PatternPayload {
        uid,
        name: &pattern.name,
        description: pattern.description.as_deref(),
        category_id: category_remote_id,
    };

    match &pattern.remote_id {
        None => {
            // INSERT: Cloud generates new ID
            client.insert("patterns", &payload, access_token).await
        }
        Some(remote_id_str) => {
            // UPDATE: Use existing cloud ID
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;

            client
                .update("patterns", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete a pattern from Supabase
///
/// Requires the pattern to have a remote_id (must be synced first).
pub async fn delete_pattern(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("patterns", remote_id, access_token).await
}
