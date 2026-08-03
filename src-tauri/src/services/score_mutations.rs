//! Command-shaped score mutations routed through the relational authored-state
//! authority.
//!
//! This is the shared adapter used by both Tauri and the headless harness. It
//! resolves caller-minimal IDs to a trusted score scope, rejects cross-owner
//! access before any authored state is created, and delegates every write to
//! [`AuthoredDocuments`].

use sqlx::SqlitePool;

use crate::database::local::scores as score_db;
use crate::database::local::venue_access::{VenueAccess, VenueResource, Write};
use crate::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, TrackScore, UpdateTrackScoreInput,
};
use crate::services::authored_documents::{AuthoredDocuments, AuthoredDocumentsError};
use crate::services::track_edits::{TrackEditResult, TrackScope};

type Result<T> = std::result::Result<T, AuthoredDocumentsError>;

pub async fn create_track_score(
    authored: &AuthoredDocuments,
    pool: &SqlitePool,
    payload: CreateTrackScoreInput,
) -> Result<TrackEditResult> {
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(&payload.score_id))
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let score = score_db::get_score(&mut access, &payload.score_id)
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let owner_user_id = access.principal().map(str::to_owned);
    let scope = scope_from_admitted_score(score, owner_user_id.as_deref())?;
    drop(access);
    let result = authored
        .create_track_score_for_scope(
            pool,
            owner_user_id.as_deref(),
            scope,
            payload,
            "Create track clip",
        )
        .await?;
    Ok(result.edit)
}

pub async fn update_track_score(
    authored: &AuthoredDocuments,
    pool: &SqlitePool,
    payload: UpdateTrackScoreInput,
) -> Result<TrackEditResult> {
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(&payload.score_id))
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let score = score_db::get_score(&mut access, &payload.score_id)
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let owner_user_id = access.principal().map(str::to_owned);
    let scope = scope_from_admitted_score(score, owner_user_id.as_deref())?;
    drop(access);
    if scope.track_id != payload.track_id {
        return Err(AuthoredDocumentsError::Scope(format!(
            "score {} belongs to track {}, not {}",
            scope.score_id, scope.track_id, payload.track_id
        )));
    }
    let result = authored
        .update_track_score_for_scope(
            pool,
            owner_user_id.as_deref(),
            scope,
            payload,
            "Update track clip",
        )
        .await?;
    Ok(result.edit)
}

pub async fn delete_track_score(
    authored: &AuthoredDocuments,
    pool: &SqlitePool,
    payload: DeleteTrackScoreInput,
) -> Result<TrackEditResult> {
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(&payload.score_id))
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let score = score_db::get_score(&mut access, &payload.score_id)
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let owner_user_id = access.principal().map(str::to_owned);
    let scope = scope_from_admitted_score(score, owner_user_id.as_deref())?;
    drop(access);
    if scope.track_id != payload.track_id {
        return Err(AuthoredDocumentsError::Scope(format!(
            "score {} belongs to track {}, not {}",
            scope.score_id, scope.track_id, payload.track_id
        )));
    }
    let result = authored
        .delete_track_score_for_scope(
            pool,
            owner_user_id.as_deref(),
            scope,
            payload,
            "Delete track clip",
        )
        .await?;
    Ok(result.edit)
}

pub async fn replace_track_scores(
    authored: &AuthoredDocuments,
    pool: &SqlitePool,
    score_id: &str,
    track_id: &str,
    base_scores: &[TrackScore],
    scores: &[TrackScore],
    operation_id: &str,
) -> Result<TrackEditResult> {
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(score_id))
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let score = score_db::get_score(&mut access, score_id)
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    let owner_user_id = access.principal().map(str::to_owned);
    let scope = scope_from_admitted_score(score, owner_user_id.as_deref())?;
    drop(access);
    if scope.track_id != track_id {
        return Err(AuthoredDocumentsError::Scope(format!(
            "score {score_id} belongs to track {}, not {track_id}",
            scope.track_id
        )));
    }
    Ok(authored
        .replace_track_scores_for_scope(
            pool,
            owner_user_id.as_deref(),
            scope,
            base_scores,
            scores,
            operation_id,
            "Replace track score",
        )
        .await?
        .edit)
}

fn scope_from_admitted_score(score: Score, principal: Option<&str>) -> Result<TrackScope> {
    if score.uid.as_deref() != principal {
        return Err(AuthoredDocumentsError::Scope(format!(
            "score {} is not owned by the current principal",
            score.id
        )));
    }
    Ok(TrackScope {
        score_id: score.id,
        track_id: score.track_id,
        venue_id: score.venue_id,
    })
}
