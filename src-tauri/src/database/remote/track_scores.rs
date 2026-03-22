// Remote CRUD operations for track_scores table
//
// Uses batch upsert for idempotent sync. Every row includes its local UUID as
// the cloud `id`, so all rows are upserted on `id`. No cloud rows are deleted
// -- local is source of truth, and orphaned cloud rows are harmless.

use super::common::{SupabaseClient, SyncError};
use crate::models::scores::TrackScore;
use serde::Serialize;

/// Payload for upserting a track_score row to Supabase.
/// The `id` is always the local UUID (= cloud UUID).
#[derive(Serialize)]
struct TrackScorePayload<'a> {
    id: &'a str,
    uid: &'a str,
    score_id: &'a str,
    pattern_id: &'a str,
    start_time: f64,
    end_time: f64,
    z_index: i64,
    blend_mode: String,
    args_json: String,
}

/// Sync all track_scores for a given score to the cloud (idempotent).
///
/// Every row is upserted using its local UUID as the cloud `id`.
/// The pattern_id in each TrackScore is already the UUID used in cloud.
pub async fn sync_track_scores_for_score(
    client: &SupabaseClient,
    track_scores: &[TrackScore],
    access_token: &str,
) -> Result<(), SyncError> {
    if track_scores.is_empty() {
        return Ok(());
    }

    let payloads: Vec<TrackScorePayload> = track_scores
        .iter()
        .filter_map(|ts| {
            let uid = ts.uid.as_deref()?;
            let blend_mode_str = match serde_json::to_string(&ts.blend_mode) {
                Ok(s) => s.trim_matches('"').to_string(),
                Err(_) => "replace".to_string(),
            };

            Some(TrackScorePayload {
                id: &ts.id,
                uid,
                score_id: &ts.score_id,
                pattern_id: &ts.pattern_id,
                start_time: ts.start_time,
                end_time: ts.end_time,
                z_index: ts.z_index,
                blend_mode: blend_mode_str,
                args_json: ts.args.to_string(),
            })
        })
        .collect();

    client
        .upsert_batch_no_return("track_scores", &payloads, "id", access_token)
        .await
}
