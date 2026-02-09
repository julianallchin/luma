// Remote CRUD operations for venues table

use super::common::{SupabaseClient, SyncError};
use crate::models::venues::Venue;
use serde::Serialize;

/// Payload for upserting a venue to Supabase
#[derive(Serialize)]
struct VenuePayload<'a> {
    uid: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

/// Insert or update a venue in Supabase
///
/// If the venue has no remote_id, performs an INSERT and returns the generated cloud ID.
/// If the venue has a remote_id, performs an UPDATE using that ID.
///
/// Returns the cloud ID (either newly generated or existing remote_id).
pub async fn upsert_venue(
    client: &SupabaseClient,
    venue: &Venue,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = venue
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = VenuePayload {
        uid,
        name: &venue.name,
        description: venue.description.as_deref(),
    };

    match &venue.remote_id {
        None => {
            // INSERT: Cloud generates new ID
            client.insert("venues", &payload, access_token).await
        }
        Some(remote_id_str) => {
            // UPDATE: Use existing cloud ID
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;

            client
                .update("venues", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete a venue from Supabase
///
/// Requires the venue to have a remote_id (must be synced first).
pub async fn delete_venue(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("venues", remote_id, access_token).await
}
