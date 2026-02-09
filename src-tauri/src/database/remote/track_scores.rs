// Remote CRUD operations for track_scores table

use super::common::{SupabaseClient, SyncError};
use crate::models::scores::TrackScore;
use serde::Serialize;
use serde_json::Value;

/// Payload for upserting a track score to Supabase
#[derive(Serialize)]
struct TrackScorePayload<'a> {
    uid: &'a str,
    score_id: i64,   // Cloud score ID (from score's remote_id)
    pattern_id: i64, // Cloud pattern ID (from pattern's remote_id)
    start_time: f64,
    end_time: f64,
    z_index: i64,
    blend_mode: &'a str, // Serialized blend mode
    args: &'a Value,
}

/// Insert or update a track score in Supabase
///
/// Returns the cloud ID (either newly generated or existing remote_id).
///
/// # Arguments
/// * `client` - Supabase client
/// * `track_score` - The track score to sync
/// * `score_remote_id` - The cloud ID of the score (from score's remote_id)
/// * `pattern_remote_id` - The cloud ID of the pattern (from pattern's remote_id)
/// * `access_token` - User's access token
pub async fn upsert_track_score(
    client: &SupabaseClient,
    track_score: &TrackScore,
    score_remote_id: i64,
    pattern_remote_id: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = track_score
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    // Serialize blend mode to string
    let blend_mode = serde_json::to_string(&track_score.blend_mode)
        .map_err(|e| SyncError::ParseError(format!("Failed to serialize blend_mode: {}", e)))?
        .trim_matches('"')
        .to_string();

    let payload = TrackScorePayload {
        uid,
        score_id: score_remote_id,
        pattern_id: pattern_remote_id,
        start_time: track_score.start_time,
        end_time: track_score.end_time,
        z_index: track_score.z_index,
        blend_mode: &blend_mode,
        args: &track_score.args,
    };

    match &track_score.remote_id {
        None => client.insert("track_scores", &payload, access_token).await,
        Some(remote_id_str) => {
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;
            client
                .update("track_scores", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}
