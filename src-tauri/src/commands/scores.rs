//! Tauri commands for score (track annotation) operations

use tauri::State;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::scores as db;
use crate::database::local::state::StateDb;
use crate::database::remote::common::SupabaseClient;
use crate::database::Db;
use crate::models::scores::{
    CreateTrackScoreInput, Score, ScoreSummary, TrackScore, UpdateTrackScoreInput,
};

fn utc_now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[tauri::command]
pub async fn list_scores_for_track(
    db: State<'_, Db>,
    track_id: String,
    venue_id: String,
) -> Result<Vec<ScoreSummary>, String> {
    db::list_scores_for_track(&db.0, &track_id, &venue_id).await
}

#[tauri::command]
pub async fn create_score(
    db: State<'_, Db>,
    track_id: String,
    venue_id: String,
    uid: String,
    name: Option<String>,
) -> Result<Score, String> {
    db::create_score(&db.0, &track_id, &venue_id, &uid, name.as_deref()).await
}

#[tauri::command]
pub async fn list_track_scores(
    db: State<'_, Db>,
    score_id: String,
) -> Result<Vec<TrackScore>, String> {
    db::list_track_scores_for_score(&db.0, &score_id).await
}

#[tauri::command]
pub async fn create_track_score(
    db: State<'_, Db>,
    payload: CreateTrackScoreInput,
) -> Result<TrackScore, String> {
    db::create_track_score(&db.0, payload).await
}

#[tauri::command]
pub async fn update_track_score(
    db: State<'_, Db>,
    payload: UpdateTrackScoreInput,
) -> Result<(), String> {
    db::update_track_score(&db.0, payload).await
}

#[tauri::command]
pub async fn delete_track_score(db: State<'_, Db>, id: String) -> Result<(), String> {
    db::delete_track_score(&db.0, &id).await
}

#[tauri::command]
pub async fn delete_score(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    id: String,
) -> Result<(), String> {
    db::delete_score(&db.0, &id).await?;

    // Best-effort remote soft-delete; next full sync covers failures
    if let Ok(Some(token)) = auth::get_current_access_token(&state_db.0).await {
        let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
        if let Err(e) = client
            .update(
                "scores",
                &id,
                &serde_json::json!({ "deleted_at": utc_now_rfc3339() }),
                &token,
            )
            .await
        {
            eprintln!(
                "[auto-sync] Failed to soft-delete score {} from cloud: {}",
                id, e
            );
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn replace_track_scores(
    db: State<'_, Db>,
    score_id: String,
    track_id: String,
    scores: Vec<TrackScore>,
) -> Result<(), String> {
    db::replace_track_scores(&db.0, &score_id, &track_id, scores).await
}
