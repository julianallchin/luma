use crate::database::local::scores as db;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::dispatch::{AppServices, CommandError};
use crate::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, ScoreSummary, TrackScore,
    UpdateTrackScoreInput,
};
use crate::services::score_mutations;
use crate::services::track_edits::TrackEditResult;

/// Opening an owned historical score records its complete conversion as one
/// authored revision. Another member's score gets a read-only projection; its
/// stored source and revision remain untouched.
pub async fn get_score_document(
    services: &AppServices,
    score_id: String,
) -> Result<Option<crate::services::graph_scores::GraphScoreDocument>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Score(&score_id)).await?;
    let score = db::get_score(&mut access, &score_id).await?;
    let scope = crate::services::track_edits::TrackScope {
        score_id,
        track_id: score.track_id,
        venue_id: score.venue_id,
    };
    let editable = access.principal() == score.uid.as_deref();
    let document = crate::services::graph_scores::read_score_document(&mut access, &scope).await?;
    let (revision, mut upgraded) = match document {
        crate::services::graph_scores::ScoreDocument::Graph(document) => {
            if document.score.version() != 2 {
                return Ok(Some(document));
            }
            let upgraded = crate::services::graph_scores::GraphScoreDocument::new(
                luma_patterns::migration::upgrade_v2(&document.score)
                    .map_err(|error| CommandError::Invalid(error.to_string()))?,
            )
            .map_err(CommandError::Invalid)?;
            (document.revision, upgraded)
        }
        crate::services::graph_scores::ScoreDocument::Legacy(document) => {
            let Some(upgraded) = crate::services::graph_scores::migration::upgrade_rows(
                &mut access,
                &scope,
                &document,
            )
            .await?
            else {
                return Ok(None);
            };
            (document.revision, upgraded)
        }
    };
    drop(access);
    if editable {
        upgraded = services
            .authored
            .upgrade_score_for_scope(
                &services.db.0,
                score.uid.as_deref(),
                scope,
                upgraded,
                &revision,
            )
            .await?;
        services.sync.push_notify.notify_one();
    }
    Ok(Some(upgraded))
}

pub async fn apply_score_document(
    services: &AppServices,
    score_id: String,
    score: luma_patterns::Score,
    base_revision: String,
    operation_id: String,
) -> Result<crate::models::authored_state::AppliedAuthoredState, CommandError> {
    let candidate = crate::services::graph_scores::GraphScoreDocument::new(score)
        .map_err(CommandError::Invalid)?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Score(&score_id)).await?;
    let metadata = db::get_score(&mut access, &score_id).await?;
    let owner = access.principal().map(str::to_owned);
    let scope = crate::services::track_edits::TrackScope {
        score_id,
        track_id: metadata.track_id,
        venue_id: metadata.venue_id,
    };
    drop(access);
    let result = services
        .authored
        .apply_score_source_for_scope(
            &services.db.0,
            owner.as_deref(),
            scope,
            &operation_id,
            &candidate.source().map_err(CommandError::Invalid)?,
            &base_revision,
            "Edit score",
        )
        .await?;
    services.sync.push_notify.notify_one();
    Ok(result)
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
    let scope = crate::services::track_edits::TrackScope {
        score_id,
        track_id: metadata.track_id,
        venue_id: metadata.venue_id,
    };
    let candidate = match score {
        Some(score) => crate::services::graph_scores::GraphScoreDocument::new(score)
            .map_err(CommandError::Invalid)?,
        None => crate::services::graph_scores::load(
            access.connection(),
            &scope,
            metadata.uid.as_deref(),
        )
        .await?
        .ok_or_else(|| {
            CommandError::Invalid("this score has not been migrated to graphs".into())
        })?,
    };
    Ok(crate::services::graph_scores::preview_clip(
        &mut access,
        &services.fixtures_root,
        &services.storage,
        &scope.track_id,
        &candidate.score,
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
    let candidate = crate::services::graph_scores::GraphScoreDocument::new(score)
        .map_err(CommandError::Invalid)?;
    Ok(crate::services::graph_scores::prepare_clip_preview(
        &mut access,
        &services.fixtures_root,
        &services.storage,
        &metadata.track_id,
        &candidate.score,
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
