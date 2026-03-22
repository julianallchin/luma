// Remote CRUD operations for track_stems table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackStem;
use serde::Serialize;

/// Payload for upserting track stems to Supabase
#[derive(Serialize)]
struct TrackStemPayload<'a> {
    uid: &'a str,
    track_id: &'a str,
    stem_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_path: Option<&'a str>,
}

/// Upsert a track stem in Supabase (idempotent).
///
/// The track_id is taken directly from the stem record (already a UUID).
/// Note: `file_path` (local path) is NOT synced. Only `storage_path` is synced.
pub async fn upsert_track_stem(
    client: &SupabaseClient,
    stem: &TrackStem,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = stem
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackStemPayload {
        uid,
        track_id: &stem.track_id,
        stem_name: &stem.stem_name,
        storage_path: stem.storage_path.as_deref(),
    };

    client
        .upsert_no_return("track_stems", &payload, "track_id,stem_name", access_token)
        .await
}
