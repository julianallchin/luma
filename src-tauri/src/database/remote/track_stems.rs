// Remote CRUD operations for track_stems table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackStem;
use serde::Serialize;

/// Payload for upserting track stems to Supabase
#[derive(Serialize)]
struct TrackStemPayload<'a> {
    uid: &'a str,
    track_id: i64, // Cloud track ID (from track's remote_id)
    stem_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_path: Option<&'a str>,
}

/// Insert or update a track stem in Supabase
///
/// Note: `file_path` (local path) is NOT synced. Only `storage_path` is synced.
pub async fn upsert_track_stem(
    client: &SupabaseClient,
    stem: &TrackStem,
    track_remote_id: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = stem
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackStemPayload {
        uid,
        track_id: track_remote_id,
        stem_name: &stem.stem_name,
        storage_path: stem.storage_path.as_deref(),
    };

    match &stem.remote_id {
        None => client.insert("track_stems", &payload, access_token).await,
        Some(remote_id_str) => {
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;
            client
                .update("track_stems", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete a track stem from Supabase
pub async fn delete_track_stem(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("track_stems", remote_id, access_token).await
}
