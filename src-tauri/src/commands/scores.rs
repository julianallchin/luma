//! Tauri commands for score (track annotation) operations

use tauri::State;

use crate::database::local::scores as db;
use crate::database::Db;
use crate::models::scores::{CreateScoreInput, TrackScore, UpdateScoreInput};

#[tauri::command]
pub async fn list_scores(
    db: State<'_, Db>,
    track_id: i64,
) -> Result<Vec<TrackScore>, String> {
    db::get_scores_for_track(&db.0, track_id).await
}

#[tauri::command]
pub async fn create_score(
    db: State<'_, Db>,
    payload: CreateScoreInput,
) -> Result<TrackScore, String> {
    db::create_score(&db.0, payload).await
}

#[tauri::command]
pub async fn update_score(
    db: State<'_, Db>,
    payload: UpdateScoreInput,
) -> Result<(), String> {
    db::update_score(&db.0, payload).await
}

#[tauri::command]
pub async fn delete_score(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db::delete_score(&db.0, id).await
}
