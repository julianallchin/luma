use super::super::*;
use super::support::*;

#[tokio::test]
async fn concurrent_partial_score_mutations_compose_under_one_repository_lock() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("wash").await;
    let scope = fixture.add_track_scope().await;
    let first = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            CreateTrackScoreInput {
                request_id: "00000000-0000-4000-8000-000000000004".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                pattern_id: "wash".into(),
                start_time: 0.0,
                end_time: 1.0,
                z_index: 0,
                blend_mode: None,
                args: Some(json!({})),
            },
            "Create first clip",
        )
        .await
        .unwrap();
    let second = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            CreateTrackScoreInput {
                request_id: "00000000-0000-4000-8000-000000000005".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                pattern_id: "wash".into(),
                start_time: 2.0,
                end_time: 3.0,
                z_index: 0,
                blend_mode: None,
                args: Some(json!({})),
            },
            "Create second clip",
        )
        .await
        .unwrap();
    let first_id = created_clip_id(&first).to_owned();
    let second_id = created_clip_id(&second).to_owned();

    let (left, right) = tokio::join!(
        fixture.authored.update_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            UpdateTrackScoreInput {
                operation_id: "concurrent-left".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: first_id.clone(),
                start_time: None,
                end_time: None,
                z_index: None,
                blend_mode: None,
                args: Some(json!({"side": "left"})),
            },
            "Update first clip",
        ),
        fixture.authored.update_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            UpdateTrackScoreInput {
                operation_id: "concurrent-right".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: second_id.clone(),
                start_time: None,
                end_time: None,
                z_index: None,
                blend_mode: None,
                args: Some(json!({"side": "right"})),
            },
            "Update second clip",
        )
    );
    left.unwrap();
    right.unwrap();
    let document = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();
    assert_eq!(document.clips.len(), 2);
    assert_eq!(
        document
            .clips
            .iter()
            .find(|clip| clip.id == first_id)
            .unwrap()
            .args,
        json!({"side": "left"})
    );
    assert_eq!(
        document
            .clips
            .iter()
            .find(|clip| clip.id == second_id)
            .unwrap()
            .args,
        json!({"side": "right"})
    );

    // Same-clip calls also both succeed: the lock gives them a total order and
    // the final value is exactly one complete update, never a torn mixture.
    let (one, two) = tokio::join!(
        fixture.authored.update_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            UpdateTrackScoreInput {
                operation_id: "same-clip-one".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: first_id.clone(),
                start_time: Some(4.0),
                end_time: Some(5.0),
                z_index: None,
                blend_mode: None,
                args: None,
            },
            "Move first clip once",
        ),
        fixture.authored.update_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            UpdateTrackScoreInput {
                operation_id: "same-clip-two".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: first_id.clone(),
                start_time: Some(6.0),
                end_time: Some(7.0),
                z_index: None,
                blend_mode: None,
                args: None,
            },
            "Move first clip twice",
        )
    );
    one.unwrap();
    two.unwrap();
    let document = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();
    let clip = document
        .clips
        .iter()
        .find(|clip| clip.id == first_id)
        .unwrap();
    assert!(
        (clip.start_time.to_bits() == 4.0f64.to_bits()
            && clip.end_time.to_bits() == 5.0f64.to_bits())
            || (clip.start_time.to_bits() == 6.0f64.to_bits()
                && clip.end_time.to_bits() == 7.0f64.to_bits())
    );
}

#[tokio::test]
async fn score_dsl_import_replays_without_reallocating_ids_or_rewinding_main() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("wash").await;
    let scope = fixture.add_track_scope().await;
    let initial = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();
    let source = "wash[\"wash\"]() @0s-1s";
    let first = fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "dsl-import-one",
            source,
            &initial.revision,
            "Import score source",
        )
        .await
        .unwrap();
    let imported_revision = match &first.document {
        AuthoredProjectedDocument::TrackScore { revision } => revision.clone(),
        AuthoredProjectedDocument::PatternGraph { .. } => {
            panic!("score import returned a graph document")
        }
    };
    let imported = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();
    assert_eq!(imported.clips.len(), 1);
    let imported_id = imported.clips[0].id.clone();

    fixture
        .authored
        .update_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            UpdateTrackScoreInput {
                operation_id: "advance-after-dsl-import".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: imported_id.clone(),
                start_time: None,
                end_time: Some(2.0),
                z_index: None,
                blend_mode: None,
                args: None,
            },
            "Advance imported clip",
        )
        .await
        .unwrap();

    let replay = fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "dsl-import-one",
            source,
            &initial.revision,
            "Retry score import after response loss",
        )
        .await
        .unwrap();
    assert_eq!(replay.commit_id, first.commit_id);
    let replay_revision = match replay.document {
        AuthoredProjectedDocument::TrackScore { revision } => revision,
        AuthoredProjectedDocument::PatternGraph { .. } => {
            panic!("score import replay returned a graph document")
        }
    };
    assert_ne!(replay_revision, imported_revision);
    let current = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();
    assert_eq!(current.revision, replay_revision);
    assert_eq!(current.clips.len(), 1);
    assert_eq!(current.clips[0].id, imported_id);
    assert_eq!(current.clips[0].end_time, 2.0);
}

#[tokio::test]
async fn track_clip_creation_replays_original_outcome_without_duplication() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("wash").await;
    let scope = fixture.add_track_scope().await;
    let payload = CreateTrackScoreInput {
        request_id: "10000000-0000-4000-8000-000000000001".into(),
        score_id: scope.score_id.clone(),
        track_id: scope.track_id.clone(),
        pattern_id: "wash".into(),
        start_time: 1.0,
        end_time: 2.0,
        z_index: 3,
        blend_mode: Some(BlendMode::Add),
        args: Some(json!({})),
    };
    let created = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            payload.clone(),
            "Create replay-safe clip",
        )
        .await
        .unwrap();
    let created_id = created_clip_id(&created).to_owned();
    fixture
        .authored
        .update_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            UpdateTrackScoreInput {
                operation_id: "create-replay-followup".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: created_id.clone(),
                start_time: None,
                end_time: Some(2.5),
                z_index: None,
                blend_mode: None,
                args: None,
            },
            "Advance clip after creation",
        )
        .await
        .unwrap();

    let replayed = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            payload.clone(),
            "Retry after response loss",
        )
        .await
        .unwrap();
    assert_eq!(created_clip_id(&replayed), created_id);
    assert_eq!(
        replayed
            .edit
            .clips
            .iter()
            .find(|clip| clip.id == created_id)
            .unwrap()
            .end_time,
        2.5
    );
    assert_eq!(replayed.authored.commit_id, created.authored.commit_id);
    assert!(!replayed.authored.changed);
    let clip_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_scores WHERE score_id = ?")
            .bind(&scope.score_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(clip_count, 1);
    let association: (String, String) = sqlx::query_as(
        "SELECT subject_id, auxiliary_id FROM authored_state_creations
         WHERE principal_key = 'signed-out' AND creation_kind = 'track_clip' AND request_id = ?",
    )
    .bind(&payload.request_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(association, (created_id.clone(), scope.score_id.clone()));

    let mut rebound = payload.clone();
    rebound.end_time = 3.0;
    let error = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            rebound,
            "Illicit request rebinding",
        )
        .await
        .err()
        .expect("request rebinding must fail");
    assert!(matches!(error, AuthoredDocumentsError::Invalid(_)));

    fixture
        .authored
        .delete_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            DeleteTrackScoreInput {
                operation_id: "delete-created-after-replay".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: created_id.clone(),
            },
            "Delete created clip",
        )
        .await
        .unwrap();
    let stale = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope,
            payload.clone(),
            "Stale retry after deletion",
        )
        .await
        .expect("creation retry must survive a later deletion");
    assert_eq!(created_clip_id(&stale), created_id);
    assert!(!stale.edit.applied_to_current_projection);
    assert!(stale.edit.clips.is_empty());
    let surviving_association: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_state_creations
         WHERE principal_key = 'signed-out' AND creation_kind = 'track_clip' AND request_id = ?",
    )
    .bind(&payload.request_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(surviving_association, 1);
}
