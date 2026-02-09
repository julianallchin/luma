// Remote CRUD operations for pattern_categories table

use super::common::{SupabaseClient, SyncError};
use crate::models::patterns::PatternCategory;
use serde::Serialize;

/// Payload for upserting a pattern category to Supabase
#[derive(Serialize)]
struct PatternCategoryPayload<'a> {
    uid: &'a str,
    name: &'a str,
}

/// Insert or update a pattern category in Supabase
///
/// If the category has no remote_id, performs an INSERT and returns the generated cloud ID.
/// If the category has a remote_id, performs an UPDATE using that ID.
///
/// Returns the cloud ID (either newly generated or existing remote_id).
pub async fn upsert_category(
    client: &SupabaseClient,
    category: &PatternCategory,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = category
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = PatternCategoryPayload {
        uid,
        name: &category.name,
    };

    match &category.remote_id {
        None => {
            // INSERT: Cloud generates new ID
            client
                .insert("pattern_categories", &payload, access_token)
                .await
        }
        Some(remote_id_str) => {
            // UPDATE: Use existing cloud ID
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;

            client
                .update("pattern_categories", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete a pattern category from Supabase
///
/// Requires the category to have a remote_id (must be synced first).
pub async fn delete_category(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client
        .delete("pattern_categories", remote_id, access_token)
        .await
}
