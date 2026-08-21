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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::SqlitePool;

    use super::*;
    use crate::models::node_graph::BlendMode;
    use crate::services::authored_documents::tests::Fixture;

    /// The stored clip list in the order every revision hash is taken over, so
    /// a snapshot read here is a valid `base` for the next replacement.
    async fn stored(pool: &SqlitePool, score_id: &str) -> Vec<TrackScore> {
        sqlx::query_as::<_, TrackScore>(
            "SELECT id, uid, score_id, pattern_id, start_time, end_time, z_index,
                    blend_mode, args_json, created_at, updated_at
             FROM track_scores
             WHERE score_id = ?
             ORDER BY start_time, z_index, id",
        )
        .bind(score_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// A stable, distinct UUID per seed: every idempotency key on this path is
    /// required to be one, and a test that replays an operation needs the same
    /// key twice.
    fn key(seed: u128) -> String {
        uuid::Uuid::from_u128(seed).to_string()
    }

    fn create(request_id: u128, start_time: f64, end_time: f64) -> CreateTrackScoreInput {
        CreateTrackScoreInput {
            request_id: key(request_id),
            score_id: "score".into(),
            track_id: "track".into(),
            pattern_id: "pattern".into(),
            start_time,
            end_time,
            z_index: 0,
            blend_mode: None,
            args: None,
        }
    }

    /// A clip row shaped like a candidate: only the semantic fields are read,
    /// and a client-minted id that is absent from `base` reads as a create.
    fn candidate(id: &str, start_time: f64, end_time: f64, z_index: i64) -> TrackScore {
        TrackScore {
            id: id.to_string(),
            uid: None,
            score_id: "score".into(),
            pattern_id: "pattern".into(),
            start_time,
            end_time,
            z_index,
            blend_mode: BlendMode::Replace,
            args: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[tokio::test]
    async fn create_is_idempotent_on_its_request_id() {
        let fixture = Fixture::new().await;
        fixture.track_scope().await;

        let first = create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap();
        let clip_id = first.created_clip_id.clone().expect("a created clip id");

        let replay = create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap();
        assert_eq!(replay.created_clip_id.as_deref(), Some(clip_id.as_str()));

        let rows = stored(&fixture.pool, "score").await;
        assert_eq!(rows.len(), 1, "a replay must not mint a second clip");
        assert_eq!(rows[0].id, clip_id);
    }

    #[tokio::test]
    async fn update_moves_one_clip_and_leaves_the_rest_alone() {
        let fixture = Fixture::new().await;
        fixture.track_scope().await;
        let moved = create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap()
            .created_clip_id
            .unwrap();
        create_track_score(&fixture.authored, &fixture.pool, create(2, 10.0, 12.0))
            .await
            .unwrap();

        update_track_score(
            &fixture.authored,
            &fixture.pool,
            UpdateTrackScoreInput {
                operation_id: key(9),
                score_id: "score".into(),
                track_id: "track".into(),
                id: moved.clone(),
                start_time: Some(4.0),
                end_time: Some(6.0),
                z_index: Some(2),
                blend_mode: None,
                args: None,
            },
        )
        .await
        .unwrap();

        let rows = stored(&fixture.pool, "score").await;
        let edited = rows.iter().find(|row| row.id == moved).unwrap();
        assert_eq!(
            (edited.start_time, edited.end_time, edited.z_index),
            (4.0, 6.0, 2)
        );
        let untouched = rows.iter().find(|row| row.id != moved).unwrap();
        assert_eq!((untouched.start_time, untouched.end_time), (10.0, 12.0));
    }

    /// The response-loss case: the clip row is already gone, and the retry
    /// must still resolve rather than fail as "no such clip".
    #[tokio::test]
    async fn delete_replays_after_the_clip_is_gone() {
        let fixture = Fixture::new().await;
        fixture.track_scope().await;
        let clip = create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap()
            .created_clip_id
            .unwrap();

        let input = DeleteTrackScoreInput {
            operation_id: key(9),
            score_id: "score".into(),
            track_id: "track".into(),
            id: clip,
        };
        delete_track_score(&fixture.authored, &fixture.pool, input.clone())
            .await
            .unwrap();
        assert!(stored(&fixture.pool, "score").await.is_empty());

        delete_track_score(&fixture.authored, &fixture.pool, input)
            .await
            .unwrap();
        assert!(stored(&fixture.pool, "score").await.is_empty());
    }

    /// The same response-loss case one step earlier: a retried update whose
    /// clip a *later* command has since deleted still replays its own outcome
    /// rather than reporting the clip missing.
    #[tokio::test]
    async fn update_replays_after_the_clip_is_gone() {
        let fixture = Fixture::new().await;
        fixture.track_scope().await;
        let clip = create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap()
            .created_clip_id
            .unwrap();

        let input = UpdateTrackScoreInput {
            operation_id: key(9),
            score_id: "score".into(),
            track_id: "track".into(),
            id: clip.clone(),
            start_time: Some(4.0),
            end_time: Some(6.0),
            z_index: None,
            blend_mode: None,
            args: None,
        };
        update_track_score(&fixture.authored, &fixture.pool, input.clone())
            .await
            .unwrap();
        delete_track_score(
            &fixture.authored,
            &fixture.pool,
            DeleteTrackScoreInput {
                operation_id: key(10),
                score_id: "score".into(),
                track_id: "track".into(),
                id: clip,
            },
        )
        .await
        .unwrap();

        let replay = update_track_score(&fixture.authored, &fixture.pool, input)
            .await
            .unwrap();
        assert!(
            !replay.applied_to_current_projection,
            "a replay behind a newer edit must say so rather than resurrect its clip"
        );
        assert!(stored(&fixture.pool, "score").await.is_empty());
    }

    /// One gesture, three kinds of change: the shape every multi-clip editor
    /// command (duplicate, split, delete-selection, paste) reduces to.
    #[tokio::test]
    async fn replace_lands_a_create_an_update_and_a_delete_together() {
        let fixture = Fixture::new().await;
        fixture.track_scope().await;
        create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap();
        create_track_score(&fixture.authored, &fixture.pool, create(2, 10.0, 12.0))
            .await
            .unwrap();

        let base = stored(&fixture.pool, "score").await;
        let kept = base[0].id.clone();
        let mut next = vec![candidate(&kept, 1.0, 5.0, 0)];
        next.push(candidate("ui-minted", 20.0, 22.0, 1));

        let result = replace_track_scores(
            &fixture.authored,
            &fixture.pool,
            "score",
            "track",
            &base,
            &next,
            &key(9),
        )
        .await
        .unwrap();
        assert_eq!((result.added, result.updated, result.removed), (1, 1, 1));

        let rows = stored(&fixture.pool, "score").await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, kept);
        assert_eq!((rows[0].start_time, rows[0].end_time), (1.0, 5.0));
        // The seam allocated the created clip's real id and said which draft
        // it answers, so the caller can rebase its selection onto it.
        let allocated = result.id_map.get("ui-minted").expect("a mapped id");
        assert_eq!(&rows[1].id, allocated);
    }

    /// Atomicity: one bad clip anywhere in the candidate and *nothing* moves.
    /// A partial apply would leave the timeline in a state its own rules
    /// forbid, which no retry could repair.
    #[tokio::test]
    async fn replace_rejects_the_whole_batch_when_one_clip_is_invalid() {
        let fixture = Fixture::new().await;
        fixture.track_scope().await;
        create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap();
        create_track_score(&fixture.authored, &fixture.pool, create(2, 10.0, 12.0))
            .await
            .unwrap();

        let base = stored(&fixture.pool, "score").await;
        // A legal move of the first clip, a legal delete of the second, and a
        // create naming a pattern that does not exist.
        let mut next = vec![candidate(&base[0].id, 1.0, 5.0, 0)];
        let mut bad = candidate("ui-minted", 20.0, 22.0, 1);
        bad.pattern_id = "no-such-pattern".into();
        next.push(bad);

        let error = replace_track_scores(
            &fixture.authored,
            &fixture.pool,
            "score",
            "track",
            &base,
            &next,
            &key(9),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("no-such-pattern"),
            "expected the offending pattern to be named, got {error}"
        );

        let rows = stored(&fixture.pool, "score").await;
        assert_eq!(rows.len(), base.len());
        for (before, after) in base.iter().zip(&rows) {
            assert_eq!(before.id, after.id);
            assert_eq!(before.start_time, after.start_time);
            assert_eq!(before.end_time, after.end_time);
        }
    }

    /// A stale `base` is a conflict, not a last-writer-wins overwrite: the
    /// caller edited a list that has since moved.
    #[tokio::test]
    async fn replace_refuses_a_candidate_built_on_a_stale_base() {
        let fixture = Fixture::new().await;
        fixture.track_scope().await;
        create_track_score(&fixture.authored, &fixture.pool, create(1, 1.0, 3.0))
            .await
            .unwrap();
        let stale = stored(&fixture.pool, "score").await;
        create_track_score(&fixture.authored, &fixture.pool, create(2, 10.0, 12.0))
            .await
            .unwrap();

        let next = vec![candidate(&stale[0].id, 1.0, 5.0, 0)];
        let error = replace_track_scores(
            &fixture.authored,
            &fixture.pool,
            "score",
            "track",
            &stale,
            &next,
            &key(9),
        )
        .await
        .unwrap_err();

        let rows = stored(&fixture.pool, "score").await;
        assert_eq!(
            rows.len(),
            2,
            "a stale replacement must not delete: {error}"
        );
    }
}
