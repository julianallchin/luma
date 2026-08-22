use crate::database::local::scores as db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, ScoreSummary, TrackScore,
    UpdateTrackScoreInput,
};
use crate::services::score_mutations;
use crate::services::track_edits::TrackEditResult;

/// An empty `venue_id` means "every venue the caller can see" — the
/// pattern editor depends on that overload.
pub async fn list_scores_for_track(
    services: &AppServices,
    track_id: String,
    venue_id: String,
) -> Result<Vec<ScoreSummary>, CommandError> {
    let pool = &services.db.0;
    if venue_id.is_empty() {
        return Ok(db::list_accessible_scores_for_track(pool, &track_id).await?);
    }
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
    Ok(db::list_scores_for_track(&mut access, &track_id).await?)
}

/// Idempotent on `request_id`: the score id is derived from it, so a replay
/// returns the existing score instead of creating a second one.
pub async fn create_score(
    services: &AppServices,
    request_id: String,
    track_id: String,
    venue_id: String,
    name: Option<String>,
) -> Result<Score, CommandError> {
    Ok(services
        .authored
        .create_score(
            &services.db.0,
            &request_id,
            &track_id,
            &venue_id,
            name.as_deref(),
        )
        .await?)
}

/// Idempotent venue-membership operation. Unlike [`create_score`], a fresh
/// request id still returns an existing score for the track/venue pair.
pub async fn ensure_venue_score(
    services: &AppServices,
    request_id: String,
    track_id: String,
    venue_id: String,
    name: Option<String>,
) -> Result<Score, CommandError> {
    Ok(services
        .authored
        .ensure_venue_score(
            &services.db.0,
            &request_id,
            &track_id,
            &venue_id,
            name.as_deref(),
        )
        .await?)
}

/// Archives the authored document — history is preserved, not rewritten — and
/// wakes the sync push loop.
pub async fn delete_score(services: &AppServices, id: String) -> Result<(), CommandError> {
    let principal = services.session_user_id().await?;
    services
        .authored
        .archive_score(&services.db.0, principal.as_deref(), &id)
        .await?;
    services.sync.push_notify.notify_one();
    Ok(())
}

pub async fn list_track_scores(
    services: &AppServices,
    score_id: String,
) -> Result<Vec<TrackScore>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Score(&score_id)).await?;
    Ok(db::list_track_scores_for_score(&mut access, &score_id).await?)
}

pub async fn create_track_score(
    services: &AppServices,
    payload: CreateTrackScoreInput,
) -> Result<TrackEditResult, CommandError> {
    Ok(score_mutations::create_track_score(&services.authored, &services.db.0, payload).await?)
}

pub async fn update_track_score(
    services: &AppServices,
    payload: UpdateTrackScoreInput,
) -> Result<TrackEditResult, CommandError> {
    Ok(score_mutations::update_track_score(&services.authored, &services.db.0, payload).await?)
}

pub async fn delete_track_score(
    services: &AppServices,
    payload: DeleteTrackScoreInput,
) -> Result<TrackEditResult, CommandError> {
    Ok(score_mutations::delete_track_score(&services.authored, &services.db.0, payload).await?)
}

/// Whole-document compare-and-swap: `base_scores` is the snapshot the caller
/// edited, `scores` the candidate. Idempotent on `operation_id`, which is what
/// makes the frontend's blind single retry safe.
pub async fn replace_track_scores(
    services: &AppServices,
    score_id: String,
    track_id: String,
    base_scores: Vec<TrackScore>,
    scores: Vec<TrackScore>,
    operation_id: String,
) -> Result<TrackEditResult, CommandError> {
    Ok(score_mutations::replace_track_scores(
        &services.authored,
        &services.db.0,
        &score_id,
        &track_id,
        &base_scores,
        &scores,
        &operation_id,
    )
    .await?)
}
