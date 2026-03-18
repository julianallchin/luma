// Remote CRUD operations for track_scores table
//
// Unlike other remote modules that upsert individual records, track_scores uses
// a delete-all-then-batch-insert strategy per score_id. This avoids diffing
// individual rows and keeps the cloud in exact sync with local state.

use super::common::{SupabaseClient, SyncError};
use crate::models::scores::TrackScore;
use serde::Serialize;
use std::collections::HashMap;

/// Payload for inserting a track_score row to Supabase
#[derive(Serialize)]
struct TrackScorePayload {
    uid: String,
    score_id: i64,
    pattern_id: i64,
    start_time: f64,
    end_time: f64,
    z_index: i64,
    blend_mode: String,
    args_json: String,
}

/// Sync all track_scores for a given score to the cloud.
///
/// Strategy: delete all existing cloud rows for this score_id, then batch-insert
/// the current local rows. Returns the cloud IDs of the inserted rows (in the
/// same order as `track_scores`).
///
/// # Arguments
/// * `client` - Supabase client
/// * `uid` - User's auth UID
/// * `score_remote_id` - Cloud ID of the parent score
/// * `track_scores` - Local track_score rows to sync
/// * `pattern_id_map` - Maps local pattern_id -> cloud pattern remote_id
/// * `access_token` - User's access token
pub async fn sync_track_scores_for_score(
    client: &SupabaseClient,
    uid: &str,
    score_remote_id: i64,
    track_scores: &[TrackScore],
    pattern_id_map: &HashMap<i64, i64>,
    access_token: &str,
) -> Result<Vec<i64>, SyncError> {
    // 1. Delete all existing cloud track_scores for this score
    let filter = format!("score_id=eq.{}", score_remote_id);
    client
        .delete_by_filter("track_scores", &filter, access_token)
        .await?;

    if track_scores.is_empty() {
        return Ok(vec![]);
    }

    // 2. Build payloads with mapped pattern IDs
    let mut payloads = Vec::with_capacity(track_scores.len());
    for ts in track_scores {
        let cloud_pattern_id = pattern_id_map.get(&ts.pattern_id).ok_or_else(|| {
            SyncError::MissingField(format!(
                "No cloud remote_id for local pattern_id {}",
                ts.pattern_id
            ))
        })?;

        let blend_mode_str = match serde_json::to_string(&ts.blend_mode) {
            Ok(s) => s.trim_matches('"').to_string(),
            Err(_) => "replace".to_string(),
        };

        payloads.push(TrackScorePayload {
            uid: uid.to_string(),
            score_id: score_remote_id,
            pattern_id: *cloud_pattern_id,
            start_time: ts.start_time,
            end_time: ts.end_time,
            z_index: ts.z_index,
            blend_mode: blend_mode_str,
            args_json: ts.args.to_string(),
        });
    }

    // 3. Batch insert
    client
        .insert_batch("track_scores", &payloads, access_token)
        .await
}
