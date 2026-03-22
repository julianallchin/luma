// Remote CRUD operations for track_beats table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackBeats;
use serde::Serialize;

/// Payload for upserting track beats to Supabase
#[derive(Serialize)]
struct TrackBeatsPayload<'a> {
    uid: &'a str,
    track_id: &'a str,
    beats_json: &'a str,
    downbeats_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bpm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downbeat_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    beats_per_bar: Option<i64>,
}

/// Upsert track beats in Supabase (idempotent).
///
/// Track beats use track_id as a unique constraint (one-to-one with tracks).
/// The track_id is taken directly from the beats record (already a UUID).
pub async fn upsert_track_beats(
    client: &SupabaseClient,
    beats: &TrackBeats,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = beats
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackBeatsPayload {
        uid,
        track_id: &beats.track_id,
        beats_json: &beats.beats_json,
        downbeats_json: &beats.downbeats_json,
        bpm: beats.bpm,
        downbeat_offset: beats.downbeat_offset,
        beats_per_bar: beats.beats_per_bar,
    };

    client
        .upsert_no_return("track_beats", &payload, "track_id", access_token)
        .await
}
