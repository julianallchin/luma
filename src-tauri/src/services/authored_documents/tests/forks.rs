use super::super::edits::{load_pattern_fork_source, pattern_fork_target_id};
use super::super::*;
use super::support::*;

#[tokio::test]
async fn implementations_of_one_pattern_have_independent_repositories_and_projections() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
         VALUES ('implementation-club', NULL, 'pattern', 'club', ?)",
    )
    .bind(serde_json::to_string(&empty_graph()).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let default_thread = fixture.pattern_thread("pattern").await;
    let club_thread = fixture
        .pattern_thread_for("pattern", "implementation-club")
        .await;
    let default_scope = ResolvedScope::from_thread(&default_thread, None).unwrap();
    let club_scope = ResolvedScope::from_thread(&club_thread, None).unwrap();
    assert_ne!(default_scope.repository_id, club_scope.repository_id);

    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            "implementation-club",
            "edit-club-implementation",
            graph_with_args(vec![scalar_arg("gain", "gain", 0.5)]),
            &graph_revision(&empty_graph()).unwrap(),
            "Edit club implementation",
        )
        .await
        .unwrap();

    let default = load_graph_document(
        &fixture.pool,
        &GraphScope {
            pattern_id: "pattern".into(),
            implementation_id: implementation_id("pattern"),
            owner_user_id: None,
        },
    )
    .await
    .unwrap();
    let club = load_graph_document(
        &fixture.pool,
        &GraphScope {
            pattern_id: "pattern".into(),
            implementation_id: "implementation-club".into(),
            owner_user_id: None,
        },
    )
    .await
    .unwrap();
    assert!(default.graph.args.is_empty());
    assert_eq!(club.graph.args[0].id, "gain");

    let graph_ledgers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_state_projections
         WHERE document_kind = 'pattern_graph' AND subject_id = 'pattern'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(graph_ledgers, 2);
}

#[tokio::test]
async fn pattern_fork_is_exactly_replayable_and_implementation_specific() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("source").await;
    let selected_graph = graph_with_args(vec![scalar_arg("gain", "gain", 0.75)]);
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
         VALUES ('source-selected', NULL, 'source', 'selected', ?)",
    )
    .bind(exact_graph_json(&selected_graph).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let input = ForkPatternInput {
        source_pattern_id: "source".into(),
        source_implementation_id: "source-selected".into(),
        request_id: "fork-request".into(),
    };
    let first = fixture
        .authored
        .fork_pattern(&fixture.pool, None, input.clone())
        .await
        .expect("fork selected implementation");
    assert_eq!(first.pattern.name, "source_fork");
    assert_eq!(first.pattern.forked_from_id.as_deref(), Some("source"));
    assert_eq!(
        first.pattern.id,
        pattern_fork_target_id("signed-out", "fork-request", "pattern")
    );
    assert_eq!(
        first.implementation_id,
        pattern_fork_target_id("signed-out", "fork-request", "implementation")
    );
    assert!(first.applied_to_current_projection);
    let forked_graph = crate::services::graph_documents::load_graph_document_unscoped(
        &fixture.pool,
        &first.pattern.id,
        &first.implementation_id,
    )
    .await
    .unwrap();
    assert_eq!(
        forked_graph.revision,
        graph_revision(&selected_graph).unwrap()
    );

    fixture
        .authored
        .archive_pattern(&fixture.pool, None, "source")
        .await
        .unwrap();

    let replay = fixture
        .authored
        .fork_pattern(&fixture.pool, None, input.clone())
        .await
        .expect("exact response-loss replay");
    assert_eq!(replay.pattern.id, first.pattern.id);
    assert_eq!(replay.implementation_id, first.implementation_id);
    assert_eq!(replay.repository_id, first.repository_id);
    assert_eq!(replay.commit_id, first.commit_id);

    let mismatch = fixture
        .authored
        .fork_pattern(
            &fixture.pool,
            None,
            ForkPatternInput {
                source_implementation_id: implementation_id("source"),
                ..input
            },
        )
        .await
        .unwrap_err();
    assert!(mismatch
        .to_string()
        .contains("already bound to another source implementation"));
    let fork_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM patterns WHERE forked_from_id = 'source'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(fork_count, 1);
    let implementation_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM implementations WHERE pattern_id = ?")
            .bind(&first.pattern.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(implementation_count, 1);
}

#[tokio::test]
async fn pattern_fork_recovers_when_sql_commits_before_main_cas() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("source").await;
    let input = ForkPatternInput {
        source_pattern_id: "source".into(),
        source_implementation_id: implementation_id("source"),
        request_id: "fork-crash".into(),
    };
    let principal_key = principal_key(None);
    let target_pattern_id = pattern_fork_target_id(&principal_key, &input.request_id, "pattern");
    let target_implementation_id =
        pattern_fork_target_id(&principal_key, &input.request_id, "implementation");
    let target_scope =
        ResolvedScope::pattern(None, &target_pattern_id, &target_implementation_id).unwrap();
    let source = load_pattern_fork_source(
        &fixture.pool,
        &input.source_pattern_id,
        &input.source_implementation_id,
    )
    .await
    .unwrap();
    let repository = fixture
        .authored
        .store
        .ensure_repository(&target_scope.repository_id)
        .unwrap();
    let request_fingerprint = operation_request_fingerprint(
        "pattern_fork",
        &[
            &principal_key,
            &input.source_pattern_id,
            &input.source_implementation_id,
        ],
    );
    let prepared = fixture
        .authored
        .store
        .prepare_commit(
            &target_scope.repository_id,
            std::slice::from_ref(&repository.main_head),
            &graph_files(&source.graph.graph).unwrap(),
            &system_author().unwrap(),
            &commit_message(
                "Fork pattern graph",
                &[
                    (TRAILER_OPERATION, "pattern_fork"),
                    (TRAILER_OPERATION_ID, &input.request_id),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    assert!(fixture
        .authored
        .project_pattern_fork_sqlite(
            &fixture.pool,
            None,
            &target_scope,
            &input,
            &source,
            &repository.main_head,
            &prepared,
            &request_fingerprint,
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&target_scope.repository_id)
            .unwrap(),
        repository.main_head
    );
    assert_eq!(
        load_ledger(&fixture.pool, &target_scope)
            .await
            .unwrap()
            .unwrap()
            .projected_commit,
        prepared.id
    );

    let recovered = fixture
        .authored
        .fork_pattern(&fixture.pool, None, input)
        .await
        .expect("recover ledger-ahead fork");
    assert_eq!(recovered.commit_id, prepared.id.as_str());
    assert!(recovered.applied_to_current_projection);
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&target_scope.repository_id)
            .unwrap(),
        prepared.id
    );
}
