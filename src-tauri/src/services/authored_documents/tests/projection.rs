use super::super::*;
use super::support::*;

#[tokio::test]
async fn prepared_score_commit_cannot_advance_main_after_principal_switch() {
    let fixture = Fixture::new().await;
    let track_scope = fixture.add_track_scope().await;
    crate::database::local::auth::arm_write_admission(&fixture.pool, None)
        .await
        .unwrap();
    let scope = ResolvedScope::track(None, track_scope).unwrap();
    let _guard = fixture
        .authored
        .repository_guard(&scope.repository_id)
        .await;
    let main = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap();
    let prepared = fixture
        .authored
        .store
        .prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&main.head),
            &main.files,
            &system_author().unwrap(),
            "Prepared before identity switch",
        )
        .unwrap();

    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    let expected_revision = main.document.revision().to_owned();
    let error = fixture
        .authored
        .project_prepared(
            &fixture.pool,
            &scope,
            &main.head,
            ProjectionLedgerExpectation::PresentAt(&main.head),
            &prepared,
            main.document,
            &expected_revision,
            TrackProjectionAuthority::ExistingOnly,
            ProjectionMetadata::default(),
        )
        .await
        .err()
        .expect("stale prepared projection must be rejected");
    assert!(matches!(error, AuthoredDocumentsError::Scope(_)));
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        main.head
    );
    let ledger = load_ledger(&fixture.pool, &scope).await.unwrap().unwrap();
    assert_eq!(ledger.projected_commit, main.head);
}

#[tokio::test]
async fn prepared_noop_graph_cannot_advance_main_after_principal_switch() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    crate::database::local::auth::arm_write_admission(&fixture.pool, None)
        .await
        .unwrap();
    let scope = ResolvedScope::pattern(None, "pattern", &implementation_id("pattern")).unwrap();
    let _guard = fixture
        .authored
        .repository_guard(&scope.repository_id)
        .await;
    let main = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap();
    let prepared = fixture
        .authored
        .store
        .prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&main.head),
            &main.files,
            &system_author().unwrap(),
            "Prepared no-op before identity switch",
        )
        .unwrap();
    let graph_before: String =
        sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = ?")
            .bind(implementation_id("pattern"))
            .fetch_one(&fixture.pool)
            .await
            .unwrap();

    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    let expected_revision = main.document.revision().to_owned();
    let error = fixture
        .authored
        .project_prepared(
            &fixture.pool,
            &scope,
            &main.head,
            ProjectionLedgerExpectation::PresentAt(&main.head),
            &prepared,
            main.document,
            &expected_revision,
            TrackProjectionAuthority::ExistingOnly,
            ProjectionMetadata::default(),
        )
        .await
        .err()
        .expect("stale no-op graph projection must be rejected");
    assert!(matches!(error, AuthoredDocumentsError::Scope(_)));
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        main.head
    );
    let ledger = load_ledger(&fixture.pool, &scope).await.unwrap().unwrap();
    assert_eq!(ledger.projected_commit, main.head);
    let graph_after: String =
        sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = ?")
            .bind(implementation_id("pattern"))
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(graph_after, graph_before);
}

#[tokio::test]
async fn initial_track_import_uses_valid_interface_despite_stale_graph_internals() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("legacy-pattern").await;
    let scope = fixture.add_track_scope().await;
    let selection_default = PatternArgDef {
        id: "selection".into(),
        name: "selection".into(),
        arg_type: PatternArgType::Selection,
        default_value: json!({
            "expression": "all",
            "spatialReference": "global"
        }),
    };
    let graph = graph_with_stale_arg_edge(vec![selection_default, scalar_arg("gain", "gain", 1.0)]);
    sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = ?")
        .bind(serde_json::to_string(&graph).unwrap())
        .bind(implementation_id("legacy-pattern"))
        .execute(&fixture.pool)
        .await
        .unwrap();

    let args = json!({
        "selection": {
            "expression": "front & left",
            "spatialReference": "group_local"
        },
        "gain": 0.625,
        "orphaned_arg": {"nested": [true, null, "preserved"]}
    });
    sqlx::query(
        "INSERT INTO track_scores
         (id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json)
         VALUES ('legacy-clip', NULL, 'score', 'legacy-pattern', 0.25, 1.75, -2, 'add', ?)",
    )
    .bind(args.to_string())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let graph_error = load_graph_document_unscoped(
        &fixture.pool,
        "legacy-pattern",
        &implementation_id("legacy-pattern"),
    )
    .await
    .unwrap_err();
    assert!(graph_error.to_string().contains("removed_arg"));
    let before = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();

    let thread = fixture
        .authored
        .create_thread_with_authored_state(
            &fixture.pool,
            CreateAgentThreadInput {
                request_id: uuid::Uuid::new_v4().to_string(),
                agent_kind: "track_copilot".into(),
                subject_kind: Some("track".into()),
                subject_id: Some(scope.track_id.clone()),
                venue_id: Some(scope.venue_id.clone()),
                score_id: Some(scope.score_id.clone()),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("valid pattern interface must permit initial Git import");

    let resolved = ResolvedScope::from_thread(&thread, None).unwrap();
    let head = fixture
        .authored
        .store
        .main_head(&resolved.repository_id)
        .unwrap();
    let (_, files) = fixture
        .authored
        .store
        .read_commit(&resolved.repository_id, &head)
        .unwrap();
    let source = std::str::from_utf8(files.get(SCORE_PATH).unwrap()).unwrap();
    let context = load_score_dsl_context(&fixture.pool, &scope).await.unwrap();
    let imported = compile_import_track_document(source, &context, false).unwrap();
    assert_eq!(imported.document, before);
    assert_eq!(imported.document.clips[0].args, args);

    let history = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, None)
        .await
        .unwrap();
    assert!(history
        .entries
        .iter()
        .any(|entry| entry.kind == AuthoredOperationKind::InitialImport));
}

#[tokio::test]
async fn historical_track_restore_is_exact_after_grid_name_and_interface_change() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("wash").await;
    let scope = fixture.add_track_scope().await;
    install_test_beat_grid(&fixture, &[0.0, 4.0, 8.0, 12.0], 4).await;
    let graph = graph_with_args(vec![
        selection_arg("selection"),
        scalar_arg("gain", "gain", 1.0),
    ]);
    sqlx::query("UPDATE implementations SET graph_json = ? WHERE pattern_id = 'wash'")
        .bind(serde_json::to_string(&graph).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let thread = fixture.track_thread(&scope).await;
    let original_args = json!({
        "selection": {"expression": "front & left", "spatialReference": "group_local"},
        "gain": 0.625,
        "huge": 9_007_199_254_740_993_u64,
        "payload": {"nested": [true, null, "preserved"]}
    });
    let created = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            CreateTrackScoreInput {
                request_id: "00000000-0000-4000-8000-000000000001".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                pattern_id: "wash".into(),
                start_time: 1.0,
                end_time: 4.0,
                z_index: -6,
                blend_mode: Some(BlendMode::Add),
                args: Some(original_args.clone()),
            },
            "Create historical clip",
        )
        .await
        .unwrap();
    let target_commit = created.authored.commit_id.clone();
    let resolved = ResolvedScope::from_thread(&thread, None).unwrap();
    let (_, target_files) = fixture
        .authored
        .store
        .read_commit(
            &resolved.repository_id,
            &CommitId::parse(&target_commit).unwrap(),
        )
        .unwrap();
    let target_source = utf8_file(&target_files, SCORE_PATH).unwrap();
    assert!(target_source.contains("@1s-4s"), "{target_source}");
    assert!(target_source.contains("selection={"), "{target_source}");

    fixture
        .authored
        .update_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            UpdateTrackScoreInput {
                operation_id: "restore-update".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                id: created_clip_id(&created).to_owned(),
                start_time: Some(6.0),
                end_time: Some(7.0),
                z_index: Some(2),
                blend_mode: Some(BlendMode::Replace),
                args: Some(json!({"new": "state"})),
            },
            "Advance beyond historical clip",
        )
        .await
        .unwrap();
    invalidate_pattern_context(&fixture, "wash").await;

    fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            &target_commit,
            "restore-context-free-score",
        )
        .await
        .expect("restore must not consult current beat or pattern context");
    let restored = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();
    assert_eq!(restored.clips.len(), 1);
    let clip = &restored.clips[0];
    assert_eq!(clip.id, created_clip_id(&created));
    assert_eq!(clip.start_time.to_bits(), 1.0_f64.to_bits());
    assert_eq!(clip.end_time.to_bits(), 4.0_f64.to_bits());
    assert_eq!(clip.z_index, -6);
    assert_eq!(clip.blend_mode, BlendMode::Add);
    assert_eq!(clip.args, original_args);
}

#[tokio::test]
async fn present_relational_graph_shadow_write_fails_closed_without_a_git_commit() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let original = graph_with_args(vec![scalar_arg("gain", "gain", 0.5)]);
    sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = ?")
        .bind(exact_graph_json(&original).unwrap())
        .bind(implementation_id("pattern"))
        .execute(&fixture.pool)
        .await
        .unwrap();
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let head = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();

    let mut shadow = original;
    shadow.nodes[0].position_x = Some(42.0);
    sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = ?")
        .bind(exact_graph_json(&shadow).unwrap())
        .bind(implementation_id("pattern"))
        .execute(&fixture.pool)
        .await
        .unwrap();

    let error = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, None)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("relational authored projection diverged from Git main"));
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        head
    );
    let stored: String = sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = ?")
        .bind(implementation_id("pattern"))
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        graph_revision(&serde_json::from_str::<Graph>(&stored).unwrap()).unwrap(),
        graph_revision(&shadow).unwrap()
    );
}

#[tokio::test]
async fn absent_graph_projection_rejects_layout_and_owner_collisions() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let original = graph_with_args(vec![scalar_arg("gain", "gain", 0.5)]);
    sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = ?")
        .bind(exact_graph_json(&original).unwrap())
        .bind(implementation_id("pattern"))
        .execute(&fixture.pool)
        .await
        .unwrap();
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let head = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    sqlx::query(
        "UPDATE authored_state_projections
         SET materialization_state = 'absent' WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let mut layout_collision = original.clone();
    layout_collision.nodes[0].position_y = Some(99.0);
    sqlx::query("UPDATE implementations SET graph_json = ? WHERE id = ?")
        .bind(exact_graph_json(&layout_collision).unwrap())
        .bind(implementation_id("pattern"))
        .execute(&fixture.pool)
        .await
        .unwrap();
    let layout_error = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, None)
        .await
        .unwrap_err();
    assert!(layout_error
        .to_string()
        .contains("cannot materialize absent Git graph over a divergent relational"));

    // Model a database created before immutable implementation identity was
    // installed. Ordinary and remote writes cannot manufacture this state.
    sqlx::query("DROP TRIGGER authored_implementation_identity_immutable")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let mut corruption = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    crate::database::local::write_admission::enter_maintenance_writes(&mut corruption, None)
        .await
        .unwrap();
    sqlx::query("UPDATE implementations SET uid = 'mallory', graph_json = ? WHERE id = ?")
        .bind(exact_graph_json(&original).unwrap())
        .bind(implementation_id("pattern"))
        .execute(&mut *corruption)
        .await
        .unwrap();
    crate::database::local::write_admission::leave_maintenance_writes(&mut corruption, None)
        .await
        .unwrap();
    corruption.commit().await.unwrap();
    let owner_error = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, None)
        .await
        .unwrap_err();
    assert!(owner_error
        .to_string()
        .contains("colliding implementation does not belong to the current principal"));
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        head
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT materialization_state FROM authored_state_projections
             WHERE repository_id = ?",
        )
        .bind(scope.repository_id.as_str())
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        "absent"
    );
}

#[tokio::test]
async fn absent_track_projection_rejects_a_relational_clip_collision() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("wash").await;
    let track_scope = fixture.add_track_scope().await;
    let thread = fixture
        .authored
        .create_thread_with_authored_state(
            &fixture.pool,
            CreateAgentThreadInput {
                request_id: uuid::Uuid::new_v4().to_string(),
                agent_kind: "track_copilot".into(),
                subject_kind: Some("track".into()),
                subject_id: Some(track_scope.track_id.clone()),
                venue_id: Some(track_scope.venue_id.clone()),
                score_id: Some(track_scope.score_id.clone()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let head = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    sqlx::query(
        "UPDATE authored_state_projections
         SET materialization_state = 'absent' WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_scores
         (id, score_id, pattern_id, start_time, end_time, args_json)
         VALUES ('shadow-clip', 'score', 'wash', 1.0, 2.0, '{}')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    let error = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, None)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("cannot materialize absent Git score over a non-empty relational"));
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&scope.repository_id)
            .unwrap(),
        head
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores WHERE id = 'shadow-clip'",)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT materialization_state FROM authored_state_projections
             WHERE repository_id = ?",
        )
        .bind(scope.repository_id.as_str())
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        "absent"
    );
}

#[tokio::test]
async fn legacy_unmaterialized_graph_thread_remains_deletable_without_catalog_rows() {
    let fixture = Fixture::new().await;
    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER agent_threads_validate_authored_route_insert")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agent_threads
         (id, owner_user_id, agent_kind, subject_kind, subject_id, implementation_id)
         VALUES ('legacy-thread', 'alice', 'pattern_graph', 'pattern', 'missing-pattern',
                 'legacy-unmaterialized-6d697373696e672d7061747465726e')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO agent_thread_messages (id, thread_id, seq, role, parts_json)
         VALUES ('legacy-message', 'legacy-thread', 0, 'user', '[]')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    fixture
        .authored
        .delete_thread_with_authored_state(
            &fixture.pool,
            Some("alice"),
            "legacy-thread",
            || async { Ok(()) },
        )
        .await
        .unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_threads WHERE id = 'legacy-thread'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_thread_messages WHERE id = 'legacy-message'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn projection_ledger_compare_and_swap_rolls_back_a_stale_writer() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let main = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap();
    let author = system_author().unwrap();
    let first = fixture
        .authored
        .store
        .prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&main.head),
            &main.files,
            &author,
            "first sibling",
        )
        .unwrap();
    let second = fixture
        .authored
        .store
        .prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&main.head),
            &main.files,
            &author,
            "second sibling",
        )
        .unwrap();

    let mut winner = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    write_ledger(
        &mut winner,
        &scope,
        ProjectionLedgerExpectation::PresentAt(&main.head),
        &first.id,
    )
    .await
    .unwrap();
    winner.commit().await.unwrap();

    let mut stale = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query("UPDATE patterns SET name = 'must roll back' WHERE id = 'pattern'")
        .execute(&mut *stale)
        .await
        .unwrap();
    let error = write_ledger(
        &mut stale,
        &scope,
        ProjectionLedgerExpectation::PresentAt(&main.head),
        &second.id,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("projection ledger moved"));
    stale.rollback().await.unwrap();

    let name: String = sqlx::query_scalar("SELECT name FROM patterns WHERE id = 'pattern'")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(name, "pattern");
    let ledger = load_ledger(&fixture.pool, &scope).await.unwrap().unwrap();
    assert_eq!(ledger.projected_commit, first.id);
}

#[tokio::test]
async fn reconciliation_fails_closed_when_git_main_is_ahead_of_projection() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let main = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap();
    let forged = fixture
        .authored
        .store
        .prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&main.head),
            &main.files,
            &system_author().unwrap(),
            "forged external main advance",
        )
        .unwrap();
    fixture
        .authored
        .store
        .advance_branch(&scope.repository_id, MAIN_BRANCH, &main.head, &forged.id)
        .unwrap();

    let error = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .err()
        .expect("external Git main advance must fail closed");
    assert!(error
        .to_string()
        .contains("refusing to project an unauthenticated ref mutation"));
    let ledger = load_ledger(&fixture.pool, &scope).await.unwrap().unwrap();
    assert_eq!(ledger.projected_commit, main.head);
}

#[tokio::test]
async fn startup_reconciliation_does_not_import_member_visible_foreign_documents() {
    let fixture = Fixture::new().await;
    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("bob"))
        .await
        .unwrap();
    let mut transaction = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    crate::database::local::write_admission::enter_remote_writes(&mut transaction)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, file_path, origin)
         VALUES ('alice-track', 'alice', 'alice-hash', 'alice-hash.stub', 'remote')",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO venues (id, uid, name, role, origin)
         VALUES ('alice-venue', 'alice', 'Alice Venue', 'member', 'remote')",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO venue_memberships (venue_id, user_id, role)
         VALUES ('alice-venue', 'bob', 'member')",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name, origin)
         VALUES ('alice-score', 'alice', 'alice-track', 'alice-venue',
                 'Alice Score', 'remote')",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO patterns (id, uid, name, is_verified, origin)
         VALUES ('alice-pattern', 'alice', 'Alice Pattern', 1, 'remote')",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, graph_json, origin)
         VALUES ('alice-implementation', 'alice', 'alice-pattern', ?, 'remote')",
    )
    .bind(exact_graph_json(&empty_graph()).unwrap())
    .execute(&mut *transaction)
    .await
    .unwrap();
    crate::database::local::write_admission::leave_remote_writes(&mut transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    fixture
        .authored
        .reconcile_available_projections(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_state_projections
             WHERE score_id = 'alice-score' OR subject_id = 'alice-pattern'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0,
        "read visibility must never create owner Git history for another principal"
    );
}
