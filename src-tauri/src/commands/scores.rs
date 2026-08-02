//! Tauri commands for score (track annotation) operations

use tauri::State;

use crate::database::local::auth;
use crate::database::local::scores as db;
use crate::database::local::state::StateDb;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::database::Db;
use crate::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, ScoreSummary, TrackScore,
    UpdateTrackScoreInput,
};
use crate::services::authored_documents::AuthoredDocuments;
use crate::services::score_mutations;
use crate::services::track_edits::TrackEditResult;
use crate::sync::orchestrator::SyncEngine;

#[tauri::command]
pub async fn list_scores_for_track(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    track_id: String,
    venue_id: String,
) -> Result<Vec<ScoreSummary>, String> {
    if venue_id.is_empty() {
        let visible = db::list_accessible_scores_for_track(&db.0, &track_id).await?;
        for score in visible {
            authored
                .reconcile_track_score_for_read(&db.0, &score.id)
                .await
                .map_err(|error| error.to_string())?;
        }
        return db::list_accessible_scores_for_track(&db.0, &track_id).await;
    }
    VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    authored
        .reconcile_track_scores_for_read(&db.0, &track_id, &venue_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    db::list_scores_for_track(&mut access, &track_id).await
}

#[tauri::command]
pub async fn create_score(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    request_id: String,
    track_id: String,
    venue_id: String,
    name: Option<String>,
) -> Result<Score, String> {
    authored
        .create_score(&db.0, &request_id, &track_id, &venue_id, name.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn list_track_scores(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    score_id: String,
) -> Result<Vec<TrackScore>, String> {
    VenueAccess::<Read>::read(&db.0, VenueResource::Score(&score_id)).await?;
    authored
        .reconcile_track_score_for_read(&db.0, &score_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Score(&score_id)).await?;
    db::list_track_scores_for_score(&mut access, &score_id).await
}

#[tauri::command]
pub async fn create_track_score(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    payload: CreateTrackScoreInput,
) -> Result<TrackEditResult, String> {
    score_mutations::create_track_score(&authored, &db.0, payload)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn update_track_score(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    payload: UpdateTrackScoreInput,
) -> Result<TrackEditResult, String> {
    score_mutations::update_track_score(&authored, &db.0, payload)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_track_score(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    payload: DeleteTrackScoreInput,
) -> Result<TrackEditResult, String> {
    score_mutations::delete_track_score(&authored, &db.0, payload)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_score(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    authored: State<'_, AuthoredDocuments>,
    engine: State<'_, SyncEngine>,
    id: String,
) -> Result<(), String> {
    let principal = auth::get_current_user_id(&state_db.0).await?;
    authored
        .archive_score(&db.0, principal.as_deref(), &id)
        .await
        .map_err(|error| error.to_string())?;
    engine.push_notify.notify_one();
    Ok(())
}

#[tauri::command]
pub async fn replace_track_scores(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    score_id: String,
    track_id: String,
    base_scores: Vec<TrackScore>,
    scores: Vec<TrackScore>,
    operation_id: String,
) -> Result<crate::services::track_edits::TrackEditResult, String> {
    score_mutations::replace_track_scores(
        &authored,
        &db.0,
        &score_id,
        &track_id,
        &base_scores,
        &scores,
        &operation_id,
    )
    .await
    .map_err(|error| error.to_string())
}
