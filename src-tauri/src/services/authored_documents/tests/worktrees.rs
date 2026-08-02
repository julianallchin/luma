use super::super::*;
use super::support::*;

fn write_worktree_files(path: &str, files: &FileMap) {
    for (name, contents) in files {
        let destination = Path::new(path).join(name);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(destination, contents).unwrap();
    }
}

fn prettify_worktree_graph(path: &str) {
    let graph_path = Path::new(path).join(GRAPH_PATH);
    let graph: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&graph_path).unwrap()).unwrap();
    std::fs::write(graph_path, serde_json::to_vec_pretty(&graph).unwrap()).unwrap();
}

#[tokio::test]
async fn historical_track_merge_is_exact_after_grid_name_and_interface_change() {
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
    let resolved = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&resolved.repository_id)
        .unwrap();
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "historical-score-child".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &worktree.path,
        &FileMap::from([(
            SCORE_PATH.to_owned(),
            concat!(
                "# child authored against the original musical context\n",
                "layer -4:\n",
                "wash[\"wash\"](front & left) @1:2-2:1 ",
                "gain=0.625 payload={\"marker\":\"child\",\"nested\":[true,null]}"
            )
            .as_bytes()
            .to_vec(),
        )]),
    );
    let check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .unwrap();
    let child = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: worktree.id.clone(),
                expected_head_commit_id: check.head_commit_id,
                expected_snapshot_id: check.snapshot_id,
                operation_id: "commit-historical-score-child".into(),
                message: "Commit historical score child".into(),
            },
        )
        .await
        .unwrap();
    fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            None,
            scope.clone(),
            CreateTrackScoreInput {
                request_id: "00000000-0000-4000-8000-000000000002".into(),
                score_id: scope.score_id.clone(),
                track_id: scope.track_id.clone(),
                pattern_id: "wash".into(),
                start_time: 8.125,
                end_time: 9.875,
                z_index: 3,
                blend_mode: Some(BlendMode::Multiply),
                args: Some(json!({"marker": "main", "amount": 0.30000000000000004})),
            },
            "Advance main independently",
        )
        .await
        .unwrap();
    invalidate_pattern_context(&fixture, "wash").await;

    let merged = fixture
        .authored
        .merge_worktree(
            &fixture.pool,
            None,
            MergeAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: worktree.id,
                expected_head_commit_id: child.commit_id,
                operation_id: "merge-context-free-score".into(),
            },
        )
        .await
        .expect("merge must decode every tree without current musical context");
    assert!(matches!(merged, AuthoredWorktreeMerge::Merged { .. }));

    let document = load_track_document_for_principal(&fixture.pool, &scope, None)
        .await
        .unwrap();
    assert_eq!(document.clips.len(), 2);
    let child_clip = document
        .clips
        .iter()
        .find(|clip| clip.args["payload"]["marker"] == "child")
        .unwrap();
    assert_eq!(child_clip.start_time.to_bits(), 1.0_f64.to_bits());
    assert_eq!(child_clip.end_time.to_bits(), 4.0_f64.to_bits());
    assert_eq!(child_clip.z_index, -4);
    assert_eq!(child_clip.args["gain"], json!(0.625));
    assert_eq!(
        child_clip.args["selection"],
        json!({"expression": "front & left", "spatialReference": "global"})
    );
    let main_clip = document
        .clips
        .iter()
        .find(|clip| clip.args["marker"] == "main")
        .unwrap();
    assert_eq!(main_clip.start_time.to_bits(), 8.125_f64.to_bits());
    assert_eq!(main_clip.end_time.to_bits(), 9.875_f64.to_bits());
    assert_eq!(main_clip.blend_mode, BlendMode::Multiply);

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
    let source = utf8_file(&files, SCORE_PATH).unwrap();
    assert!(source.contains("wash[\"wash\"]"), "{source}");
    assert!(!source.contains("renamed_after_commit"), "{source}");
    assert!(source.contains("@1s-4s"), "{source}");
}

#[tokio::test]
async fn worktree_requests_bind_to_an_explicit_historical_main_base() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let chosen_base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .expect("initial main head");

    let advanced = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "advance-main-after-orchestration",
            graph_with_args(vec![scalar_arg("gain", "gain", 0.5)]),
            &graph_revision(&empty_graph()).unwrap(),
            "Advance main after orchestration",
        )
        .await
        .expect("advance main");
    assert_ne!(advanced.commit_id, chosen_base.as_str());

    let first_request = CreateAuthoredWorktreeInput {
        thread_id: thread.id.clone(),
        request_id: "child-one".into(),
        expected_base_commit_id: chosen_base.to_string(),
    };
    let second_request = CreateAuthoredWorktreeInput {
        thread_id: thread.id.clone(),
        request_id: "child-two".into(),
        expected_base_commit_id: chosen_base.to_string(),
    };
    let (first, second) = tokio::join!(
        fixture
            .authored
            .create_worktree(&fixture.pool, None, first_request.clone()),
        fixture
            .authored
            .create_worktree(&fixture.pool, None, second_request),
    );
    let first = first.expect("first child worktree");
    let second = second.expect("second child worktree");
    assert_ne!(first.id, second.id);
    for worktree in [&first, &second] {
        assert_eq!(worktree.base_commit_id, chosen_base.as_str());
        assert_eq!(worktree.head_commit_id, chosen_base.as_str());
    }

    let replay = fixture
        .authored
        .create_worktree(&fixture.pool, None, first_request)
        .await
        .expect("exact replay");
    assert_eq!(replay.id, first.id);
    assert_eq!(replay.base_commit_id, chosen_base.as_str());

    let mismatch = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "child-one".into(),
                expected_base_commit_id: advanced.commit_id,
            },
        )
        .await
        .unwrap_err();
    assert!(mismatch
        .to_string()
        .contains("already bound to a different base"));

    let reservation: (String, String) = sqlx::query_as(
        "SELECT base_commit, request_fingerprint
         FROM authored_state_worktrees
         WHERE owner_thread_id = ? AND request_id = 'child-one'",
    )
    .bind(&thread.id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(reservation.0, chosen_base.as_str());
    assert_eq!(
        reservation.1,
        operation_request_fingerprint(
            "worktree_create",
            &[
                scope.repository_id.as_str(),
                &thread.id,
                chosen_base.as_str(),
            ],
        )
    );
}

#[tokio::test]
async fn worktree_creation_rejects_a_commit_outside_main_history() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let prepared = prepare(
        &fixture,
        &thread,
        "unpublished-child",
        graph_with_args(vec![scalar_arg("gain", "gain", 0.5)]),
    )
    .await;

    let error = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "invalid-base".into(),
                expected_base_commit_id: prepared.branch_commit_id,
            },
        )
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("not in this document's main history"));
    let reservations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_state_worktrees
         WHERE owner_thread_id = ? AND request_id = 'invalid-base'",
    )
    .bind(&thread.id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(reservations, 0);
}

#[tokio::test]
async fn worktree_commit_replays_after_branch_advance_and_binds_the_exact_snapshot() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap()
        .head;
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "commit-child".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();

    write_worktree_files(
        &worktree.path,
        &graph_files(&graph_with_args(vec![scalar_arg("one", "one", 0.1)])).unwrap(),
    );
    prettify_worktree_graph(&worktree.path);
    let first_check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .unwrap();
    let first_input = CommitAuthoredWorktreeInput {
        thread_id: thread.id.clone(),
        worktree_id: worktree.id.clone(),
        expected_head_commit_id: first_check.head_commit_id,
        expected_snapshot_id: first_check.snapshot_id,
        operation_id: "commit-one".into(),
        message: "Commit child one".into(),
    };
    let first = fixture
        .authored
        .commit_worktree(&fixture.pool, None, first_input.clone())
        .await
        .unwrap();
    assert!(first.applied_to_current_worktree);
    let canonicalized_check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .unwrap();
    assert!(!canonicalized_check.changed);
    assert_eq!(canonicalized_check.head_commit_id, first.commit_id);
    let exact_replay = fixture
        .authored
        .commit_worktree(&fixture.pool, None, first_input.clone())
        .await
        .unwrap();
    assert_eq!(exact_replay.commit_id, first.commit_id);

    let mut rebound = first_input.clone();
    rebound.message = "Different request".into();
    let error = fixture
        .authored
        .commit_worktree(&fixture.pool, None, rebound)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("already bound to another request"));

    write_worktree_files(
        &worktree.path,
        &graph_files(&graph_with_args(vec![scalar_arg("two", "two", 0.2)])).unwrap(),
    );
    let second_check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .unwrap();
    fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: worktree.id.clone(),
                expected_head_commit_id: second_check.head_commit_id,
                expected_snapshot_id: second_check.snapshot_id,
                operation_id: "commit-two".into(),
                message: "Commit child two".into(),
            },
        )
        .await
        .unwrap();
    let late_replay = fixture
        .authored
        .commit_worktree(&fixture.pool, None, first_input)
        .await
        .unwrap();
    assert_eq!(late_replay.commit_id, first.commit_id);
    assert!(!late_replay.applied_to_current_worktree);
}

#[tokio::test]
async fn worktree_check_is_structural_but_commit_rejects_an_unsupported_node() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap()
        .head;
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "unsupported-node".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    let unsupported = Graph {
        nodes: vec![NodeInstance {
            id: "legacy".into(),
            type_id: "retired_node_type".into(),
            params: HashMap::new(),
            position_x: Some(1.0),
            position_y: Some(2.0),
        }],
        edges: Vec::new(),
        args: Vec::new(),
    };
    write_worktree_files(&worktree.path, &graph_files(&unsupported).unwrap());
    let checked = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .expect("historical codec must decode without the live catalog");
    assert!(checked.changed);
    assert!(matches!(
        &checked.document,
        AuthoredProjectedDocument::PatternGraph { graph, .. }
            if graph.nodes[0].type_id == "retired_node_type"
    ));

    let error = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: worktree.id.clone(),
                expected_head_commit_id: checked.head_commit_id,
                expected_snapshot_id: checked.snapshot_id,
                operation_id: "commit-unsupported-node".into(),
                message: "Must reject unsupported node".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unknown node type"), "{error}");
    assert_eq!(
        fixture
            .authored
            .store
            .branch_head(&scope.repository_id, &worktree.branch)
            .unwrap(),
        base
    );
}

#[tokio::test]
async fn prettified_graph_worktree_retires_only_at_its_bound_snapshot() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap()
        .head;

    let clean = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "graph-clean-removal".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &clean.path,
        &graph_files(&graph_with_args(vec![scalar_arg("gain", "gain", 0.5)])).unwrap(),
    );
    prettify_worktree_graph(&clean.path);
    let raw_graph = std::fs::read(Path::new(&clean.path).join(GRAPH_PATH)).unwrap();
    let before = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &clean.id)
        .await
        .unwrap();
    let committed = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: clean.id.clone(),
                expected_head_commit_id: before.head_commit_id,
                expected_snapshot_id: before.snapshot_id.clone(),
                operation_id: "commit-graph-clean-removal".into(),
                message: "Commit prettified graph".into(),
            },
        )
        .await
        .unwrap();
    let (_, canonical_files) = fixture
        .authored
        .store
        .read_commit(
            &scope.repository_id,
            &CommitId::parse(&committed.commit_id).unwrap(),
        )
        .unwrap();
    assert_ne!(raw_graph, canonical_files[GRAPH_PATH]);
    let after = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &clean.id)
        .await
        .unwrap();
    assert!(!after.changed);
    assert_ne!(after.snapshot_id, before.snapshot_id);
    assert_eq!(
        after.snapshot_id,
        file_snapshot_id(&canonical_files).unwrap()
    );
    fixture
        .authored
        .remove_worktree(&fixture.pool, None, &thread.id, &clean.id)
        .await
        .expect("bound prettified graph is clean");
    assert!(!Path::new(&clean.path).exists());

    let mutated = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "graph-mutated-removal".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &mutated.path,
        &graph_files(&graph_with_args(vec![scalar_arg("gain", "gain", 0.75)])).unwrap(),
    );
    prettify_worktree_graph(&mutated.path);
    let before = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &mutated.id)
        .await
        .unwrap();
    fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: mutated.id.clone(),
                expected_head_commit_id: before.head_commit_id,
                expected_snapshot_id: before.snapshot_id,
                operation_id: "commit-graph-mutated-removal".into(),
                message: "Commit second prettified graph".into(),
            },
        )
        .await
        .unwrap();
    let graph_path = Path::new(&mutated.path).join(GRAPH_PATH);
    let mut changed_bytes = std::fs::read(&graph_path).unwrap();
    changed_bytes.push(b'\n');
    std::fs::write(&graph_path, changed_bytes).unwrap();
    let changed = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &mutated.id)
        .await
        .unwrap();
    assert!(changed.changed);
    let error = fixture
        .authored
        .remove_worktree(&fixture.pool, None, &thread.id, &mutated.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted or untracked"));
    assert!(Path::new(&mutated.path).exists());
}

#[tokio::test]
async fn canonicalized_score_worktree_retires_only_at_its_bound_snapshot() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("wash").await;
    let track_scope = fixture.add_track_scope().await;
    let thread = fixture.track_thread(&track_scope).await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap()
        .head;
    let raw_score = concat!(
        "# source intentionally omits the host-owned clip id\n",
        "wash[\"wash\"](all) @1s-4s"
    )
    .as_bytes()
    .to_vec();

    let clean = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "score-clean-removal".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &clean.path,
        &FileMap::from([(SCORE_PATH.to_owned(), raw_score.clone())]),
    );
    let before = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &clean.id)
        .await
        .unwrap();
    let committed = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: clean.id.clone(),
                expected_head_commit_id: before.head_commit_id,
                expected_snapshot_id: before.snapshot_id.clone(),
                operation_id: "commit-score-clean-removal".into(),
                message: "Commit score requiring canonical ids".into(),
            },
        )
        .await
        .unwrap();
    let (_, canonical_files) = fixture
        .authored
        .store
        .read_commit(
            &scope.repository_id,
            &CommitId::parse(&committed.commit_id).unwrap(),
        )
        .unwrap();
    assert_ne!(raw_score, canonical_files[SCORE_PATH]);
    let after = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &clean.id)
        .await
        .unwrap();
    assert!(!after.changed);
    assert_ne!(after.snapshot_id, before.snapshot_id);
    assert_eq!(
        after.snapshot_id,
        file_snapshot_id(&canonical_files).unwrap()
    );
    fixture
        .authored
        .remove_worktree(&fixture.pool, None, &thread.id, &clean.id)
        .await
        .expect("bound pre-canonical score is clean");
    assert!(!Path::new(&clean.path).exists());

    let mutated = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "score-mutated-removal".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &mutated.path,
        &FileMap::from([(SCORE_PATH.to_owned(), raw_score)]),
    );
    let before = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &mutated.id)
        .await
        .unwrap();
    fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: mutated.id.clone(),
                expected_head_commit_id: before.head_commit_id,
                expected_snapshot_id: before.snapshot_id,
                operation_id: "commit-score-mutated-removal".into(),
                message: "Commit second canonicalized score".into(),
            },
        )
        .await
        .unwrap();
    let score_path = Path::new(&mutated.path).join(SCORE_PATH);
    let mut changed_bytes = std::fs::read(&score_path).unwrap();
    changed_bytes.extend_from_slice(b"\n# edited after commit\n");
    std::fs::write(&score_path, changed_bytes).unwrap();
    let changed = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &mutated.id)
        .await
        .unwrap();
    assert!(changed.changed);
    let error = fixture
        .authored
        .remove_worktree(&fixture.pool, None, &thread.id, &mutated.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted or untracked"));
    assert!(Path::new(&mutated.path).exists());
}

#[tokio::test]
async fn worktree_merge_replays_after_main_advance_and_persists_conflicts() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base_state = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap();
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "merge-child".into(),
                expected_base_commit_id: base_state.head.to_string(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &worktree.path,
        &graph_files(&graph_with_args(vec![scalar_arg("child", "child", 0.1)])).unwrap(),
    );
    prettify_worktree_graph(&worktree.path);
    let check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .unwrap();
    let child_commit = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: worktree.id.clone(),
                expected_head_commit_id: check.head_commit_id,
                expected_snapshot_id: check.snapshot_id,
                operation_id: "prepare-merge".into(),
                message: "Prepare merge child".into(),
            },
        )
        .await
        .unwrap();
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "advance-main-independently",
            graph_with_args(vec![scalar_arg("main", "main", 0.2)]),
            base_state.document.revision(),
            "Advance main independently",
        )
        .await
        .unwrap();
    let merge_input = MergeAuthoredWorktreeInput {
        thread_id: thread.id.clone(),
        worktree_id: worktree.id.clone(),
        expected_head_commit_id: child_commit.commit_id.clone(),
        operation_id: "merge-one".into(),
    };
    let merged = fixture
        .authored
        .merge_worktree(&fixture.pool, None, merge_input.clone())
        .await
        .unwrap();
    let merge_commit = match merged {
        AuthoredWorktreeMerge::Merged {
            commit_id,
            document: AuthoredProjectedDocument::PatternGraph { graph, .. },
            ..
        } => {
            assert_eq!(graph.args.len(), 2);
            commit_id
        }
        other => panic!("expected merged worktree, got {other:?}"),
    };
    let current_revision =
        load_graph_document_unscoped(&fixture.pool, "pattern", &implementation_id("pattern"))
            .await
            .unwrap()
            .revision;
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "advance-after-merge",
            graph_with_args(vec![scalar_arg("later", "later", 0.3)]),
            &current_revision,
            "Advance after merge response",
        )
        .await
        .unwrap();
    let replay = fixture
        .authored
        .merge_worktree(&fixture.pool, None, merge_input.clone())
        .await
        .unwrap();
    match replay {
        AuthoredWorktreeMerge::Merged {
            commit_id,
            applied_to_current_projection,
            ..
        } => {
            assert_eq!(commit_id, merge_commit);
            assert!(!applied_to_current_projection);
        }
        other => panic!("expected merge replay, got {other:?}"),
    }
    let mut rebound = merge_input;
    rebound.expected_head_commit_id = base_state.head.to_string();
    let error = fixture
        .authored
        .merge_worktree(&fixture.pool, None, rebound)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("already bound to another source"));

    let conflict_base_graph = graph_with_args(vec![scalar_arg("shared", "shared", 0.0)]);
    let current =
        load_graph_document_unscoped(&fixture.pool, "pattern", &implementation_id("pattern"))
            .await
            .unwrap();
    let conflict_base = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "conflict-base",
            conflict_base_graph,
            &current.revision,
            "Conflict base",
        )
        .await
        .unwrap();
    let conflict_child = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "conflict-child".into(),
                expected_base_commit_id: conflict_base.commit_id.clone(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &conflict_child.path,
        &graph_files(&graph_with_args(vec![scalar_arg("shared", "shared", 1.0)])).unwrap(),
    );
    let conflict_check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &conflict_child.id)
        .await
        .unwrap();
    let conflict_head = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: conflict_child.id.clone(),
                expected_head_commit_id: conflict_check.head_commit_id,
                expected_snapshot_id: conflict_check.snapshot_id,
                operation_id: "prepare-conflict".into(),
                message: "Prepare conflict child".into(),
            },
        )
        .await
        .unwrap();
    let base_revision = match conflict_base.document {
        AuthoredProjectedDocument::PatternGraph { revision, .. } => revision,
        _ => unreachable!(),
    };
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "conflicting-main-edit",
            graph_with_args(vec![scalar_arg("shared", "shared", 2.0)]),
            &base_revision,
            "Conflicting main edit",
        )
        .await
        .unwrap();
    let conflict_input = MergeAuthoredWorktreeInput {
        thread_id: thread.id.clone(),
        worktree_id: conflict_child.id,
        expected_head_commit_id: conflict_head.commit_id,
        operation_id: "merge-conflict".into(),
    };
    let first_conflict = fixture
        .authored
        .merge_worktree(&fixture.pool, None, conflict_input.clone())
        .await
        .unwrap();
    let replayed_conflict = fixture
        .authored
        .merge_worktree(&fixture.pool, None, conflict_input)
        .await
        .unwrap();
    match (first_conflict, replayed_conflict) {
        (
            AuthoredWorktreeMerge::Conflicted { conflicts: first },
            AuthoredWorktreeMerge::Conflicted { conflicts: replay },
        ) => {
            assert!(!first.is_empty());
            assert_eq!(
                serde_json::to_value(first).unwrap(),
                serde_json::to_value(replay).unwrap()
            );
        }
        other => panic!("expected durable conflict replay, got {other:?}"),
    }
}

#[tokio::test]
async fn worktree_merge_validation_failure_is_a_terminal_replayable_conflict() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let base_graph = two_math_nodes(Vec::new());
    let base = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "install-worktree-cycle-base",
            base_graph.clone(),
            &graph_revision(&empty_graph()).unwrap(),
            "Install worktree cycle base",
        )
        .await
        .expect("install valid base graph");
    let base_revision = match &base.document {
        AuthoredProjectedDocument::PatternGraph { revision, .. } => revision.clone(),
        AuthoredProjectedDocument::TrackScore { .. } => unreachable!(),
    };
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base_commit = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "cycle-worktree".into(),
                expected_base_commit_id: base_commit.to_string(),
            },
        )
        .await
        .unwrap();

    let mut child_graph = base_graph.clone();
    child_graph.edges.push(signal_edge("a", "b"));
    write_worktree_files(&worktree.path, &graph_files(&child_graph).unwrap());
    let check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .unwrap();
    let child = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: worktree.id.clone(),
                expected_head_commit_id: check.head_commit_id,
                expected_snapshot_id: check.snapshot_id,
                operation_id: "commit-cycle-child".into(),
                message: "Commit one valid edge".into(),
            },
        )
        .await
        .unwrap();

    let mut main_graph = base_graph;
    main_graph.edges.push(signal_edge("b", "a"));
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "install-worktree-opposite-edge",
            main_graph,
            &base_revision,
            "Install opposite worktree edge",
        )
        .await
        .expect("main side remains independently valid");

    let input = MergeAuthoredWorktreeInput {
        thread_id: thread.id,
        worktree_id: worktree.id,
        expected_head_commit_id: child.commit_id,
        operation_id: "merge-cycle-conflict".into(),
    };
    let first = fixture
        .authored
        .merge_worktree(&fixture.pool, None, input.clone())
        .await
        .expect("combined cycle becomes a conflict");
    let replay = fixture
        .authored
        .merge_worktree(&fixture.pool, None, input)
        .await
        .expect("worktree validation conflict replay");
    let (first, replay) = match (first, replay) {
        (
            AuthoredWorktreeMerge::Conflicted { conflicts: first },
            AuthoredWorktreeMerge::Conflicted { conflicts: replay },
        ) => (first, replay),
        other => panic!("expected durable worktree conflicts, got {other:?}"),
    };
    assert!(first.iter().any(|conflict| {
        conflict.kind == AuthoredMergeConflictKind::InvalidInput
            && conflict
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("cycle"))
    }));
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM authored_state_operations
         WHERE operation_kind = 'worktree_merge' AND operation_id = 'merge-cycle-conflict'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(status, "conflicted");
}

#[tokio::test]
async fn thread_deletion_archives_dirty_child_worktree_before_pruning_it() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "dirty-child".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    std::fs::write(
        Path::new(&worktree.path).join("unfinished-notes.txt"),
        b"preserve me",
    )
    .unwrap();

    fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async { Ok(()) })
        .await
        .unwrap();

    assert!(!Path::new(&worktree.path).exists());
    let archived_head = fixture
        .authored
        .store
        .branch_head(&scope.repository_id, &worktree.branch)
        .unwrap();
    assert_ne!(archived_head, base);
    let (_, files) = fixture
        .authored
        .store
        .read_commit(&scope.repository_id, &archived_head)
        .unwrap();
    assert_eq!(
        files.get("unfinished-notes.txt").map(Vec::as_slice),
        Some(b"preserve me".as_slice())
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM authored_state_worktrees WHERE worktree_id = ?")
            .bind(&worktree.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(status, "retired");
}

#[tokio::test]
async fn thread_deletion_removes_safe_unregistered_partial_worktree_before_retiring_it() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-interrupted-delete").unwrap();
    let branch = format!("agents/worktrees/{}/{}", thread.id, worktree_id.as_str());
    fixture
        .authored
        .store
        .create_branch(&scope.repository_id, &branch, &base)
        .unwrap();
    sqlx::query(
        "INSERT INTO authored_state_worktrees
         (worktree_id, request_id, request_fingerprint, repository_id,
          owner_thread_id, branch_name, base_commit, status)
         VALUES (?, 'interrupted-delete', 'interrupted-delete-fingerprint', ?, ?, ?, ?, 'preparing')",
    )
    .bind(worktree_id.as_str())
    .bind(scope.repository_id.as_str())
    .bind(&thread.id)
    .bind(&branch)
    .bind(base.as_str())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let storage = StorageRoot::from_path(fixture._directory.path().join("storage"));
    let path = storage.authored_worktree_dir(scope.repository_id.as_str(), worktree_id.as_str());
    std::fs::create_dir_all(&path).unwrap();
    let (_, files) = fixture
        .authored
        .store
        .read_commit(&scope.repository_id, &base)
        .unwrap();
    write_worktree_files(path.to_str().unwrap(), &files);

    fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async { Ok(()) })
        .await
        .unwrap();

    assert!(!path.exists());
    let status: String =
        sqlx::query_scalar("SELECT status FROM authored_state_worktrees WHERE worktree_id = ?")
            .bind(worktree_id.as_str())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(status, "retired");
}

#[tokio::test]
async fn thread_deletion_keeps_unsafe_unregistered_partial_worktree_retryable() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-unsafe-interrupted-delete").unwrap();
    let branch = format!("agents/worktrees/{}/{}", thread.id, worktree_id.as_str());
    fixture
        .authored
        .store
        .create_branch(&scope.repository_id, &branch, &base)
        .unwrap();
    sqlx::query(
        "INSERT INTO authored_state_worktrees
         (worktree_id, request_id, request_fingerprint, repository_id,
          owner_thread_id, branch_name, base_commit, status)
         VALUES (?, 'unsafe-interrupted-delete', 'unsafe-interrupted-delete-fingerprint', ?, ?, ?, ?, 'preparing')",
    )
    .bind(worktree_id.as_str())
    .bind(scope.repository_id.as_str())
    .bind(&thread.id)
    .bind(&branch)
    .bind(base.as_str())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let storage = StorageRoot::from_path(fixture._directory.path().join("storage"));
    let path = storage.authored_worktree_dir(scope.repository_id.as_str(), worktree_id.as_str());
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("unknown-user-data"), b"preserve me").unwrap();

    let error = fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async { Ok(()) })
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("refusing to overwrite non-checkout data"));
    assert_eq!(
        std::fs::read(path.join("unknown-user-data")).unwrap(),
        b"preserve me"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM authored_state_worktrees WHERE worktree_id = ?")
            .bind(worktree_id.as_str())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(status, "preparing");
    let lifecycle: String =
        sqlx::query_scalar("SELECT lifecycle_state FROM agent_threads WHERE id = ?")
            .bind(&thread.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(lifecycle, "deleting");
}

#[tokio::test]
async fn thread_deletion_prunes_bound_canonicalized_worktree_without_archiving_it() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .unwrap();
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "canonicalized-child".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap();
    write_worktree_files(
        &worktree.path,
        &graph_files(&graph_with_args(vec![scalar_arg("gain", "gain", 0.5)])).unwrap(),
    );
    prettify_worktree_graph(&worktree.path);
    let check = fixture
        .authored
        .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .unwrap();
    let committed = fixture
        .authored
        .commit_worktree(
            &fixture.pool,
            None,
            CommitAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                worktree_id: worktree.id.clone(),
                expected_head_commit_id: check.head_commit_id,
                expected_snapshot_id: check.snapshot_id,
                operation_id: "commit-canonicalized-child".into(),
                message: "Commit canonicalized child".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        !fixture
            .authored
            .check_worktree(&fixture.pool, None, &thread.id, &worktree.id)
            .await
            .unwrap()
            .changed
    );

    fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async { Ok(()) })
        .await
        .unwrap();

    assert!(!Path::new(&worktree.path).exists());
    assert_eq!(
        fixture
            .authored
            .store
            .branch_head(&scope.repository_id, &worktree.branch)
            .unwrap()
            .as_str(),
        committed.commit_id
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM authored_state_worktrees WHERE worktree_id = ?")
            .bind(&worktree.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(status, "retired");
}

#[tokio::test]
async fn worktree_creation_that_owns_repository_gate_finishes_before_deletion() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .expect("main head");
    let repository_lock = {
        let locks = fixture.authored.repository_locks.lock().await;
        std::sync::Arc::clone(
            locks
                .get(&scope.repository_id)
                .expect("thread creation installs repository lock"),
        )
    };

    // Hold the SQLite writer so create_worktree pauses at its durable
    // reservation while still owning the repository gate.
    let database_blocker = fixture
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("database blocker");
    let create_authored = fixture.authored.clone();
    let create_pool = fixture.pool.clone();
    let create_thread_id = thread.id.clone();
    let create_base = base.to_string();
    let creation = tokio::spawn(async move {
        create_authored
            .create_worktree(
                &create_pool,
                None,
                CreateAuthoredWorktreeInput {
                    thread_id: create_thread_id,
                    request_id: "operation-wins-race".into(),
                    expected_base_commit_id: create_base,
                },
            )
            .await
    });
    loop {
        if repository_lock.try_lock().is_err() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let delete_authored = fixture.authored.clone();
    let delete_pool = fixture.pool.clone();
    let delete_thread_id = thread.id.clone();
    let deletion = tokio::spawn(async move {
        delete_authored
            .delete_thread_with_authored_state(&delete_pool, None, &delete_thread_id, || async {
                Ok(())
            })
            .await
    });
    tokio::task::yield_now().await;
    database_blocker.commit().await.expect("release database");

    let worktree = creation
        .await
        .expect("creation task")
        .expect("creation wins repository ordering");
    deletion
        .await
        .expect("deletion task")
        .expect("deletion follows creation");
    assert!(!Path::new(&worktree.path).exists());
    let status: String =
        sqlx::query_scalar("SELECT status FROM authored_state_worktrees WHERE worktree_id = ?")
            .bind(&worktree.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(status, "retired");
}
