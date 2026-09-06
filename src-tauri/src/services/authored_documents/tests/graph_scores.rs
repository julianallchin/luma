use super::*;
use crate::services::graph_scores::{self, GraphScoreDocument};

#[tokio::test]
async fn new_scores_start_as_graph_documents_and_creation_replay_preserves_edits() {
    let fixture = Fixture::new().await;
    let existing = fixture.track_scope().await;
    let request = Uuid::new_v4().to_string();
    let metadata = fixture
        .authored
        .create_score(
            &fixture.pool,
            &request,
            &existing.track_id,
            &existing.venue_id,
            Some("New score"),
        )
        .await
        .unwrap();
    let scope = TrackScope {
        score_id: metadata.id.clone(),
        ..existing
    };
    let empty = stored(&fixture, &scope).await.unwrap();
    assert_eq!(empty.score, luma_patterns::Score::default());
    let initial = current(&fixture, &scope).await;
    assert_eq!(
        initial.files[SCORE_PATH],
        empty.source().unwrap().as_bytes()
    );
    let candidate = chase();
    fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "edit-created-score",
            &candidate.source().unwrap(),
            &empty.revision,
            "Place Chase",
        )
        .await
        .unwrap();
    let replay = fixture
        .authored
        .create_score(
            &fixture.pool,
            &request,
            &scope.track_id,
            &scope.venue_id,
            Some("New score"),
        )
        .await
        .unwrap();
    assert_eq!(replay.id, metadata.id);
    assert_eq!(
        stored(&fixture, &scope).await.unwrap().revision,
        candidate.revision
    );
}

fn chase() -> GraphScoreDocument {
    let mut score = luma_patterns::Score::default();
    score
        .insert_effect(
            &luma_patterns::standard_library(),
            "chase",
            "chase-1",
            0.0,
            16.0,
        )
        .unwrap();
    let mut repeated = score.clips["chase-1"].clone();
    repeated.start = 32.0;
    repeated
        .inputs
        .insert("width".into(), luma_patterns::Value::Proportion(0.5));
    score.clips.insert("chase-2".into(), repeated);
    GraphScoreDocument::new(score).unwrap()
}

async fn legacy() -> (Fixture, AgentThread, TrackScope) {
    let fixture = Fixture::new().await;
    let scope = fixture.track_scope().await;
    sqlx::query("INSERT INTO track_scores(id,score_id,pattern_id,start_time,end_time,z_index,blend_mode,args_json) VALUES('0482d1cf-a7e3-4db1-9681-529aa1b87ab3','score','pattern',0,1,0,'replace','{}')")
        .execute(&fixture.pool).await.unwrap();
    let thread = fixture
        .authored
        .create_thread_with_authored_state(
            &fixture.pool,
            CreateAgentThreadInput {
                request_id: Uuid::new_v4().to_string(),
                agent_kind: "track_copilot".into(),
                subject_kind: Some("track".into()),
                subject_id: Some("track".into()),
                venue_id: Some("venue".into()),
                score_id: Some("score".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    (fixture, thread, scope)
}

async fn current(fixture: &Fixture, scope: &TrackScope) -> MainState {
    fixture
        .authored
        .load_current_locked(
            &fixture.pool,
            &ResolvedScope::track(None, scope.clone()).unwrap(),
        )
        .await
        .unwrap()
}

async fn stored(fixture: &Fixture, scope: &TrackScope) -> Option<GraphScoreDocument> {
    graph_scores::load(&mut fixture.pool.acquire().await.unwrap(), scope, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn graph_score_migration_edit_retry_and_restore_share_one_history() {
    let (fixture, thread, scope) = legacy().await;
    let initial = current(&fixture, &scope).await;
    let metadata: (i64, String) =
        sqlx::query_as("SELECT version, updated_at FROM scores WHERE id='score'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let document = chase();
    let source = document.source().unwrap();
    let migrate = || {
        fixture.authored.apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "migrate",
            &source,
            initial.document.revision(),
            "Migrate score",
        )
    };
    let applied = migrate().await.unwrap();
    assert!(applied.changed);
    assert_eq!(
        metadata,
        sqlx::query_as::<_, (i64, String)>(
            "SELECT version, updated_at FROM scores WHERE id='score'"
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    );
    let legacy_write = sqlx::query("INSERT INTO track_scores(id,score_id,pattern_id,start_time,end_time,args_json) VALUES('legacy','score','pattern',0,1,'{}')").execute(&fixture.pool).await;
    assert!(legacy_write
        .unwrap_err()
        .to_string()
        .contains("authored document"));
    assert!(!crate::sync::registry::TABLES
        .iter()
        .find(|table| table.name == "scores")
        .unwrap()
        .columns
        .contains(&"graph_document_json"));
    assert_eq!(
        stored(&fixture, &scope).await.unwrap().revision,
        document.revision
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );
    let mut access = crate::database::local::venue_access::VenueAccess::<
        crate::database::local::venue_access::Read,
    >::read(
        &fixture.pool,
        crate::database::local::venue_access::VenueResource::Score(&scope.score_id),
    )
    .await
    .unwrap();
    let scores =
        crate::database::local::scores::list_scores_for_track(&mut access, &scope.track_id)
            .await
            .unwrap();
    assert_eq!(scores[0].annotation_count, 2);
    let counts = crate::database::local::tracks::get_venue_annotation_counts(&mut access)
        .await
        .unwrap();
    assert_eq!(counts[&scope.track_id], 2);
    drop(access);
    assert_eq!(
        crate::database::local::scores::get_accessible_venue_for_track(
            &fixture.pool,
            &scope.track_id
        )
        .await
        .unwrap(),
        Some(scope.venue_id.clone())
    );
    let migrated = current(&fixture, &scope).await;
    assert_eq!(migrated.files.len(), 1);
    assert_eq!(migrated.files[SCORE_PATH], source.as_bytes());
    assert_eq!(migrate().await.unwrap().revision_id, applied.revision_id);

    // One edit changes a reusable graph and one of its placements together.
    let mut changed = document.score.clone();
    changed
        .definitions
        .get_mut("chase-1")
        .unwrap()
        .inputs
        .get_mut("width")
        .unwrap()
        .default = Some(luma_patterns::Value::Proportion(0.3));
    changed.clips.get_mut("chase-2").unwrap().start = 48.0;
    let changed = GraphScoreDocument::new(changed).unwrap();
    fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "edit",
            &changed.source().unwrap(),
            &document.revision,
            "Edit graph and placement",
        )
        .await
        .unwrap();
    let edited = current(&fixture, &scope).await;
    assert_eq!(edited.document.revision(), changed.revision);
    let stale = fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "stale",
            &source,
            &document.revision,
            "Stale edit",
        )
        .await;
    assert!(matches!(
        stale,
        Err(AuthoredDocumentsError::Track(
            TrackEditError::Conflict { .. }
        ))
    ));
    assert_eq!(current(&fixture, &scope).await.head, edited.head);

    fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            initial.head.as_str(),
            "restore-legacy",
            AuthoredRestoreMode::StateOnly,
        )
        .await
        .unwrap();
    assert!(stored(&fixture, &scope).await.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT id FROM track_scores")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        "0482d1cf-a7e3-4db1-9681-529aa1b87ab3"
    );
    fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            edited.head.as_str(),
            "restore-graphs",
            AuthoredRestoreMode::StateOnly,
        )
        .await
        .unwrap();
    assert_eq!(
        stored(&fixture, &scope).await.unwrap().revision,
        changed.revision
    );
}

#[tokio::test]
async fn graph_score_projection_rolls_back_with_failed_history_write() {
    let (fixture, _, scope) = legacy().await;
    let before = current(&fixture, &scope).await;
    sqlx::query("CREATE TRIGGER reject_test_score_edit BEFORE INSERT ON authored_operation_outcomes WHEN NEW.operation_id = 'fail-after-project' BEGIN SELECT RAISE(ABORT, 'injected history failure'); END")
        .execute(&fixture.pool).await.unwrap();
    let result = fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "fail-after-project",
            &chase().source().unwrap(),
            before.document.revision(),
            "Migrate",
        )
        .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("injected history failure"));
    assert!(stored(&fixture, &scope).await.is_none());
    assert_eq!(current(&fixture, &scope).await.head, before.head);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn graph_score_projection_rejects_wrong_scope_or_forged_revision() {
    let (fixture, _, scope) = legacy().await;
    let mut connection = fixture.pool.begin().await.unwrap();
    let mut wrong = scope.clone();
    wrong.track_id = "elsewhere".into();
    assert!(graph_scores::load(&mut connection, &wrong, None)
        .await
        .is_err());
    assert!(
        graph_scores::load(&mut connection, &scope, Some("another-owner"))
            .await
            .is_err()
    );
    let mut document = chase();
    document.revision = "forged".into();
    assert!(
        graph_scores::project(&mut connection, &scope, None, &document)
            .await
            .unwrap_err()
            .contains("revision")
    );
    connection.rollback().await.unwrap();
    assert!(stored(&fixture, &scope).await.is_none());
}

#[test]
fn graph_score_merge_preserves_independent_graph_and_clip_edits() {
    let base = chase();
    let mut ours = base.score.clone();
    ours.clips.get_mut("chase-2").unwrap().start = 64.0;
    let mut theirs = base.score.clone();
    theirs
        .definitions
        .get_mut("chase-1")
        .unwrap()
        .inputs
        .get_mut("width")
        .unwrap()
        .default = Some(luma_patterns::Value::Proportion(0.2));
    let ours = GraphScoreDocument::new(ours).unwrap();
    let theirs = GraphScoreDocument::new(theirs).unwrap();
    let merged = graph_scores::merge::strict(&base, &ours, &theirs).unwrap();
    assert_eq!(merged.score.clips["chase-2"].start, 64.0);
    assert_eq!(
        merged.score.definitions["chase-1"].inputs["width"].default,
        Some(luma_patterns::Value::Proportion(0.2))
    );
}

#[test]
fn graph_score_merge_reports_overlap_and_sync_keeps_unrelated_edits() {
    let base = chase();
    let mut ours = base.score.clone();
    ours.clips.get_mut("chase-1").unwrap().start = 8.0;
    ours.clips.get_mut("chase-2").unwrap().start = 48.0;
    let mut theirs = base.score.clone();
    theirs.clips.get_mut("chase-1").unwrap().start = 16.0;
    let ours = GraphScoreDocument::new(ours).unwrap();
    let theirs = GraphScoreDocument::new(theirs).unwrap();
    let conflicts = graph_scores::merge::strict(&base, &ours, &theirs).unwrap_err();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].kind, AuthoredMergeConflictKind::ConcurrentEdit);
    assert_eq!(
        conflicts[0].path.last(),
        Some(&AuthoredMergePathSegment::Field("start".into()))
    );
    let merged = graph_scores::merge::total(&base, &ours, &theirs);
    assert_eq!(merged.value.score.clips["chase-1"].start, 16.0);
    assert_eq!(merged.value.score.clips["chase-2"].start, 48.0);
}

#[test]
fn graph_score_merge_rejects_reference_input_coupling_and_dangling_definitions() {
    let mut score = chase().score;
    score
        .definitions
        .insert("other".into(), score.definitions["chase-1"].clone());
    let base = GraphScoreDocument::new(score).unwrap();
    let mut ours = base.score.clone();
    ours.clips.get_mut("chase-1").unwrap().graph = "other".into();
    let mut theirs = base.score.clone();
    theirs
        .clips
        .get_mut("chase-1")
        .unwrap()
        .inputs
        .insert("width".into(), luma_patterns::Value::Proportion(0.8));
    let ours = GraphScoreDocument::new(ours).unwrap();
    let theirs = GraphScoreDocument::new(theirs).unwrap();
    let conflicts = graph_scores::merge::strict(&base, &ours, &theirs).unwrap_err();
    assert_eq!(
        conflicts[0].kind,
        AuthoredMergeConflictKind::SemanticDependency
    );
    let mut removed = base.score.clone();
    removed.definitions.remove("other");
    let removed = GraphScoreDocument::new(removed).unwrap();
    let conflicts = graph_scores::merge::strict(&base, &ours, &removed).unwrap_err();
    assert_eq!(conflicts[0].kind, AuthoredMergeConflictKind::InvalidInput);
    assert!(conflicts[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("unknown clip graph"));
}

#[tokio::test]
async fn graph_score_workspace_commits_graphs_and_clips_then_merges_independent_live_edit() {
    let (fixture, thread, scope) = legacy().await;
    let initial = current(&fixture, &scope).await;
    let base = chase();
    fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "migrate",
            &base.source().unwrap(),
            initial.document.revision(),
            "Migrate",
        )
        .await
        .unwrap();
    let main = current(&fixture, &scope).await;
    let workspace = fixture
        .authored
        .create_workspace(
            &fixture.pool,
            None,
            CreateAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "work".into(),
                expected_base_revision_id: main.head.to_string(),
            },
        )
        .await
        .unwrap();
    let mut draft = base.score.clone();
    draft
        .insert_effect(
            &luma_patterns::standard_library(),
            "dissolve_flash",
            "flash",
            24.0,
            4.0,
        )
        .unwrap();
    let draft = GraphScoreDocument::new(draft).unwrap();
    let files = super::super::projection::score_files(&draft).unwrap();
    std::fs::write(
        std::path::Path::new(&workspace.path).join(SCORE_PATH),
        &files[SCORE_PATH],
    )
    .unwrap();
    let check = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &workspace.id)
        .await
        .unwrap();
    assert!(check.changed);
    assert_eq!(check.snapshot_id, file_snapshot_id(&files));
    let committed = fixture
        .authored
        .commit_workspace(
            &fixture.pool,
            None,
            CommitAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                workspace_id: workspace.id.clone(),
                expected_head_revision_id: workspace.head_revision_id,
                expected_snapshot_id: file_snapshot_id(&files),
                operation_id: "draft".into(),
                message: "Create dissolve".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        stored(&fixture, &scope).await.unwrap().revision,
        base.revision
    );
    let mut live = base.score.clone();
    live.clips.get_mut("chase-2").unwrap().start = 56.0;
    let live = GraphScoreDocument::new(live).unwrap();
    fixture
        .authored
        .apply_score_source_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            "live",
            &live.source().unwrap(),
            &base.revision,
            "Move clip",
        )
        .await
        .unwrap();
    let merged = fixture
        .authored
        .merge_workspace(
            &fixture.pool,
            None,
            MergeAuthoredWorkspaceInput {
                thread_id: thread.id,
                workspace_id: workspace.id,
                expected_head_revision_id: committed.revision_id,
                operation_id: "merge".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(merged, AuthoredWorkspaceMerge::Merged { .. }));
    let result = stored(&fixture, &scope).await.unwrap();
    assert!(result.score.definitions.contains_key("flash"));
    assert!(result.score.clips.contains_key("flash"));
    assert_eq!(result.score.clips["chase-2"].start, 56.0);
}

#[tokio::test]
async fn graph_score_initial_server_head_materializes_only_from_revision_files() {
    let owner = "graph-score-owner";
    let fixture = Fixture::signed_in(owner).await;
    let track = fixture.track_scope().await;
    let scope = ResolvedScope::track(Some(owner), track.clone()).unwrap();
    let score = chase();
    let mut transaction = fixture.pool.begin().await.unwrap();
    fixture
        .authored
        .store
        .insert_document(&mut transaction, &scope.specification().unwrap())
        .await
        .unwrap();
    let revision = fixture
        .authored
        .store
        .insert_revision(
            &mut transaction,
            &scope.document_id,
            &[],
            &super::super::projection::score_files(&score).unwrap(),
            &revision_metadata("initial_import", None, "Score from another device").unwrap(),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    fixture
        .authored
        .apply_server_head(
            &fixture.pool,
            owner,
            scope.document_id.as_str(),
            revision.id.as_str(),
            1,
            "2026-09-06T01:02:03Z",
        )
        .await
        .unwrap();
    let stored = graph_scores::load(
        &mut fixture.pool.acquire().await.unwrap(),
        &track,
        Some(owner),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.revision, score.revision);
    assert_eq!(stored.score.clips.len(), 2);
    assert_eq!(stored.score.definitions.len(), 1);
}
