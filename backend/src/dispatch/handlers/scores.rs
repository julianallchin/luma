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
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Score(&score_id)).await?;
    let metadata = db::get_score(&mut access, &score_id).await?;
    // Check each clip this write changes. A stored clip that no longer
    // passes, such as one from before a form changed, does not block edits
    // to the rest of the score.
    let stored = rows::load_score(access.connection(), &score_id).await?;
    score
        .validate_changes(&luma_patterns::standard_library(), &stored)
        .map_err(|error| CommandError::Invalid(error.to_string()))?;
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

/// One definition record per clip-graph node kind: inputs with type, unit,
/// range and axes, and settings with their options. The inspector, Python
/// and the checker read the same records.
pub async fn clip_graph_definitions(
    _services: &AppServices,
) -> Result<serde_json::Value, CommandError> {
    serde_json::to_value(luma_patterns::clip_graph::definitions())
        .map_err(|error| CommandError::Internal(error.to_string()))
}

/// The shipped clip presets, curves, gradients and bands.
pub async fn clip_presets(_services: &AppServices) -> Result<serde_json::Value, CommandError> {
    serde_json::to_value(luma_patterns::presets())
        .map_err(|error| CommandError::Internal(error.to_string()))
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

/// Rename a score. The name is trimmed; an empty one is refused, because a
/// score is always shown by its name.
pub async fn rename_score(
    services: &AppServices,
    score_id: String,
    name: String,
) -> Result<(), CommandError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::Invalid("A score name cannot be empty".into()));
    }
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Score(&score_id)).await?;
    db::rename_score(&mut access, &score_id, name).await?;
    access.commit().await?;
    Ok(())
}

/// A delete is a delete: the score's clips, definitions and drafts go with it.
pub async fn delete_score(services: &AppServices, id: String) -> Result<(), CommandError> {
    Ok(catalog::delete_score(&services.db.0, &id).await?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices};

    const OWNER: &str = "11111111-2222-3333-4444-555555555555";

    #[tokio::test]
    async fn rename_score_trims_refuses_empty_and_writes_the_name() {
        let directory = tempfile::tempdir().unwrap();
        let db = database::init_app_db_at(directory.path()).await.unwrap();
        let state_db = state::init_state_db_at(directory.path()).await.unwrap();
        auth::install_test_session(&state_db.0, OWNER).await;
        auth::bootstrap_headless_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.path().to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let services = AppServices::headless(db, state_db, storage, repo, workspaces);

        let venue = dispatch(
            &services,
            "create_venue",
            &json!({ "name": "Room", "description": null }),
        )
        .await
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let uid: Option<String> = sqlx::query_scalar("SELECT uid FROM venues WHERE id = ?")
            .bind(&venue)
            .fetch_one(&services.db.0)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('track', ?, 'h', '/t.wav')",
        )
        .bind(&uid)
        .execute(&services.db.0)
        .await
        .unwrap();
        let score = dispatch(
            &services,
            "create_score",
            &json!({
                "requestId": uuid::Uuid::new_v4().to_string(),
                "trackId": "track",
                "venueId": venue,
                "name": "Draft",
            }),
        )
        .await
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let name = |services: &AppServices| {
            let pool = services.db.0.clone();
            let score = score.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>("SELECT name FROM scores WHERE id = ?")
                    .bind(score)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };

        dispatch(
            &services,
            "rename_score",
            &json!({ "scoreId": score, "name": "  Opening set  " }),
        )
        .await
        .unwrap();
        assert_eq!(name(&services).await.as_deref(), Some("Opening set"));

        assert!(dispatch(
            &services,
            "rename_score",
            &json!({ "scoreId": score, "name": "   " }),
        )
        .await
        .is_err());
        assert_eq!(name(&services).await.as_deref(), Some("Opening set"));

        assert!(dispatch(
            &services,
            "rename_score",
            &json!({ "scoreId": "no-such-score", "name": "Other" }),
        )
        .await
        .is_err());
    }
}
