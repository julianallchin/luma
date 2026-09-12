use crate::database::local::scores::{self as db, rows};
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::dispatch::{AppServices, CommandError};
use crate::models::scores::{Score, ScoreSummary};
use crate::services::catalog;

/// The score's document, assembled from its rows. `None` only when the id
/// names no score.
pub async fn get_score_document(
    services: &AppServices,
    score_id: String,
) -> Result<Option<luma_patterns::Score>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Score(&score_id)).await?;
    db::get_score(&mut access, &score_id).await?;
    Ok(Some(
        rows::load_score(access.connection(), &score_id).await?,
    ))
}

/// Write the candidate onto the score's rows. Only what differs moves, so two
/// people editing different clips of one score do not overwrite each other —
/// and there is no revision to compare, because a row diff needs none.
pub async fn apply_score_document(
    services: &AppServices,
    score_id: String,
    score: luma_patterns::Score,
) -> Result<(), CommandError> {
    score
        .validate(&luma_patterns::standard_library())
        .map_err(|error| CommandError::Invalid(error.to_string()))?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Score(&score_id)).await?;
    let metadata = db::get_score(&mut access, &score_id).await?;
    rows::save_score(
        access.connection(),
        &score_id,
        metadata.uid.as_deref().unwrap_or_default(),
        &score,
    )
    .await?;
    access.commit().await?;
    Ok(())
}

pub async fn preview_score_clip(
    services: &AppServices,
    score_id: String,
    clip_id: String,
    score: Option<luma_patterns::Score>,
) -> Result<crate::models::patterns::AnnotationPreview, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Score(&score_id)).await?;
    let metadata = db::get_score(&mut access, &score_id).await?;
    let candidate = match score {
        Some(score) => score,
        None => rows::load_score(access.connection(), &score_id).await?,
    };
    Ok(crate::services::graph_scores::preview_clip(
        &mut access,
        &services.fixtures_root,
        &services.storage,
        &metadata.track_id,
        &candidate,
        &clip_id,
    )
    .await?)
}

/// Native preview programs cross the same authorized dispatch seam as image
/// previews, without serializing an executable scene or installing it globally.
pub async fn prepare_score_clip_preview(
    services: &AppServices,
    score_id: String,
    clip_id: String,
    score: luma_patterns::Score,
) -> Result<crate::services::graph_scores::ClipPreview, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Score(&score_id)).await?;
    let metadata = db::get_score(&mut access, &score_id).await?;
    Ok(crate::services::graph_scores::prepare_clip_preview(
        &mut access,
        &services.fixtures_root,
        &services.storage,
        &metadata.track_id,
        &score,
        &clip_id,
    )
    .await?)
}

/// This track's scores in one venue, newest first. A track/venue pair holds
/// more than one score whenever more than one principal has annotated it.
pub async fn list_scores_for_track(
    services: &AppServices,
    track_id: String,
    venue_id: String,
) -> Result<Vec<ScoreSummary>, CommandError> {
    let pool = &services.db.0;
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
    Ok(db::list_scores_for_track(&mut access, &track_id).await?)
}

/// Every score for a track that the current admission may see, in any venue.
/// Filtered by the same admission the per-venue guard applies, in one
/// statement — so a caller with no venue in hand still cannot read a sealed
/// row.
pub async fn list_scores_across_venues(
    services: &AppServices,
    track_id: String,
) -> Result<Vec<ScoreSummary>, CommandError> {
    Ok(db::list_accessible_scores_for_track(&services.db.0, &track_id).await?)
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
    Ok(catalog::create_score(
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
    Ok(catalog::ensure_venue_score(
        &services.db.0,
        &request_id,
        &track_id,
        &venue_id,
        name.as_deref(),
    )
    .await?)
}

/// A delete is a delete: the score's clips, definitions and drafts go with it.
pub async fn delete_score(services: &AppServices, id: String) -> Result<(), CommandError> {
    Ok(catalog::delete_score(&services.db.0, &id).await?)
}
