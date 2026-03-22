// Remote CRUD operations for scores table

use super::common::{SupabaseClient, SyncError};
use crate::models::scores::Score;
use serde::Serialize;

/// Payload for upserting a score to Supabase
#[derive(Serialize)]
struct ScorePayload<'a> {
    id: &'a str,
    uid: &'a str,
    track_id: &'a str,
    venue_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

/// Upsert a score in Supabase (idempotent).
///
/// The local UUID is sent as the cloud `id`. Uses ON CONFLICT(uid,track_id,venue_id)
/// so that repeated syncs produce the same result.
/// All FK references (track_id, venue_id) are taken directly from the score (already UUIDs).
pub async fn upsert_score(
    client: &SupabaseClient,
    score: &Score,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = score
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = ScorePayload {
        id: &score.id,
        uid,
        track_id: &score.track_id,
        venue_id: &score.venue_id,
        name: score.name.as_deref(),
    };

    client
        .upsert_no_return("scores", &payload, "uid,track_id,venue_id", access_token)
        .await
}
