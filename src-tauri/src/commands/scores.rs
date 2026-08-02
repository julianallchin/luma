//! Tauri commands for score (track annotation) operations

use tauri::State;

use crate::database::local::scores as db;
use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::models::scores::{
    CreateTrackScoreInput, Score, ScoreSummary, TrackScore, UpdateTrackScoreInput,
};

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
pub async fn delete_score(db: State<'_, Db>, id: String) -> Result<(), String> {
    db::delete_score(&db.0, &id).await?;

    // Enqueue soft-delete for the sync push loop
    if let Err(e) = crate::sync::pending::enqueue_delete(&db.0, "scores", &id, "id").await {
        eprintln!("[delete_score] Failed to enqueue delete: {e}");
    }

    Ok(())
}

#[tauri::command]
pub async fn replace_track_scores(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    score_id: String,
    track_id: String,
    base_scores: Vec<TrackScore>,
    scores: Vec<TrackScore>,
) -> Result<crate::services::track_edits::TrackEditResult, String> {
    let user_id = crate::database::local::auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "sign in before replacing an authored track".to_string())?;
    let score = db::get_score(&db.0, &score_id).await?;
    let scope = crate::services::track_edits::TrackEditScope {
        score_id,
        track_id,
        venue_id: score.venue_id,
        user_id,
    };
    crate::services::track_edits::replace_track_scores_from_snapshot(
        &db.0,
        &scope,
        base_scores,
        scores,
    )
    .await
    .map_err(|error| error.to_string())
}
