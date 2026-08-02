use super::super::*;
use super::support::*;

#[tokio::test]
async fn sign_out_retains_the_venue_score_and_restores_a_loose_graph_without_new_commits() {
    let fixture = Fixture::new().await;
    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    for pattern_id in ["graph-pattern", "wash"] {
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES (?, 'alice', ?)")
            .bind(pattern_id)
            .bind(pattern_id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
             VALUES (?, 'alice', ?, ?, ?)",
        )
        .bind(implementation_id(pattern_id))
        .bind(pattern_id)
        .bind(format!("{pattern_id}-implementation"))
        .bind(serde_json::to_string(&empty_graph()).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
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
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score', 'alice', 'track', 'venue', 'score')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    let graph_thread = fixture
        .authored
        .create_thread_with_authored_state(
            &fixture.pool,
            CreateAgentThreadInput {
                request_id: uuid::Uuid::new_v4().to_string(),
                agent_kind: "pattern_graph".into(),
                subject_kind: Some("pattern".into()),
                subject_id: Some("graph-pattern".into()),
                implementation_id: Some(implementation_id("graph-pattern")),
                ..Default::default()
            },
            Some("alice"),
        )
        .await
        .unwrap();
    let track_thread = fixture
        .authored
        .create_thread_with_authored_state(
            &fixture.pool,
            CreateAgentThreadInput {
                request_id: uuid::Uuid::new_v4().to_string(),
                agent_kind: "track_copilot".into(),
                subject_kind: Some("track".into()),
                subject_id: Some("track".into()),
                venue_id: Some("venue".into()),
                score_id: Some("score".into()),
                ..Default::default()
            },
            Some("alice"),
        )
        .await
        .unwrap();
    let authored_graph = graph_with_args(vec![scalar_arg("gain", "gain", 0.75)]);
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            Some("alice"),
            "graph-pattern",
            &implementation_id("graph-pattern"),
            "author-graph",
            authored_graph.clone(),
            &graph_revision(&empty_graph()).unwrap(),
            "Author graph",
        )
        .await
        .unwrap();
    let track_scope = TrackScope {
        score_id: "score".into(),
        track_id: "track".into(),
        venue_id: "venue".into(),
    };
    let created_clip = fixture
        .authored
        .create_track_score_for_scope(
            &fixture.pool,
            Some("alice"),
            track_scope.clone(),
            CreateTrackScoreInput {
                request_id: "00000000-0000-4000-8000-000000000003".into(),
                score_id: "score".into(),
                track_id: "track".into(),
                pattern_id: "wash".into(),
                start_time: 4.0,
                end_time: 8.0,
                z_index: 2,
                blend_mode: None,
                args: Some(json!({})),
            },
            "Author track",
        )
        .await
        .unwrap();

    let graph_history = fixture
        .authored
        .list_history(&fixture.pool, Some("alice"), &graph_thread.id, None, None)
        .await
        .unwrap();
    let track_history = fixture
        .authored
        .list_history(&fixture.pool, Some("alice"), &track_thread.id, None, None)
        .await
        .unwrap();
    let graph_scope = ResolvedScope::from_thread(&graph_thread, Some("alice")).unwrap();
    let resolved_track_scope = ResolvedScope::from_thread(&track_thread, Some("alice")).unwrap();
    let graph_head = fixture
        .authored
        .store
        .main_head(&graph_scope.repository_id)
        .unwrap();
    let track_head = fixture
        .authored
        .store
        .main_head(&resolved_track_scope.repository_id)
        .unwrap();

    for table in ["venues", "tracks", "patterns", "scores"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table}
             SET synced_at = updated_at, version = version + 1
             WHERE uid = 'alice'",
        )))
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
    crate::commands::auth::wipe_database_pool(&fixture.pool, &fixture.authored, "alice")
        .await
        .unwrap();

    assert!(
        crate::database::local::patterns::list_patterns_pool(&fixture.pool)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1,
        "the sealed physical venue cache retains its lossless score projection"
    );
    assert!(fixture
        .authored
        .list_history(&fixture.pool, Some("bob"), &graph_thread.id, None, None)
        .await
        .is_err());
    assert!(fixture
        .authored
        .list_history(&fixture.pool, Some("bob"), &track_thread.id, None, None)
        .await
        .is_err());

    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("bob"))
        .await
        .unwrap();

    // A metadata pull under another principal may reuse a public pattern ID,
    // but that catalog row cannot claim Alice's retained repository. The
    // physical venue/score closure itself remains sealed under Alice's owner
    // identity and is not replaced during Bob's session.
    sqlx::query(
        "INSERT INTO patterns (id, uid, name) VALUES ('graph-pattern', 'bob', 'graph-pattern')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert!(fixture
        .authored
        .list_history(&fixture.pool, Some("alice"), &graph_thread.id, None, None)
        .await
        .is_err());
    assert!(fixture
        .authored
        .list_history(&fixture.pool, Some("alice"), &track_thread.id, None, None)
        .await
        .is_err());
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&graph_scope.repository_id)
            .unwrap(),
        graph_head
    );
    assert_eq!(
        fixture
            .authored
            .store
            .main_head(&resolved_track_scope.repository_id)
            .unwrap(),
        track_head
    );

    // Repository identity is immutable: a later correct pull replaces Bob's
    // colliding loose catalog row; it never rebinds it in place.
    let mut cleanup = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    crate::database::local::write_admission::enter_maintenance_writes(&mut cleanup, Some("bob"))
        .await
        .unwrap();
    sqlx::query("DELETE FROM patterns WHERE id = 'graph-pattern'")
        .execute(&mut *cleanup)
        .await
        .unwrap();
    crate::database::local::write_admission::leave_maintenance_writes(&mut cleanup, Some("bob"))
        .await
        .unwrap();
    cleanup.commit().await.unwrap();
    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO patterns (id, uid, name)
         VALUES ('graph-pattern', 'alice', 'graph-pattern')",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    let restored_graph_history = fixture
        .authored
        .list_history(&fixture.pool, Some("alice"), &graph_thread.id, None, None)
        .await
        .unwrap();
    let restored_track_history = fixture
        .authored
        .list_history(&fixture.pool, Some("alice"), &track_thread.id, None, None)
        .await
        .unwrap();
    assert_eq!(
        restored_graph_history
            .entries
            .iter()
            .map(|entry| &entry.commit_id)
            .collect::<Vec<_>>(),
        graph_history
            .entries
            .iter()
            .map(|entry| &entry.commit_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        restored_track_history
            .entries
            .iter()
            .map(|entry| &entry.commit_id)
            .collect::<Vec<_>>(),
        track_history
            .entries
            .iter()
            .map(|entry| &entry.commit_id)
            .collect::<Vec<_>>()
    );
    let restored_graph = load_graph_document(
        &fixture.pool,
        &GraphScope {
            pattern_id: "graph-pattern".into(),
            implementation_id: implementation_id("graph-pattern"),
            owner_user_id: Some("alice".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        restored_graph.revision,
        graph_revision(&authored_graph).unwrap()
    );
    let restored_implementation: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT uid, name FROM implementations WHERE id = ? AND pattern_id = 'graph-pattern'",
    )
    .bind(implementation_id("graph-pattern"))
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        restored_implementation,
        (
            Some("alice".into()),
            Some("graph-pattern-implementation".into())
        )
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM implementations WHERE pattern_id = 'wash'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT id FROM track_scores WHERE score_id = 'score'",)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        created_clip_id(&created_clip)
    );
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT materialization_state FROM authored_state_projections ORDER BY repository_id",
    )
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(states, vec!["present", "present", "present"]);
}

#[tokio::test]
async fn thread_identity_survives_cleanup_failure_and_deletes_after_cleanup() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;

    let error = fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async {
            Err("workspace is busy".into())
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("workspace is busy"));
    assert!(
        agent_threads::get_thread_row(&fixture.pool, &thread.id, None)
            .await
            .is_err()
    );
    agent_threads::find_thread_row_including_deleting(&fixture.pool, &thread.id, None)
        .await
        .unwrap()
        .expect("cleanup failure keeps retryable deleting identity");
    let lifecycle: String =
        sqlx::query_scalar("SELECT lifecycle_state FROM agent_threads WHERE id = ?")
            .bind(&thread.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(lifecycle, "deleting");
    let routing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_state_thread_branches WHERE thread_id = ?",
    )
    .bind(&thread.id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(routing_count, 1);

    let cleaned = AtomicBool::new(false);
    fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async {
            cleaned.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .expect("retry deletion");
    assert!(cleaned.load(Ordering::SeqCst));
    assert!(
        agent_threads::get_thread_row(&fixture.pool, &thread.id, None)
            .await
            .is_err()
    );
    let routing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_state_thread_branches WHERE thread_id = ?",
    )
    .bind(&thread.id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(routing_count, 0);
}

#[tokio::test]
async fn completed_thread_deletion_is_exact_owner_terminal_idempotent() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let resolved = ResolvedScope::from_thread(&thread, None).unwrap();
    let cleanup_count = AtomicUsize::new(0);

    agent_threads::mark_thread_deleting(&fixture.pool, &thread.id, None)
        .await
        .unwrap();
    let mut invalid_receipt = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let error = agent_threads::insert_thread_deletion_receipt(
        &mut invalid_receipt,
        &thread.id,
        None,
        "unrelated-repository",
    )
    .await
    .expect_err("the terminal receipt must match the thread routing");
    assert!(error.contains("terminal scope admission"));
    invalid_receipt.rollback().await.unwrap();

    let deleted = fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async {
            cleanup_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .expect("initial deletion");
    assert_eq!(
        deleted.as_ref().map(|deleted| deleted.id.as_str()),
        Some(thread.id.as_str())
    );

    // Model a committed delete whose response was lost: the same owner may
    // replay it, but external cleanup must not run a second time.
    let replayed = fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, || async {
            cleanup_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .expect("terminal deletion replay");
    assert!(replayed.is_none());
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        agent_threads::find_thread_deletion_receipt(&fixture.pool, &thread.id, None)
            .await
            .unwrap()
            .as_deref(),
        Some(resolved.repository_id.as_str())
    );

    let error = fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, Some("bob"), &thread.id, || async {
            cleanup_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .expect_err("another owner cannot observe or replay the receipt");
    assert!(matches!(error, AuthoredDocumentsError::Scope(_)));
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_track_apply_cannot_invert_repository_and_execution_locks() {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agent_execution::workspace::PythonWorkspaceService;

    let fixture = Fixture::new().await;
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

    // `track.apply` performs this optimistic active read before it waits for
    // the authored repository. Pause at that exact boundary while its cell
    // still owns the execution/kernel lease.
    let pending_apply = fixture
        .authored
        .resolve_active_thread_lock(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let workspaces = Arc::new(PythonWorkspaceService::new(
        fixture._directory.path().join("lock-inversion-workspaces"),
        Arc::new(|| Err("the lock-order test never launches Python".into())),
    ));
    let running_cell = workspaces.claim_cell(&thread.id).unwrap();
    let (deleting_tx, deleting_rx) = tokio::sync::oneshot::channel();
    let delete_authored = fixture.authored.clone();
    let delete_pool = fixture.pool.clone();
    let delete_thread_id = thread.id.clone();
    let cleanup_thread_id = delete_thread_id.clone();
    let delete_workspaces = Arc::clone(&workspaces);
    let deletion = tokio::spawn(async move {
        delete_authored
            .delete_thread_with_authored_state(
                &delete_pool,
                None,
                &delete_thread_id,
                || async move {
                    deleting_tx
                        .send(())
                        .map_err(|_| "signal deleting lifecycle".to_string())?;
                    delete_workspaces.retire_thread(&cleanup_thread_id).await
                },
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        deleting_rx.await.expect("deletion reached execution drain");
        let lifecycle: String =
            sqlx::query_scalar("SELECT lifecycle_state FROM agent_threads WHERE id = ?")
                .bind(&thread.id)
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
        assert_eq!(lifecycle, "deleting");
        let error = fixture
            .authored
            .acquire_active_thread_lock(&fixture.pool, None, pending_apply)
            .await
            .err()
            .expect("deleting must stay terminal after the repository wait");
        assert!(matches!(error, AuthoredDocumentsError::Scope(_)));
        // Releasing the simulated kernel holder lets deletion finish. If
        // deletion retained the repository lock during its drain, the acquire
        // above and this lease would wait on each other forever.
        drop(running_cell);
        deletion.await.unwrap().unwrap();
    })
    .await
    .expect("repository/execution lock order must make progress");
}

#[tokio::test]
async fn startup_recovery_discovers_and_finishes_hidden_deleting_threads() {
    use std::sync::Arc;

    use crate::agent_execution::graph_runs::GraphRunStore;
    use crate::agent_execution::workspace::PythonWorkspaceService;

    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let workspace_root = fixture._directory.path().join("agent-workspaces");
    let workspaces = PythonWorkspaceService::new(
        workspace_root.clone(),
        Arc::new(|| Err("the recovery test never launches Python".into())),
    );
    workspaces.workspace_for_test(&thread.id).unwrap();
    assert!(workspace_root.join(&thread.id).is_dir());

    agent_threads::mark_thread_deleting(&fixture.pool, &thread.id, None)
        .await
        .unwrap();
    assert!(
        agent_threads::get_thread_row(&fixture.pool, &thread.id, None)
            .await
            .is_err()
    );

    let recovered = crate::commands::agent_threads::recover_deleting_agent_threads(
        &fixture.pool,
        &fixture.authored,
        &workspaces,
        &GraphRunStore::new(),
    )
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    assert!(!workspace_root.join(&thread.id).exists());
    assert!(
        agent_threads::find_thread_row_including_deleting(&fixture.pool, &thread.id, None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn startup_recovery_never_finishes_another_principals_deleting_thread() {
    use std::sync::Arc;

    use crate::agent_execution::graph_runs::GraphRunStore;
    use crate::agent_execution::workspace::PythonWorkspaceService;

    let fixture = Fixture::new().await;
    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'pattern')")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, graph_json)
         VALUES ('implementation-pattern', 'alice', 'pattern', ?)",
    )
    .bind(exact_graph_json(&empty_graph()).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let thread = fixture
        .authored
        .create_thread_with_authored_state(
            &fixture.pool,
            CreateAgentThreadInput {
                request_id: uuid::Uuid::new_v4().to_string(),
                agent_kind: "pattern_graph".into(),
                subject_kind: Some("pattern".into()),
                subject_id: Some("pattern".into()),
                implementation_id: Some("implementation-pattern".into()),
                ..Default::default()
            },
            Some("alice"),
        )
        .await
        .unwrap();
    let workspace_root = fixture
        ._directory
        .path()
        .join("principal-recovery-workspaces");
    let workspaces = PythonWorkspaceService::new(
        workspace_root.clone(),
        Arc::new(|| Err("the recovery test never launches Python".into())),
    );
    workspaces.workspace_for_test(&thread.id).unwrap();
    agent_threads::mark_thread_deleting(&fixture.pool, &thread.id, Some("alice"))
        .await
        .unwrap();

    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("bob"))
        .await
        .unwrap();
    assert_eq!(
        crate::commands::agent_threads::recover_deleting_agent_threads(
            &fixture.pool,
            &fixture.authored,
            &workspaces,
            &GraphRunStore::new(),
        )
        .await
        .unwrap(),
        0
    );
    assert!(workspace_root.join(&thread.id).is_dir());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_threads
             WHERE id = ? AND owner_user_id = 'alice' AND lifecycle_state = 'deleting'",
        )
        .bind(&thread.id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        1
    );

    crate::database::local::auth::arm_write_admission(&fixture.pool, Some("alice"))
        .await
        .unwrap();
    assert_eq!(
        crate::commands::agent_threads::recover_deleting_agent_threads(
            &fixture.pool,
            &fixture.authored,
            &workspaces,
            &GraphRunStore::new(),
        )
        .await
        .unwrap(),
        1
    );
    assert!(!workspace_root.join(&thread.id).exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_threads WHERE id = ?")
            .bind(&thread.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn deleting_gate_rejects_racing_turn_and_worktree_creation() {
    let fixture = Fixture::new().await;
    fixture.add_pattern("pattern").await;
    let thread = fixture.pattern_thread("pattern").await;
    let scope = ResolvedScope::from_thread(&thread, None).unwrap();
    let base = fixture
        .authored
        .store
        .main_head(&scope.repository_id)
        .expect("main head");

    let authored = fixture.authored.clone();
    let pool = fixture.pool.clone();
    let thread_id = thread.id.clone();
    let cleanup_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let cleanup_started_in_delete = std::sync::Arc::clone(&cleanup_started);
    let (release_cleanup, wait_for_release) = std::sync::mpsc::channel();
    let runtime = tokio::runtime::Handle::current();
    let deletion = tokio::task::spawn_blocking(move || {
        runtime.block_on(authored.delete_thread_with_authored_state(
            &pool,
            None,
            &thread_id,
            || async move {
                cleanup_started_in_delete.notify_one();
                wait_for_release
                    .recv()
                    .map_err(|error| format!("release deletion cleanup: {error}"))?;
                Ok(())
            },
        ))
    });

    cleanup_started.notified().await;
    let lifecycle: String =
        sqlx::query_scalar("SELECT lifecycle_state FROM agent_threads WHERE id = ?")
            .bind(&thread.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(lifecycle, "deleting");

    let worktree_error = fixture
        .authored
        .create_worktree(
            &fixture.pool,
            None,
            CreateAuthoredWorktreeInput {
                thread_id: thread.id.clone(),
                request_id: "racing-worktree".into(),
                expected_base_commit_id: base.to_string(),
            },
        )
        .await
        .unwrap_err();
    assert!(worktree_error.to_string().contains("not found"));
    let turn_error = fixture
        .authored
        .prepare_turn(
            &fixture.pool,
            None,
            PrepareAuthoredTurnInput {
                thread_id: thread.id.clone(),
                assistant_message_id: "racing-turn".into(),
                graph: Some(empty_graph()),
            },
        )
        .await
        .unwrap_err();
    assert!(turn_error.to_string().contains("not found"));

    let worktree_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_state_worktrees
         WHERE owner_thread_id = ? AND request_id = 'racing-worktree'",
    )
    .bind(&thread.id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(worktree_rows, 0);
    let turn_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_state_turn_commits
         WHERE thread_id = ? AND assistant_message_id = 'racing-turn'",
    )
    .bind(&thread.id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(turn_rows, 0);

    release_cleanup.send(()).unwrap();
    deletion
        .await
        .expect("deletion task")
        .expect("deletion completes");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_threads WHERE id = ?")
        .bind(&thread.id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn queued_lifecycle_writer_does_not_deadlock_thread_branch_initialization() {
    let fixture = Fixture::new().await;
    let scope = fixture.add_track_scope().await;
    let input = CreateAgentThreadInput {
        request_id: "30000000-0000-4000-8000-000000000000".into(),
        agent_kind: "track_copilot".into(),
        subject_kind: Some("track".into()),
        subject_id: Some(scope.track_id.clone()),
        implementation_id: None,
        venue_id: Some(scope.venue_id.clone()),
        score_id: Some(scope.score_id.clone()),
        title: Some("Queued lifecycle writer".into()),
    };

    // Hold SQLite's writer slot so creation pauses after taking the lifecycle
    // read guard but before it initializes the deterministic thread branch.
    let blocker = fixture
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("hold SQLite writer slot");
    let authored = fixture.authored.clone();
    let pool = fixture.pool.clone();
    let creation = tokio::spawn(async move {
        authored
            .create_thread_with_authored_state(&pool, input, None)
            .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if fixture.authored.lifecycle_lock.try_write().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("thread creation must acquire the lifecycle read guard");

    let lifecycle = Arc::clone(&fixture.authored.lifecycle_lock);
    let (writer_started, writer_attempting) = tokio::sync::oneshot::channel();
    let writer = tokio::spawn(async move {
        writer_started.send(()).expect("observe writer attempt");
        let _guard = lifecycle.write_owned().await;
    });
    writer_attempting.await.expect("lifecycle writer task");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            // Tokio's fair RwLock refuses new readers once a writer is queued.
            if fixture.authored.lifecycle_lock.try_read().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lifecycle writer must be queued behind thread creation");

    blocker.commit().await.expect("release SQLite writer slot");
    let created = tokio::time::timeout(std::time::Duration::from_secs(5), creation)
        .await
        .expect("thread creation must not reacquire the lifecycle lock")
        .expect("thread creation task")
        .expect("create thread with authored state");
    assert_eq!(created.score_id.as_deref(), Some(scope.score_id.as_str()));
    tokio::time::timeout(std::time::Duration::from_secs(2), writer)
        .await
        .expect("queued lifecycle writer must proceed after creation")
        .expect("lifecycle writer task");
}

#[tokio::test]
async fn malformed_thread_creation_leaves_no_row_and_valid_routes_initialize() {
    let fixture = Fixture::new().await;
    let malformed = CreateAgentThreadInput {
        request_id: "30000000-0000-4000-8000-000000000010".into(),
        agent_kind: "track_copilot".into(),
        subject_kind: Some("track".into()),
        subject_id: Some("track".into()),
        venue_id: Some("venue".into()),
        score_id: None,
        ..Default::default()
    };
    let error = fixture
        .authored
        .create_thread_with_authored_state(&fixture.pool, malformed, None)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthoredDocumentsError::Invalid(_)));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_threads")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_state_creations WHERE creation_kind = 'agent_thread'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );

    fixture.add_pattern("pattern").await;
    let track_scope = fixture.add_track_scope().await;
    let track = fixture.track_thread(&track_scope).await;
    let graph = fixture.pattern_thread("pattern").await;
    assert!(matches!(
        track.authored_route().unwrap(),
        AuthoredThreadRoute::Track { .. }
    ));
    assert!(matches!(
        graph.authored_route().unwrap(),
        AuthoredThreadRoute::Pattern { .. }
    ));
}

#[tokio::test]
async fn agent_thread_creation_replays_same_branch_and_never_resurrects() {
    let fixture = Fixture::new().await;
    let scope = fixture.add_track_scope().await;
    let input = CreateAgentThreadInput {
        request_id: "30000000-0000-4000-8000-000000000001".into(),
        agent_kind: "track_copilot".into(),
        subject_kind: Some("track".into()),
        subject_id: Some(scope.track_id.clone()),
        implementation_id: None,
        venue_id: Some(scope.venue_id.clone()),
        score_id: Some(scope.score_id.clone()),
        title: Some("First conversation".into()),
    };
    let created = fixture
        .authored
        .create_thread_with_authored_state(&fixture.pool, input.clone(), None)
        .await
        .unwrap();
    let resolved = ResolvedScope::from_thread(&created, None).unwrap();
    let branch = format!("agents/threads/{}", created.id);
    let original_branch_head = fixture
        .authored
        .store
        .branch_head(&resolved.repository_id, &branch)
        .unwrap();

    let replayed = fixture
        .authored
        .create_thread_with_authored_state(&fixture.pool, input.clone(), None)
        .await
        .unwrap();
    assert_eq!(replayed.id, created.id);
    assert_eq!(
        fixture
            .authored
            .store
            .branch_head(&resolved.repository_id, &branch)
            .unwrap(),
        original_branch_head
    );
    let thread_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_threads")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(thread_count, 1);

    let mut rebound = input.clone();
    rebound.title = Some("Different title".into());
    let error = fixture
        .authored
        .create_thread_with_authored_state(&fixture.pool, rebound, None)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthoredDocumentsError::Invalid(_)));

    fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &created.id, || async {
            Ok::<(), String>(())
        })
        .await
        .unwrap();
    let stale = fixture
        .authored
        .create_thread_with_authored_state(&fixture.pool, input.clone(), None)
        .await
        .unwrap_err();
    assert!(
        stale.to_string().contains("was later deleted"),
        "unexpected stale replay error: {stale}"
    );
    let surviving_association: (String, String) = sqlx::query_as(
        "SELECT subject_id, commit_id FROM authored_state_creations
         WHERE principal_key = 'signed-out' AND creation_kind = 'agent_thread' AND request_id = ?",
    )
    .bind(&input.request_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(surviving_association.0, created.id);
    assert_eq!(surviving_association.1, original_branch_head.to_string());
    let remaining_threads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_threads")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(remaining_threads, 0);
}
