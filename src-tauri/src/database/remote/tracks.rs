// Remote CRUD operations for tracks table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackSummary;
use serde::Serialize;

/// Payload for upserting a track to Supabase
#[derive(Serialize)]
struct TrackPayload<'a> {
    id: &'a str,
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

/// Upsert a track in Supabase (idempotent).
///
/// The local UUID is sent as the cloud `id`. Uses ON CONFLICT(id) upsert.
///
/// Note: `file_path` and `album_art_data` are NOT synced to cloud (local only).
pub async fn upsert_track(
    client: &SupabaseClient,
    track: &TrackSummary,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = track
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackPayload {
        id: &track.id,
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

    client
        .upsert_no_return("tracks", &payload, "id", access_token)
        .await
}

/// Delete a track from Supabase
pub async fn delete_track(
    client: &SupabaseClient,
    id: &str,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("tracks", id, access_token).await
}
