// Remote CRUD operations for scores table

use super::common::{SupabaseClient, SyncError};
use crate::models::scores::Score;
use serde::Serialize;

/// Payload for upserting a score to Supabase
#[derive(Serialize)]
struct ScorePayload<'a> {
    uid: &'a str,
    track_id: i64, // Cloud track ID (from track's remote_id)
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

/// Insert or update a score in Supabase
///
/// If the score has no remote_id, performs an INSERT and returns the generated cloud ID.
/// If the score has a remote_id, performs an UPDATE using that ID.
///
/// Returns the cloud ID (either newly generated or existing remote_id).
///
/// # Arguments
/// * `client` - Supabase client
/// * `score` - The score to sync
/// * `track_remote_id` - The cloud ID of the track (from track's remote_id)
/// * `access_token` - User's access token
///
/// # FK Resolution
/// The track must be synced first to get its remote_id.
pub async fn upsert_score(
    client: &SupabaseClient,
    score: &Score,
    track_remote_id: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = score
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = ScorePayload {
        uid,
        track_id: track_remote_id,
        name: score.name.as_deref(),
    };

    match &score.remote_id {
        None => {
            // INSERT: Cloud generates new ID
            client.insert("scores", &payload, access_token).await
        }
        Some(remote_id_str) => {
            // UPDATE: Use existing cloud ID
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;

            client
                .update("scores", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}
