//! Tauri commands for score (track annotation) operations

use tauri::State;

use crate::database::local::scores as db;
use crate::database::Db;
use crate::models::scores::{CreateTrackScoreInput, TrackScore, UpdateTrackScoreInput};

#[tauri::command]
pub async fn list_track_scores(
    db: State<'_, Db>,
    track_id: i64,
) -> Result<Vec<TrackScore>, String> {
    db::get_scores_for_track(&db.0, track_id).await
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
pub async fn delete_track_score(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db::delete_track_score(&db.0, id).await
}

#[tauri::command]
pub async fn replace_track_scores(
    db: State<'_, Db>,
    track_id: i64,
    scores: Vec<TrackScore>,
) -> Result<(), String> {
    db::replace_track_scores(&db.0, track_id, scores).await
}
