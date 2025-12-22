// Remote CRUD operations for venue_implementation_overrides table

use super::common::{SupabaseClient, SyncError};
use crate::models::venues::VenueImplementationOverride;
use serde::Serialize;

/// Payload for upserting a venue implementation override to Supabase
#[derive(Serialize)]
struct VenueImplementationOverridePayload {
    uid: String,
    venue_id: i64,          // Cloud venue ID (from venue's remote_id)
    pattern_id: i64,        // Cloud pattern ID (from pattern's remote_id)
    implementation_id: i64, // Cloud implementation ID (from implementation's remote_id)
}

/// Insert or update a venue implementation override in Supabase
///
/// Returns the cloud ID (either newly generated or existing remote_id).
///
/// # Arguments
/// * `client` - Supabase client
/// * `override_data` - The override to sync
/// * `venue_remote_id` - The cloud ID of the venue
/// * `pattern_remote_id` - The cloud ID of the pattern
/// * `implementation_remote_id` - The cloud ID of the implementation
/// * `access_token` - User's access token
pub async fn upsert_venue_override(
    client: &SupabaseClient,
    override_data: &VenueImplementationOverride,
    venue_remote_id: i64,
    pattern_remote_id: i64,
    implementation_remote_id: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = override_data
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?
        .to_string();

    let payload = VenueImplementationOverridePayload {
        uid,
        venue_id: venue_remote_id,
        pattern_id: pattern_remote_id,
        implementation_id: implementation_remote_id,
    };

    match &override_data.remote_id {
        None => {
            client
                .insert("venue_implementation_overrides", &payload, access_token)
                .await
        }
        Some(remote_id_str) => {
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;
            client
                .update(
                    "venue_implementation_overrides",
                    remote_id,
                    &payload,
                    access_token,
                )
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete a venue implementation override from Supabase
pub async fn delete_venue_override(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client
        .delete("venue_implementation_overrides", remote_id, access_token)
        .await
}
