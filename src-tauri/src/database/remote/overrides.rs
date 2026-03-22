// Remote CRUD operations for venue_implementation_overrides table

use super::common::{SupabaseClient, SyncError};
use crate::models::venues::VenueImplementationOverride;
use serde::Serialize;

/// Payload for upserting a venue implementation override to Supabase
#[derive(Serialize)]
struct VenueImplementationOverridePayload<'a> {
    uid: &'a str,
    venue_id: &'a str,
    pattern_id: &'a str,
    implementation_id: &'a str,
}

/// Upsert a venue implementation override in Supabase (idempotent).
///
/// All FK references (venue_id, pattern_id, implementation_id) are taken
/// directly from the override record (already UUIDs).
/// Uses ON CONFLICT(venue_id, pattern_id) upsert.
pub async fn upsert_venue_override(
    client: &SupabaseClient,
    override_data: &VenueImplementationOverride,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = override_data
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = VenueImplementationOverridePayload {
        uid,
        venue_id: &override_data.venue_id,
        pattern_id: &override_data.pattern_id,
        implementation_id: &override_data.implementation_id,
    };

    client
        .upsert_no_return(
            "venue_implementation_overrides",
            &payload,
            "venue_id,pattern_id",
            access_token,
        )
        .await
}
