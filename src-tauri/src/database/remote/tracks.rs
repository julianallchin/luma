// Remote CRUD operations for tracks table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackSummary;
use serde::Serialize;

/// Payload for upserting a track to Supabase
#[derive(Serialize)]
struct TrackPayload<'a> {
    uid: &'a str,
    track_hash: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artist: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    album: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    track_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disc_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    album_art_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    album_art_mime: Option<&'a str>,
}

/// Insert or update a track in Supabase
///
/// If the track has no remote_id, performs an INSERT and returns the generated cloud ID.
/// If the track has a remote_id, performs an UPDATE using that ID.
///
/// Returns the cloud ID (either newly generated or existing remote_id).
///
/// Note: `file_path` and `album_art_data` are NOT synced to cloud (local only).
pub async fn upsert_track(
    client: &SupabaseClient,
    track: &TrackSummary,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = track
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackPayload {
        uid,
        track_hash: &track.track_hash,
        title: track.title.as_deref(),
        artist: track.artist.as_deref(),
        album: track.album.as_deref(),
        track_number: track.track_number,
        disc_number: track.disc_number,
        duration_seconds: track.duration_seconds,
        storage_path: track.storage_path.as_deref(),
        album_art_path: track.album_art_path.as_deref(),
        album_art_mime: track.album_art_mime.as_deref(),
    };

    match &track.remote_id {
        None => {
            // INSERT: Cloud generates new ID
            client.insert("tracks", &payload, access_token).await
        }
        Some(remote_id_str) => {
            // UPDATE: Use existing cloud ID
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;

            client
                .update("tracks", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete a track from Supabase
///
/// Requires the track to have a remote_id (must be synced first).
pub async fn delete_track(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("tracks", remote_id, access_token).await
}
