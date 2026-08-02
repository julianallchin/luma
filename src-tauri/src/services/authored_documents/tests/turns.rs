use super::super::*;
use super::support::*;
use crate::models::agent_threads::{AppendAgentThreadMessagesInput, NewAgentThreadMessage};

#[tokio::test]
async fn unreserved_assistant_append_is_rejected_before_git_changes() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let branch = format!("agents/threads/{}", thread.id);
    let before = fixture
        .authored
        .store
        .branch_head(&scope.repository_id, &branch)
        .unwrap();
    let error = agent_threads::append_messages(
        &fixture.pool,
        &thread.id,
        AppendAgentThreadMessagesInput {
            operation_id: "append-unreserved-assistant".into(),
            messages: vec![NewAgentThreadMessage {
                id: Some("unreserved-assistant".into()),
                role: "assistant".into(),
                parts: serde_json::json!([]),
            }],
        },
        None,
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("assistant message requires a prepared authored turn"),
        "{error}"
    );
    assert!(
        agent_threads::list_messages(&fixture.pool, &thread.id, None)
            .await
            .unwrap()
            .is_empty(),
        "the rejected assistant response must not enter the transcript"
    );
    assert_eq!(
        fixture
            .authored
            .store
            .branch_head(&scope.repository_id, &branch)
            .unwrap(),
        before,
        "a rejected unreserved response must not change authored history"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_state_turn_commits
             WHERE assistant_message_id = 'unreserved-assistant'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn recovers_one_prepared_turn_once_and_keeps_thread_and_worktree_refs_disjoint() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let prepared = prepare(
        &fixture,
        &thread,
        "message-one",
        graph_with_args(vec![scalar_arg("gain", "gain", 0.5)]),
    )
    .await;
    fixture.append_assistant(&thread.id, "message-one").await;

    let recovered = fixture
        .authored
        .recover_turns(&fixture.pool, None, &thread.id)
        .await
        .expect("recover prepared turn");
    assert_eq!(recovered.len(), 1);
    assert!(matches!(
        &recovered[0],
        AuthoredTurnCommit::Committed {
            applied_to_current_projection: true,
            ..
        }
    ));
    assert!(fixture
        .authored
        .recover_turns(&fixture.pool, None, &thread.id)
        .await
        .expect("idempotent recovery")
        .is_empty());

    let retry = fixture
        .authored
        .finalize_turn(
            &fixture.pool,
            None,
            FinalizeAuthoredTurnInput {
                thread_id: thread.id.clone(),
                assistant_message_id: "message-one".into(),
                branch_commit_id: prepared.branch_commit_id,
            },
        )
        .await
        .expect("idempotent finalize");
    assert!(matches!(
        retry,
        AuthoredTurnCommit::Committed { changed: true, .. }
    ));

    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .expect("main head");
    let worktree = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "request-one".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .expect("worktree");
    assert!(Path::new(&worktree.path).is_dir());
    assert!(worktree.branch.starts_with("agents/worktrees/"));
    fixture
        .authored
        .store
        .branch_head(
            &scope.repository_id,
            &format!("agents/threads/{}", thread.id),
        )
        .expect("thread ref coexists");
    fixture
        .authored
        .remove_worktree(&fixture.pool, None, &thread.id, &worktree.id)
        .await
        .expect("remove worktree");
}

#[tokio::test]
async fn restore_replays_the_original_commit_and_rejects_operation_id_rebinding() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let initial = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap();
    let first_graph = graph_with_args(vec![scalar_arg("first", "first", 0.25)]);
    let first = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "first-graph",
            first_graph.clone(),
            initial.document.revision(),
            "First graph",
        )
        .await
        .unwrap();
    let second_graph = graph_with_args(vec![scalar_arg("second", "second", 0.75)]);
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "second-graph",
            second_graph.clone(),
            match &first.document {
                AuthoredProjectedDocument::PatternGraph { revision, .. } => revision,
                _ => unreachable!(),
            },
            "Second graph",
        )
        .await
        .unwrap();
    let graph_replay = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "first-graph",
            first_graph.clone(),
            initial.document.revision(),
            "Retry first graph after response loss",
        )
        .await
        .unwrap();
    assert_eq!(graph_replay.commit_id, first.commit_id);
    assert!(graph_replay.changed);
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = graph_replay.document else {
        panic!("graph replay returned a track document")
    };
    assert_eq!(
        graph_revision(&graph).unwrap(),
        graph_revision(&second_graph).unwrap()
    );

    let restored = fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            &first.commit_id,
            "restore-one",
        )
        .await
        .unwrap();
    assert!(restored.applied_to_current_projection);
    let restore_commit = restored.commit_id.clone();

    let restored_revision = match &restored.document {
        AuthoredProjectedDocument::PatternGraph {
            revision, graph, ..
        } => {
            assert_eq!(
                graph_revision(graph).unwrap(),
                graph_revision(&first_graph).unwrap()
            );
            revision.clone()
        }
        _ => unreachable!(),
    };
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "advance-after-restore",
            empty_graph(),
            &restored_revision,
            "Advance after restore response",
        )
        .await
        .unwrap();
    let replay = fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            &first.commit_id,
            "restore-one",
        )
        .await
        .unwrap();
    assert_eq!(replay.commit_id, restore_commit);
    assert!(!replay.applied_to_current_projection);

    let mismatch = fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            &initial.head.to_string(),
            "restore-one",
        )
        .await
        .unwrap_err();
    assert!(mismatch
        .to_string()
        .contains("already bound to a different target"));
}

#[tokio::test]
async fn history_pages_the_unbounded_mainline_and_old_pages_remain_restorable() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let initial = fixture
        .authored
        .reconcile_locked(&fixture.pool, &scope)
        .await
        .unwrap();
    let initial_commit = initial.head.to_string();
    let mut revision = initial.document.revision().to_owned();

    for index in 0..5 {
        let applied = fixture
            .authored
            .apply_graph_for_scope(
                &fixture.pool,
                None,
                "pattern",
                &implementation_id("pattern"),
                &format!("graph-revision-{index}"),
                graph_with_args(vec![scalar_arg("gain", "gain", f64::from(index))]),
                &revision,
                &format!("Graph revision {index}"),
            )
            .await
            .unwrap();
        revision = match applied.document {
            AuthoredProjectedDocument::PatternGraph { revision, .. } => revision,
            _ => unreachable!(),
        };
    }

    let mut cursor = None;
    let mut commits = Vec::new();
    loop {
        let page = fixture
            .authored
            .list_history(&fixture.pool, None, &thread.id, cursor.as_deref(), Some(2))
            .await
            .unwrap();
        assert!(!page.entries.is_empty());
        commits.extend(page.entries.into_iter().map(|entry| entry.commit_id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(commits.len(), 6);
    assert_eq!(commits.last(), Some(&initial_commit));
    assert_eq!(commits.iter().collect::<HashSet<_>>().len(), commits.len());

    let restored = fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            &initial_commit,
            "restore-old-page",
        )
        .await
        .unwrap();
    match restored.document {
        AuthoredProjectedDocument::PatternGraph { graph, .. } => {
            assert_eq!(
                graph_revision(&graph).unwrap(),
                graph_revision(&empty_graph()).unwrap()
            );
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn divergent_turns_merge_disjoint_edits_and_persist_same_field_conflicts() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread_a = fixture.pattern_thread("pattern").await;
    let thread_b = fixture.pattern_thread("pattern").await;

    let a_graph = graph_with_args(vec![scalar_arg("gain", "gain", 0.5)]);
    let a = prepare(&fixture, &thread_a, "a-one", a_graph).await;
    fixture.append_assistant(&thread_a.id, "a-one").await;

    let initial_revision = graph_revision(&empty_graph()).unwrap();
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "add-speed",
            graph_with_args(vec![scalar_arg("speed", "speed", 1.0)]),
            &initial_revision,
            "Add speed",
        )
        .await
        .expect("concurrent disjoint edit");
    let merged = fixture
        .authored
        .finalize_turn(
            &fixture.pool,
            None,
            FinalizeAuthoredTurnInput {
                thread_id: thread_a.id.clone(),
                assistant_message_id: "a-one".into(),
                branch_commit_id: a.branch_commit_id,
            },
        )
        .await
        .expect("typed turn merge");
    let merged_graph = match merged {
        AuthoredTurnCommit::Committed {
            document: AuthoredProjectedDocument::PatternGraph { graph, .. },
            ..
        } => graph,
        other => panic!("expected merged graph, got {other:?}"),
    };
    assert_eq!(merged_graph.args.len(), 2);

    let mut ours = merged_graph.clone();
    ours.args
        .iter_mut()
        .find(|arg| arg.id == "gain")
        .unwrap()
        .name = "agent_gain".into();
    let conflict_prepare = prepare(&fixture, &thread_a, "a-conflict", ours).await;
    fixture.append_assistant(&thread_a.id, "a-conflict").await;

    let mut theirs = merged_graph;
    theirs
        .args
        .iter_mut()
        .find(|arg| arg.id == "gain")
        .unwrap()
        .name = "human_gain".into();
    let base_revision = graph_revision(&theirs).unwrap();
    // `theirs` was cloned from the current graph before its mutation, so use
    // the current document revision as the CAS base.
    let current = load_graph_document(
        &fixture.pool,
        &GraphScope {
            pattern_id: "pattern".into(),
            implementation_id: implementation_id("pattern"),
            owner_user_id: None,
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
            "conflicting-human-edit",
            theirs,
            &current.revision,
            "Conflicting human edit",
        )
        .await
        .expect("conflicting main edit");
    assert_ne!(base_revision, current.revision);

    let conflict_input = FinalizeAuthoredTurnInput {
        thread_id: thread_a.id.clone(),
        assistant_message_id: "a-conflict".into(),
        branch_commit_id: conflict_prepare.branch_commit_id,
    };
    let conflicted = fixture
        .authored
        .finalize_turn(&fixture.pool, None, conflict_input.clone())
        .await
        .expect("structured conflict");
    let conflicts = match conflicted {
        AuthoredTurnCommit::Conflicted { conflicts, .. } => conflicts,
        other => panic!("expected conflict, got {other:?}"),
    };
    assert!(!conflicts.is_empty());
    let retry = fixture
        .authored
        .finalize_turn(&fixture.pool, None, conflict_input)
        .await
        .expect("terminal conflict retry");
    assert!(matches!(retry, AuthoredTurnCommit::Conflicted { .. }));
    assert!(fixture
        .authored
        .recover_turns(&fixture.pool, None, &thread_a.id)
        .await
        .expect("conflict is not auto-retried")
        .is_empty());

    // Another thread remains independently usable after A's turn conflict.
    let next_graph = load_graph_document(
        &fixture.pool,
        &GraphScope {
            pattern_id: "pattern".into(),
            implementation_id: implementation_id("pattern"),
            owner_user_id: None,
        },
    )
    .await
    .unwrap()
    .graph;
    let next = prepare(&fixture, &thread_b, "b-next", next_graph).await;
    fixture.append_assistant(&thread_b.id, "b-next").await;
    assert!(matches!(
        fixture
            .authored
            .finalize_turn(
                &fixture.pool,
                None,
                FinalizeAuthoredTurnInput {
                    thread_id: thread_b.id,
                    assistant_message_id: "b-next".into(),
                    branch_commit_id: next.branch_commit_id,
                },
            )
            .await
            .unwrap(),
        AuthoredTurnCommit::Committed { .. }
    ));
}

#[tokio::test]
async fn turn_merge_runtime_validation_failure_is_a_terminal_replayable_conflict() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let base_graph = Graph {
        nodes: vec![
            catalog_node("source", "scalar", 0.0),
            catalog_node("view", "view_signal", 1.0),
        ],
        edges: Vec::new(),
        args: Vec::new(),
    };
    let base = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "install-runtime-validation-base",
            base_graph.clone(),
            &graph_revision(&empty_graph()).unwrap(),
            "Install runtime validation merge base",
        )
        .await
        .expect("install valid base graph");
    let base_revision = match &base.document {
        AuthoredProjectedDocument::PatternGraph { revision, .. } => revision.clone(),
        AuthoredProjectedDocument::TrackScore { .. } => unreachable!(),
    };
    let thread = fixture.pattern_thread("pattern").await;

    let mut agent_graph = base_graph.clone();
    agent_graph.nodes[0] = catalog_node("source", "beat_pulses", 0.0);
    let prepared = prepare(&fixture, &thread, "runtime-invalid-turn", agent_graph).await;
    fixture
        .append_assistant(&thread.id, "runtime-invalid-turn")
        .await;

    let mut main_graph = base_graph;
    main_graph.edges.push(Edge {
        id: "source:out->view:in".into(),
        from_node: "source".into(),
        from_port: "out".into(),
        to_node: "view".into(),
        to_port: "in".into(),
    });
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "install-concurrent-source-use",
            main_graph.clone(),
            &base_revision,
            "Use source output concurrently",
        )
        .await
        .expect("each side is independently valid");

    let input = FinalizeAuthoredTurnInput {
        thread_id: thread.id.clone(),
        assistant_message_id: "runtime-invalid-turn".into(),
        branch_commit_id: prepared.branch_commit_id,
    };
    let first = fixture
        .authored
        .finalize_turn(&fixture.pool, None, input.clone())
        .await
        .expect("invalid combined graph becomes a conflict");
    let first_conflicts = match first {
        AuthoredTurnCommit::Conflicted { conflicts, .. } => conflicts,
        other => panic!("expected terminal graph validation conflict, got {other:?}"),
    };
    assert!(first_conflicts.iter().any(|conflict| {
        conflict.kind == AuthoredMergeConflictKind::InvalidInput
            && conflict
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("unknown output source.out"))
    }));

    // A later main commit must not reopen or reinterpret the completed turn.
    let current =
        load_graph_document_unscoped(&fixture.pool, "pattern", &implementation_id("pattern"))
            .await
            .unwrap();
    let mut later = current.graph;
    later.nodes[0].position_x = Some(99.0);
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            &implementation_id("pattern"),
            "advance-after-runtime-conflict",
            later,
            &current.revision,
            "Advance after runtime conflict",
        )
        .await
        .unwrap();

    let replay = fixture
        .authored
        .finalize_turn(&fixture.pool, None, input)
        .await
        .expect("terminal conflict replay");
    let replay_conflicts = match replay {
        AuthoredTurnCommit::Conflicted { conflicts, .. } => conflicts,
        other => panic!("expected replayed conflict, got {other:?}"),
    };
    assert_eq!(
        serde_json::to_value(&replay_conflicts).unwrap(),
        serde_json::to_value(&first_conflicts).unwrap()
    );
    assert!(fixture
        .authored
        .recover_turns(&fixture.pool, None, &thread.id)
        .await
        .expect("terminal conflict is not retried during recovery")
        .is_empty());
    let status: String = sqlx::query_scalar(
        "SELECT status FROM authored_state_turn_commits
         WHERE thread_id = ? AND assistant_message_id = 'runtime-invalid-turn'",
    )
    .bind(&thread.id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(status, "conflicted");
}
