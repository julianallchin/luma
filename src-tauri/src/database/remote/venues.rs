// Remote CRUD operations for venues table

use super::common::{SupabaseClient, SyncError};
use crate::models::venues::Venue;
use serde::Serialize;

/// Payload for upserting a venue to Supabase
#[derive(Serialize)]
struct VenuePayload<'a> {
    id: &'a str,
    uid: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    share_code: Option<&'a str>,
}

/// Upsert a venue in Supabase (idempotent).
///
/// The local UUID is sent as the cloud `id`. Uses ON CONFLICT(id) upsert.
pub async fn upsert_venue(
    client: &SupabaseClient,
    venue: &Venue,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = venue
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = VenuePayload {
        id: &venue.id,
        uid,
        name: &venue.name,
        description: venue.description.as_deref(),
        share_code: venue.share_code.as_deref(),
    };

    client
        .upsert_no_return("venues", &payload, "id", access_token)
        .await
}

/// Delete a venue from Supabase
pub async fn delete_venue(
    client: &SupabaseClient,
    id: &str,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("venues", id, access_token).await
}
