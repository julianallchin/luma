// Remote CRUD operations for track_beats table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackBeats;
use serde::Serialize;

/// Payload for upserting track beats to Supabase
#[derive(Serialize)]
struct TrackBeatsPayload<'a> {
    uid: &'a str,
    track_id: i64, // Cloud track ID (from track's remote_id)
    beats_json: &'a str,
    downbeats_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bpm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downbeat_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    beats_per_bar: Option<i64>,
}

/// Insert or update track beats in Supabase
///
/// Track beats use track_id as the primary key (one-to-one with tracks).
/// Upserts based on track_id.
///
/// # Arguments
/// * `client` - Supabase client
/// * `beats` - The track beats to sync
/// * `track_remote_id` - The cloud ID of the track (from track's remote_id)
/// * `access_token` - User's access token
pub async fn upsert_track_beats(
    client: &SupabaseClient,
    beats: &TrackBeats,
    track_remote_id: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = beats
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackBeatsPayload {
        uid,
        track_id: track_remote_id,
        beats_json: &beats.beats_json,
        downbeats_json: &beats.downbeats_json,
        bpm: beats.bpm,
        downbeat_offset: beats.downbeat_offset,
        beats_per_bar: beats.beats_per_bar,
    };

    match &beats.remote_id {
        None => client.insert("track_beats", &payload, access_token).await,
        Some(remote_id_str) => {
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;
            client
                .update("track_beats", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}
