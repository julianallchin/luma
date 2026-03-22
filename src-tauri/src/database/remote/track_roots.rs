// Remote CRUD operations for track_roots table

use super::common::{SupabaseClient, SyncError};
use crate::models::tracks::TrackRoots;
use serde::Serialize;

/// Payload for upserting track roots to Supabase
#[derive(Serialize)]
struct TrackRootsPayload<'a> {
    uid: &'a str,
    track_id: &'a str,
    sections_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    logits_storage_path: Option<&'a str>,
}

/// Upsert track roots in Supabase (idempotent).
///
/// The track_id is taken directly from the roots record (already a UUID).
/// Note: `logits_path` (local path) is NOT synced. Only `logits_storage_path` is synced.
pub async fn upsert_track_roots(
    client: &SupabaseClient,
    roots: &TrackRoots,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = roots
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackRootsPayload {
        uid,
        track_id: &roots.track_id,
        sections_json: &roots.sections_json,
        logits_storage_path: roots.logits_storage_path.as_deref(),
    };

    client
        .upsert_no_return("track_roots", &payload, "track_id", access_token)
        .await
}
