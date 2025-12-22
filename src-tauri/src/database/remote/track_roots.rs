// Remote CRUD operations for track_roots table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackRoots;
use serde::Serialize;

/// Payload for upserting track roots to Supabase
#[derive(Serialize)]
struct TrackRootsPayload<'a> {
    uid: &'a str,
    track_id: i64, // Cloud track ID (from track's remote_id)
    sections_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    logits_storage_path: Option<&'a str>,
}

/// Insert or update track roots in Supabase
///
/// Note: `logits_path` (local path) is NOT synced. Only `logits_storage_path` is synced.
pub async fn upsert_track_roots(
    client: &SupabaseClient,
    roots: &TrackRoots,
    track_remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = roots
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackRootsPayload {
        uid,
        track_id: track_remote_id,
        sections_json: &roots.sections_json,
        logits_storage_path: roots.logits_storage_path.as_deref(),
    };

    match &roots.remote_id {
        None => {
            client.insert("track_roots", &payload, access_token).await?;
            Ok(())
        }
        Some(remote_id_str) => {
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;
            client.update("track_roots", remote_id, &payload, access_token).await
        }
    }
}

/// Delete track roots from Supabase
pub async fn delete_track_roots(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("track_roots", remote_id, access_token).await
}
