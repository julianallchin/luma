use crate::database::local::scores::{self as db, rows};
use crate::database::local::tracks::Grid;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::scores::{Score, ScoreSummary};
use crate::services::catalog;
use crate::sync::triggers::Attribution;

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

/// Write an editor's changes over the score's rows as they stand. A clip the
/// edit does not name keeps its stored value, so an agent's write that landed
/// after the editor read the score survives the editor's next save.
pub async fn edit_score_document(
    services: &AppServices,
    score_id: String,
    edit: luma_patterns::ScoreEdit,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Score(&score_id)).await?;
    let stored = rows::load_score(access.connection(), &score_id).await?;
    let edited = edit.applied_to(&stored);
    // Check each clip this write changes. A stored clip that no longer
    // passes, such as one from before a form changed, does not block edits
    // to the rest of the score.
    edited
        .validate_changes(&luma_patterns::standard_library(), &stored)
        .map_err(|error| CommandError::Invalid(error.to_string()))?;
    rows::save_score(access.connection(), &score_id, &edited).await?;
    access.commit().await?;
    Ok(())
}

/// Each clip's strip, alone on the venue: see
/// [`crate::services::graph_scores::preview_clips`].
pub async fn preview_score_clips(
    services: &AppServices,
    score_id: String,
    clip_ids: Vec<String>,
    score: Option<luma_patterns::Score>,
) -> Result<Vec<crate::services::graph_scores::ClipStrip>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Score(&score_id)).await?;
    let metadata = db::get_score(&mut access, &score_id).await?;
    let candidate = match score {
        Some(score) => score,
        None => rows::load_score(access.connection(), &score_id).await?,
    };
    Ok(crate::services::graph_scores::preview_clips(
        &mut access,
        &services.fixtures_root,
        &services.storage,
        Grid::of_score(&metadata.track_id, &score_id),
        &candidate,
        &clip_ids,
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
        &services.controls,
        Grid::of_score(&metadata.track_id, &score_id),
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
    services.require_session().await?;
    Ok(db::list_accessible_scores_for_track(&services.db.0, &track_id).await?)
}

/// A new score on the track/venue pair, beside any the pair already has.
pub async fn create_score(
    services: &AppServices,
    track_id: String,
    venue_id: String,
    name: Option<String>,
) -> Result<Score, CommandError> {
    Ok(catalog::create_score(
        &services.db.0,
        &track_id,
        &venue_id,
        name.as_deref(),
        Attribution::PERSON,
    )
    .await?)
}

/// Idempotent venue-membership operation. Unlike [`create_score`], this
/// returns an existing score for the track/venue pair.
pub async fn ensure_venue_score(
    services: &AppServices,
    track_id: String,
    venue_id: String,
    name: Option<String>,
) -> Result<Score, CommandError> {
    Ok(catalog::ensure_venue_score(
        &services.db.0,
        &track_id,
        &venue_id,
        name.as_deref(),
        Attribution::PERSON,
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

/// Copy a score and its clips into a new score of the same song in the same
/// venue, named `name`.
pub async fn duplicate_score(
    services: &AppServices,
    score_id: String,
    name: String,
) -> Result<Score, CommandError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::Invalid("A score name cannot be empty".into()));
    }
    Ok(catalog::duplicate_score(&services.db.0, &score_id, name).await?)
}

/// A delete is a delete: the score's clips, definitions and drafts go with it.
pub async fn delete_score(services: &AppServices, id: String) -> Result<(), CommandError> {
    Ok(catalog::delete_score(&services.db.0, &id).await?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use crate::dispatch::{dispatch, AppServices};

    /// A signed-in library with one venue, one track and one empty score.
    async fn library() -> (tempfile::TempDir, AppServices, String) {
        let directory = tempfile::tempdir().unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let services = crate::test_support::signed_in(directory.path(), repo).await;

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

        (directory, services, score)
    }

    #[tokio::test]
    async fn rename_score_trims_refuses_empty_and_writes_the_name() {
        let (_directory, services, score) = library().await;
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

    /// A copy is a new score of the same song and venue, with the source's
    /// clips and the given name; the source is untouched. The copy's clips are rows of their own:
    /// new ids, with the labels the source's clips have.
    #[tokio::test]
    async fn duplicate_score_copies_the_clips_under_a_new_name() {
        let (_directory, services, id) = library().await;
        let mut labelled = clip("A", 0.);
        labelled.label = Some("intro".into());
        let clips = score(&[("a", &labelled), ("b", &clip("B", 8.))]);
        edit(&services, &id, &luma_patterns::Score::default(), &clips).await;

        let args = json!({
            "scoreId": id,
            "name": "Draft copy",
        });
        let copy = dispatch(&services, "duplicate_score", &args).await.unwrap();
        let copy_id = copy["id"].as_str().unwrap().to_owned();
        assert_ne!(copy_id, id);
        assert_eq!(copy["name"], "Draft copy");
        assert_eq!(copy["trackId"], "track");
        let copied = stored(&services, &copy_id).await;
        assert!(copied.clips.keys().all(|id| !clips.clips.contains_key(id)));
        let by_start = |score: &luma_patterns::Score| {
            let mut clips: Vec<_> = score.clips.values().cloned().collect();
            clips.sort_by(|a, b| a.start.total_cmp(&b.start));
            clips
        };
        assert_eq!(by_start(&copied), by_start(&clips));
        assert_eq!(stored(&services, &id).await, clips);
        let source: Option<String> =
            sqlx::query_scalar("SELECT copied_from_score_id FROM scores WHERE id = ?")
                .bind(&copy_id)
                .fetch_one(&services.db.0)
                .await
                .unwrap();
        assert_eq!(
            source.as_deref(),
            Some(id.as_str()),
            "a copy names its source"
        );

        // An edit to the copy leaves the source alone: the clips are its own.
        edit(
            &services,
            &copy_id,
            &copied,
            &luma_patterns::Score::default(),
        )
        .await;
        assert_eq!(stored(&services, &id).await, clips);
    }

    fn clip(name: &str, start: f64) -> luma_patterns::Clip {
        let mut clip = luma_patterns::presets()
            .clip("Wash")
            .expect("the Wash preset")
            .clip(start, 4.);
        clip.name = name.into();
        clip
    }

    fn score(clips: &[(&str, &luma_patterns::Clip)]) -> luma_patterns::Score {
        luma_patterns::Score {
            clips: clips
                .iter()
                .map(|(id, clip)| ((*id).to_owned(), (*clip).clone()))
                .collect(),
        }
    }

    async fn edit(
        services: &AppServices,
        id: &str,
        base: &luma_patterns::Score,
        candidate: &luma_patterns::Score,
    ) {
        let edit = luma_patterns::ScoreEdit::between(base, candidate);
        dispatch(
            services,
            "edit_score_document",
            &json!({ "scoreId": id, "edit": edit }),
        )
        .await
        .unwrap();
    }

    async fn stored(services: &AppServices, id: &str) -> luma_patterns::Score {
        serde_json::from_value(
            dispatch(services, "get_score_document", &json!({ "scoreId": id }))
                .await
                .unwrap(),
        )
        .unwrap()
    }

    /// One call previews every clip asked for, each on its own: a clip that
    /// is not in the score fails alone.
    #[tokio::test]
    async fn previews_come_back_per_clip_and_one_failure_stays_its_own() {
        let (_directory, services, id) = library().await;
        sqlx::query(
            "INSERT INTO track_beats (id, track_id, beats_json, downbeats_json, bpm, beats_per_bar)
             VALUES ('grid', 'track', '[0.0,0.5,1.0,1.5,2.0,2.5,3.0,3.5,4.0,4.5,5.0,5.5,6.0,6.5,7.0,7.5,8.0,8.5,9.0,9.5,10.0]', '[0.0,2.0,4.0,6.0,8.0,10.0]', 120.0, 4)",
        )
        .execute(&services.db.0)
        .await
        .unwrap();
        let candidate = score(&[("a", &clip("A", 0.)), ("b", &clip("B", 8.))]);

        let strips: Vec<crate::services::graph_scores::ClipStrip> = serde_json::from_value(
            dispatch(
                &services,
                "preview_score_clips",
                &json!({ "scoreId": id, "clipIds": ["a", "ghost", "b"], "score": candidate }),
            )
            .await
            .unwrap(),
        )
        .unwrap();

        let mut by_clip: std::collections::BTreeMap<_, _> = strips
            .into_iter()
            .map(|strip| (strip.clip_id, strip.preview))
            .collect();
        assert_eq!(by_clip.len(), 3);
        for clip in ["a", "b"] {
            let preview = by_clip.remove(clip).unwrap().unwrap();
            assert_eq!(preview.annotation_id, clip);
            assert!(preview.width > 0);
        }
        let ghost = by_clip.remove("ghost").unwrap().unwrap_err();
        assert!(ghost.contains("unknown clip ghost"), "{ghost}");
    }

    /// The editor read {A}; an agent then added B. The editor's save of A′
    /// over its stale copy keeps B.
    #[tokio::test]
    async fn an_edit_keeps_a_clip_another_writer_added_since_its_read() {
        let (_directory, services, id) = library().await;
        let (a, edited_a, b) = (clip("A", 0.), clip("A", 8.), clip("B", 16.));
        let read = score(&[("a", &a)]);
        edit(&services, &id, &luma_patterns::Score::default(), &read).await;
        edit(&services, &id, &read, &score(&[("a", &a), ("b", &b)])).await;

        edit(&services, &id, &read, &score(&[("a", &edited_a)])).await;
        assert_eq!(
            stored(&services, &id).await,
            score(&[("a", &edited_a), ("b", &b)])
        );
    }

    /// The editor read {A, C}; an agent then deleted C. The editor's save of
    /// an edit to A does not bring C back.
    #[tokio::test]
    async fn an_edit_does_not_restore_a_clip_another_writer_deleted() {
        let (_directory, services, id) = library().await;
        let (a, edited_a, c) = (clip("A", 0.), clip("A", 8.), clip("C", 16.));
        let read = score(&[("a", &a), ("c", &c)]);
        edit(&services, &id, &luma_patterns::Score::default(), &read).await;
        edit(&services, &id, &read, &score(&[("a", &a)])).await;

        edit(
            &services,
            &id,
            &read,
            &score(&[("a", &edited_a), ("c", &c)]),
        )
        .await;
        assert_eq!(stored(&services, &id).await, score(&[("a", &edited_a)]));
    }
}
