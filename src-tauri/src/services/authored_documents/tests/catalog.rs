use super::super::*;
use super::support::*;

#[tokio::test]
async fn score_archive_is_terminal_idempotent_and_retains_git_history() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', NULL, 'venue')")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tracks
         (id, uid, track_hash, title, duration_seconds, file_path)
         VALUES ('track', NULL, 'hash', 'track', 120.0, '/tmp/track.wav')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let score = fixture
        .authored
        .create_score(
            &fixture.pool,
            "00000000-0000-4000-8000-000000000001",
            "track",
            "venue",
            Some("score"),
        )
        .await
        .unwrap();
    let scope = ResolvedScope::track(
        None,
        TrackScope {
            score_id: score.id.clone(),
            track_id: "track".into(),
            venue_id: "venue".into(),
        },
    )
    .unwrap();
    let clip_payload = CreateTrackScoreInput {
        request_id: "00000000-0000-4000-8000-000000000002".into(),
        score_id: score.id.clone(),
        track_id: "track".into(),
        pattern_id: "pattern".into(),
        start_time: 1.0,
        end_time: 2.0,
        z_index: 3,
        blend_mode: Some(BlendMode::Add),
        args: Some(json!({})),
    };
    let clip = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.track_scope().unwrap().clone(),
            clip_payload.clone(),
            "Create clip before score archive",
        )
        .await
        .unwrap();
    let clip_id = created_clip_id(&clip).to_owned();
    let head = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let (_, files_before) = fixture
        .authored
        .store
        .read_commit(&scope.repository_id, &head)
        .unwrap();

    fixture
        .authored
        .archive_score(&fixture.pool, None, &score.id)
        .await
        .unwrap();
    fixture
        .authored
        .archive_score(&fixture.pool, None, &score.id)
        .await
        .expect("a lost successful archive response must be replayable");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = ?")
            .bind(&score.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores WHERE score_id = ?")
            .bind(&score.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );
    let ledger: (String, String) = sqlx::query_as(
        "SELECT projected_commit, materialization_state
         FROM authored_state_projections WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(ledger, (head.to_string(), "archived".into()));
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        head
    );
    assert_eq!(
        fixture
            .authored
            .store
            .read_commit(&scope.repository_id, &head)
            .unwrap()
            .1,
        files_before
    );
    let retained = fixture
        .authored
        .snapshot_from_commit(&scope, &head)
        .unwrap();
    let AuthoredDocument::Track(retained) = retained.document else {
        panic!("score archive retained a graph document")
    };
    assert_eq!(retained.clips.len(), 1);
    assert_eq!(retained.clips[0].id, clip_id);
    let score_creation: (String, String) = sqlx::query_as(
        "SELECT subject_id, commit_id FROM authored_state_creations
         WHERE principal_key = 'signed-out' AND creation_kind = 'score'
           AND request_id = '00000000-0000-4000-8000-000000000001'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(score_creation.0, score.id);
    assert!(fixture
        .authored
        .store
        .is_ancestor(
            &scope.repository_id,
            &CommitId::parse(&score_creation.1).unwrap(),
            &head,
        )
        .unwrap());
    let clip_creation: (String, Option<String>, String) = sqlx::query_as(
        "SELECT subject_id, auxiliary_id, commit_id FROM authored_state_creations
         WHERE principal_key = 'signed-out' AND creation_kind = 'track_clip'
           AND request_id = '00000000-0000-4000-8000-000000000002'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(clip_creation.0, clip_id);
    assert_eq!(clip_creation.1.as_deref(), Some(score.id.as_str()));
    assert!(fixture
        .authored
        .store
        .is_ancestor(
            &scope.repository_id,
            &CommitId::parse(&clip_creation.2).unwrap(),
            &head,
        )
        .unwrap());

    fixture
        .authored
        .archive_score(&fixture.pool, None, &score.id)
        .await
        .expect("terminal archive retry");
    let stale_clip = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.track_scope().unwrap().clone(),
            clip_payload,
            "Retry clip creation after score archive",
        )
        .await
        .err()
        .expect("archived score must reject a stale clip creation retry");
    assert!(stale_clip.to_string().contains("was archived"));
    let stale_create = fixture
        .authored
        .create_score(
            &fixture.pool,
            "00000000-0000-4000-8000-000000000001",
            "track",
            "venue",
            Some("score"),
        )
        .await
        .unwrap_err();
    assert!(stale_create.to_string().contains("was archived"));
    let stale_catalog = sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES (?, NULL, 'track', 'venue', 'stale')",
    )
    .bind(&score.id)
    .execute(&fixture.pool)
    .await
    .unwrap_err();
    assert!(stale_catalog
        .to_string()
        .contains("cannot recreate an archived authored score"));
}

#[tokio::test]
async fn score_archive_refuses_durable_threads_without_partial_archival() {
    let fixture = Fixture::new().await;
    let track_scope = fixture.add_track_scope().await;
    fixture.track_thread(&track_scope).await;
    let scope = ResolvedScope::track(None, track_scope).unwrap();
    let head = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let thread_error = fixture
        .authored
        .archive_score(&fixture.pool, None, "score")
        .await
        .unwrap_err();
    assert!(thread_error.to_string().contains("durable conversations"));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT materialization_state FROM authored_state_projections
             WHERE document_kind = 'track_score' AND score_id = 'score'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        "present"
    );
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        head
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = 'score'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_threads WHERE score_id = 'score'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn pattern_archive_is_atomic_across_implementations_and_terminal() {
    let fixture = Fixture::new().await;
    let pattern = fixture
        .authored
        .create_pattern(
            &fixture.pool,
            None,
            "00000000-0000-4000-8000-000000000010",
            "pattern".into(),
            None,
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
         VALUES ('second-implementation', NULL, ?, 'second', ?)",
    )
    .bind(&pattern.id)
    .bind(exact_graph_json(&graph_with_args(vec![scalar_arg("gain", "gain", 0.5)])).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture
        .authored
        .reconcile_pattern_graphs_for_read(&fixture.pool, &pattern.id)
        .await
        .unwrap();
    let scopes = pattern_projection_scopes(&fixture.pool, &pattern.id)
        .await
        .unwrap();
    assert_eq!(scopes.len(), 2);
    let heads: Vec<_> = scopes
        .iter()
        .map(|scope| {
            (
                scope.repository_id.clone(),
                fixture
                    .authored
                    .store
                    .main_head(&scope.repository_id)
                    .unwrap(),
            )
        })
        .collect();

    fixture
        .authored
        .archive_pattern(&fixture.pool, None, &pattern.id)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns WHERE id = ?")
            .bind(&pattern.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM implementations WHERE pattern_id = ?")
            .bind(&pattern.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT materialization_state FROM authored_state_projections
         WHERE document_kind = 'pattern_graph' AND subject_id = ? ORDER BY repository_id",
    )
    .bind(&pattern.id)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(states, vec!["archived", "archived"]);
    for (repository_id, head) in heads {
        assert_eq!(
            fixture.authored.store.main_head(&repository_id).unwrap(),
            head
        );
        fixture
            .authored
            .store
            .read_commit(&repository_id, &head)
            .unwrap();
    }

    fixture
        .authored
        .archive_pattern(&fixture.pool, None, &pattern.id)
        .await
        .expect("terminal archive retry");
    let stale_create = fixture
        .authored
        .create_pattern(
            &fixture.pool,
            None,
            "00000000-0000-4000-8000-000000000010",
            "pattern".into(),
            None,
        )
        .await
        .unwrap_err();
    assert!(stale_create.to_string().contains("was archived"));
    let stale_catalog =
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES (?, NULL, 'stale')")
            .bind(&pattern.id)
            .execute(&fixture.pool)
            .await
            .unwrap_err();
    assert!(stale_catalog
        .to_string()
        .contains("cannot recreate an archived authored pattern"));
}

#[tokio::test]
async fn venue_override_remains_local_routing_while_each_graph_is_git_authored() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', NULL, 'venue')")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
         VALUES ('venue-implementation', NULL, 'pattern', 'venue', ?)",
    )
    .bind(exact_graph_json(&graph_with_args(vec![scalar_arg("gain", "gain", 0.5)])).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO venue_implementation_overrides
         (venue_id, pattern_id, implementation_id, uid)
         VALUES ('venue', 'pattern', 'venue-implementation', NULL)",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    fixture
        .authored
        .reconcile_available_projections(&fixture.pool)
        .await
        .unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_state_projections
             WHERE document_kind = 'pattern_graph' AND subject_id = 'pattern'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        2,
        "both implementation documents keep independent Git histories",
    );
    assert_eq!(
        crate::services::graph_documents::resolve_graph_implementation(
            &fixture.pool,
            "pattern",
            Some("venue"),
            None,
        )
        .await
        .unwrap(),
        "venue-implementation",
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT implementation_id FROM venue_implementation_overrides
             WHERE venue_id = 'venue' AND pattern_id = 'pattern'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        "venue-implementation",
    );
}

#[tokio::test]
async fn pattern_archive_refuses_score_and_thread_dependents_without_partial_archival() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    fixture.add_track_scope().await;
    fixture
        .authored
        .reconcile_pattern_graphs_for_read(&fixture.pool, "pattern")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track_scores
         (id, score_id, pattern_id, start_time, end_time, z_index, args_json)
         VALUES ('clip', 'score', 'pattern', 0.0, 1.0, 0, '{}')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let clip_error = fixture
        .authored
        .archive_pattern(&fixture.pool, None, "pattern")
        .await
        .unwrap_err();
    assert!(clip_error.to_string().contains("authored score clips"));

    sqlx::query("DELETE FROM track_scores WHERE id = 'clip'")
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.pattern_thread("pattern").await;
    let thread_error = fixture
        .authored
        .archive_pattern(&fixture.pool, None, "pattern")
        .await
        .unwrap_err();
    assert!(thread_error.to_string().contains("durable conversations"));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT materialization_state FROM authored_state_projections
             WHERE document_kind = 'pattern_graph' AND subject_id = 'pattern'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        "present"
    );
}

#[tokio::test]
async fn catalog_only_pattern_delete_has_no_fake_git_document_but_graphs_require_archival() {
    let fixture = Fixture::new().await;
    sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('catalog-only', NULL, 'catalog')")
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture
        .authored
        .archive_pattern(&fixture.pool, None, "catalog-only")
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_state_projections
             WHERE document_kind = 'pattern_graph' AND subject_id = 'catalog-only'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );

    sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('raw-graph', NULL, 'raw')")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, graph_json)
         VALUES ('raw-implementation', NULL, 'raw-graph', ?)",
    )
    .bind(exact_graph_json(&empty_graph()).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let bypass = sqlx::query("DELETE FROM patterns WHERE id = 'raw-graph'")
        .execute(&fixture.pool)
        .await
        .unwrap_err();
    assert!(bypass
        .to_string()
        .contains("pattern deletion requires archived authored projections"));
}

#[tokio::test]
async fn legacy_score_archive_backfills_git_before_catalog_deletion() {
    let fixture = Fixture::new().await;
    let track_scope = fixture.add_track_scope().await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_state_projections
             WHERE document_kind = 'track_score' AND score_id = 'score'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
    fixture
        .authored
        .archive_score(&fixture.pool, None, "score")
        .await
        .unwrap();
    let scope = ResolvedScope::track(None, track_scope).unwrap();
    let ledger: (String, String) = sqlx::query_as(
        "SELECT projected_commit, materialization_state
         FROM authored_state_projections WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(ledger.1, "archived");
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap()
            .to_string(),
        ledger.0
    );
}

#[tokio::test]
async fn remote_archive_requires_the_exact_principal_and_terminalizes_absent_git_state() {
    let fixture = Fixture::new().await;
    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'venue')")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tracks
         (id, uid, track_hash, title, duration_seconds, file_path)
         VALUES ('track', 'alice', 'hash', 'track', 120.0, '/tmp/track.wav')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let score = fixture
        .authored
        .create_score(
            &fixture.pool,
            "00000000-0000-4000-8000-000000000011",
            "track",
            "venue",
            Some("score"),
        )
        .await
        .unwrap();
    let scope = ResolvedScope::track(
        Some("alice"),
        TrackScope {
            score_id: score.id.clone(),
            track_id: "track".into(),
            venue_id: "venue".into(),
        },
    )
    .unwrap();
    let head = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let wrong_principal = fixture
        .authored
        .archive_score_from_remote(&fixture.pool, "bob", &score.id)
        .await
        .unwrap_err();
    assert!(wrong_principal.to_string().contains("does not exist"));

    let mut signout = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query(
        "UPDATE auth_write_admission
         SET accepting = 0, maintenance = 1, remote_writes = 0
         WHERE singleton = 1",
    )
    .execute(&mut *signout)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE authored_state_projections SET materialization_state = 'absent'
         WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .execute(&mut *signout)
    .await
    .unwrap();
    sqlx::query("DELETE FROM scores WHERE id = ?")
        .bind(&score.id)
        .execute(&mut *signout)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE auth_write_admission SET accepting = 1, maintenance = 0
         WHERE singleton = 1",
    )
    .execute(&mut *signout)
    .await
    .unwrap();
    signout.commit().await.unwrap();

    let wrong_absent_principal = fixture
        .authored
        .archive_score_from_remote(&fixture.pool, "bob", &score.id)
        .await
        .unwrap_err();
    assert!(wrong_absent_principal
        .to_string()
        .contains("does not exist"));
    assert!(fixture
        .authored
        .archive_score_from_remote(&fixture.pool, "alice", &score.id)
        .await
        .unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT materialization_state FROM authored_state_projections
             WHERE repository_id = ?",
        )
        .bind(scope.repository_id.as_str())
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        "archived"
    );
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        head
    );
    assert!(!fixture
        .authored
        .archive_score_from_remote(&fixture.pool, "alice", &score.id)
        .await
        .unwrap());
}
