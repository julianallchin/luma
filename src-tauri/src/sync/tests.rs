#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use crate::models::agent_threads::{
        AppendAgentThreadMessagesInput, CreateAgentThreadInput, NewAgentThreadMessage,
    };
    use crate::services::authored_state::{
        Actor, AuthoredRevisionStore, NewAuthoredDocument, RevisionMetadata,
    };
    use crate::sync::authored_remote::{
        ArchiveAuthoredDocumentInput, ArchiveAuthoredDocumentReceipt,
    };
    use crate::sync::error::SyncError;
    use crate::sync::pending;
    use crate::sync::pull;
    use crate::sync::push;
    use crate::sync::registry;
    use crate::sync::state;
    use crate::sync::traits::RemoteClient;

    // ========================================================================
    // Mock remote client
    // ========================================================================

    /// Records all calls and returns canned responses.
    struct MockRemoteClient {
        /// Canned responses: key = "{table}:{query_prefix}", value = rows to return.
        select_responses: Mutex<HashMap<String, Vec<Value>>>,
        /// One-shot pages: served on the first select of that table, then gone.
        select_pages: Mutex<HashMap<String, Vec<Value>>>,
        /// Tables queried by the pull engine.
        selected_tables: Mutex<Vec<String>>,
        /// All upsert calls recorded here for assertion.
        upserted: Mutex<Vec<(String, Value)>>,
        /// Optional one-shot API failure for the next upsert.
        next_upsert_error: Mutex<Option<(u16, String)>>,
    }

    impl MockRemoteClient {
        fn new() -> Self {
            Self {
                select_responses: Mutex::new(HashMap::new()),
                select_pages: Mutex::new(HashMap::new()),
                selected_tables: Mutex::new(Vec::new()),
                upserted: Mutex::new(Vec::new()),
                next_upsert_error: Mutex::new(None),
            }
        }

        /// Register a canned response for select queries on a table.
        fn on_select(&self, table: &str, rows: Vec<Value>) {
            self.select_responses
                .lock()
                .unwrap()
                .insert(table.to_string(), rows);
        }

        /// Register one page and then nothing, the way a keyset cursor
        /// behaves: the pull loop asks again with the advanced cursor and the
        /// server has no more rows for it.
        fn on_select_page(&self, table: &str, rows: Vec<Value>) {
            self.select_pages
                .lock()
                .unwrap()
                .insert(table.to_string(), rows);
        }

        fn upsert_count(&self) -> usize {
            self.upserted.lock().unwrap().len()
        }

        fn selected_tables(&self) -> Vec<String> {
            self.selected_tables.lock().unwrap().clone()
        }

        fn fail_next_upsert(&self, status: u16, message: &str) {
            *self.next_upsert_error.lock().unwrap() = Some((status, message.to_string()));
        }
    }

    #[async_trait]
    impl RemoteClient for MockRemoteClient {
        async fn select_json(
            &self,
            table: &str,
            _query: &str,
            _token: &str,
        ) -> Result<Vec<Value>, SyncError> {
            self.selected_tables.lock().unwrap().push(table.to_owned());
            if let Some(page) = self.select_pages.lock().unwrap().remove(table) {
                return Ok(page);
            }
            let responses = self.select_responses.lock().unwrap();
            Ok(responses.get(table).cloned().unwrap_or_default())
        }

        async fn upsert_json(
            &self,
            table: &str,
            payload: &Value,
            _conflict_key: &str,
            _token: &str,
        ) -> Result<(), SyncError> {
            if let Some((status, message)) = self.next_upsert_error.lock().unwrap().take() {
                return Err(SyncError::Api { status, message });
            }
            self.upserted
                .lock()
                .unwrap()
                .push((table.to_string(), payload.clone()));
            Ok(())
        }

        async fn patch_json(
            &self,
            table: &str,
            _filter: &str,
            payload: &Value,
            _token: &str,
        ) -> Result<(), SyncError> {
            self.upserted
                .lock()
                .unwrap()
                .push((table.to_string(), payload.clone()));
            Ok(())
        }

        async fn upload_file(
            &self,
            _bucket: &str,
            path: &str,
            _bytes: Vec<u8>,
            _content_type: &str,
            _token: &str,
        ) -> Result<String, SyncError> {
            Ok(format!("bucket/{path}"))
        }

        async fn download_file(
            &self,
            _bucket: &str,
            _path: &str,
            _token: &str,
        ) -> Result<Vec<u8>, SyncError> {
            Ok(vec![0u8; 100])
        }
    }

    // ========================================================================
    // Test database helper
    // ========================================================================

    /// Create an in-memory SQLite pool with the sync engine tables.
    async fn test_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true)
            .foreign_keys(false); // Disable FKs for test isolation

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("Failed to create in-memory pool");

        // Create the sync engine tables
        sqlx::query(
            "CREATE TABLE pending_ops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                principal_key TEXT NOT NULL,
                op_type TEXT NOT NULL,
                table_name TEXT NOT NULL,
                record_id TEXT NOT NULL,
                payload_json TEXT,
                conflict_key TEXT NOT NULL DEFAULT 'id',
                attempts INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE UNIQUE INDEX idx_pending_ops_dedup
             ON pending_ops(principal_key, table_name, record_id, op_type)",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE sync_state (
                uid TEXT NOT NULL,
                table_name TEXT NOT NULL,
                last_pulled_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z',
                PRIMARY KEY (uid, table_name)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        crate::database::local::auth::initialize_auth_state_schema(&pool)
            .await
            .unwrap();

        sqlx::query(
            "CREATE TABLE auth_write_admission (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                armed INTEGER NOT NULL DEFAULT 0 CHECK (armed IN (0, 1)),
                accepting INTEGER NOT NULL DEFAULT 0 CHECK (accepting IN (0, 1)),
                maintenance INTEGER NOT NULL DEFAULT 0 CHECK (maintenance IN (0, 1)),
                remote_writes INTEGER NOT NULL DEFAULT 0 CHECK (remote_writes IN (0, 1)),
                active_uid TEXT,
                generation INTEGER NOT NULL DEFAULT 0,
                CHECK (maintenance = 0 OR (accepting = 0 AND remote_writes = 0)),
                CHECK (remote_writes = 0 OR (accepting = 1 AND maintenance = 0 AND active_uid IS NOT NULL))
             );
             INSERT INTO auth_write_admission (singleton) VALUES (1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create minimal venue/membership tables for pull tests
        sqlx::query(
            "CREATE TABLE venues (
                id TEXT PRIMARY KEY,
                uid TEXT,
                name TEXT NOT NULL,
                description TEXT,
                share_code TEXT,
                role TEXT NOT NULL DEFAULT 'owner',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                version INTEGER NOT NULL DEFAULT 1,
                synced_at TEXT,
                origin TEXT NOT NULL DEFAULT 'local'
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE venue_memberships (
                venue_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'member',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (venue_id, user_id)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    async fn migrated_pool() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("sync.db");
        let migrate_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .unwrap();
        migrate_pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        (directory, pool)
    }

    async fn authenticate(pool: &SqlitePool, uid: &str) {
        crate::database::local::auth::install_test_principal(pool, uid)
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(pool, Some(uid))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn archive_receipt_converges_a_losing_device_timestamp() {
        let (_directory, pool) = migrated_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("archive-owner"))
            .await
            .unwrap();
        let document =
            NewAuthoredDocument::track_score("signed-in:archive-owner", "track", "venue", "score")
                .unwrap();
        let store = AuthoredRevisionStore;
        let mut connection = pool.acquire().await.unwrap();
        store
            .insert_document(&mut connection, &document)
            .await
            .unwrap();
        let files = std::collections::BTreeMap::from([(
            "score.luma".to_owned(),
            b"version = 1\n".to_vec(),
        )]);
        let root = store
            .insert_revision(
                &mut connection,
                &document.id,
                &[],
                &files,
                &RevisionMetadata {
                    operation_kind: "initial_import".into(),
                    operation_id: None,
                    message: "Import".into(),
                    actor: Actor::user(),
                    author_name: "Luma".into(),
                    author_email: "test@luma.local".into(),
                    authored_at: "2026-08-02T00:00:00Z".into(),
                    thread_id: None,
                    assistant_message_id: None,
                    restored_revision_id: None,
                },
            )
            .await
            .unwrap();
        store
            .create_head(&mut connection, &document.id, &root.id)
            .await
            .unwrap();
        let losing_timestamp = "2026-08-02T00:00:02Z";
        store
            .archive_document(&mut connection, &document.id, &root.id, losing_timestamp)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO authored_document_archives
             (archive_id, principal_key, document_id, device_id, operation_id,
              requested_revision_id, archived_at)
             VALUES ('archive-b', 'signed-in:archive-owner', ?, 'device-b',
                     'operation-b', ?, ?)",
        )
        .bind(document.id.as_str())
        .bind(root.id.as_str())
        .bind(losing_timestamp)
        .execute(&mut *connection)
        .await
        .unwrap();
        drop(connection);

        let input = ArchiveAuthoredDocumentInput {
            archive_id: "archive-b".into(),
            document_id: document.id.to_string(),
            device_id: "device-b".into(),
            operation_id: "operation-b".into(),
            requested_revision_id: Some(root.id.to_string()),
            archived_at: losing_timestamp.into(),
        };
        let canonical_timestamp = "2026-08-02T00:00:01Z";
        let receipt = ArchiveAuthoredDocumentReceipt {
            archive_id: input.archive_id.clone(),
            document_id: input.document_id.clone(),
            status: "already_archived".into(),
            final_revision_id: Some(root.id.to_string()),
            cancelled_proposal_count: 0,
            archive_seq: 41,
            document_archived_at: canonical_timestamp.into(),
        };
        push::apply_archive_receipt(&pool, "archive-owner", &input, &receipt)
            .await
            .unwrap();

        let archived_at: String =
            sqlx::query_scalar("SELECT archived_at FROM authored_documents WHERE document_id = ?")
                .bind(document.id.as_str())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(archived_at, canonical_timestamp);
        let stored: (Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT final_revision_id, server_archive_seq
             FROM authored_document_archives WHERE archive_id = 'archive-b'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, (Some(root.id.to_string()), Some(41)));
    }

    #[tokio::test]
    async fn headless_archive_receipt_does_not_wait_for_an_unpulled_server_head() {
        let (_directory, pool) = migrated_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("archive-owner"))
            .await
            .unwrap();
        let document = NewAuthoredDocument::pattern_graph(
            "signed-in:archive-owner",
            "pattern",
            "implementation",
        )
        .unwrap();
        let store = AuthoredRevisionStore;
        let mut connection = pool.acquire().await.unwrap();
        store
            .insert_document(&mut connection, &document)
            .await
            .unwrap();
        sqlx::query("UPDATE authored_documents SET archived_at = ? WHERE document_id = ?")
            .bind("2026-08-02T00:00:02Z")
            .bind(document.id.as_str())
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO authored_document_archives
             (archive_id, principal_key, document_id, device_id, operation_id,
              requested_revision_id, archived_at)
             VALUES ('headless-archive', 'signed-in:archive-owner', ?, 'device-b',
                     'headless-operation', NULL, '2026-08-02T00:00:02Z')",
        )
        .bind(document.id.as_str())
        .execute(&mut *connection)
        .await
        .unwrap();
        drop(connection);

        let input = ArchiveAuthoredDocumentInput {
            archive_id: "headless-archive".into(),
            document_id: document.id.to_string(),
            device_id: "device-b".into(),
            operation_id: "headless-operation".into(),
            requested_revision_id: None,
            archived_at: "2026-08-02T00:00:02Z".into(),
        };
        let receipt = ArchiveAuthoredDocumentReceipt {
            archive_id: input.archive_id.clone(),
            document_id: input.document_id.clone(),
            status: "already_archived".into(),
            final_revision_id: Some(format!("rv-{}", "b".repeat(64))),
            cancelled_proposal_count: 0,
            archive_seq: 42,
            document_archived_at: "2026-08-02T00:00:01Z".into(),
        };
        push::apply_archive_receipt(&pool, "archive-owner", &input, &receipt)
            .await
            .unwrap();
        // A lost IPC response is also safe: the server sequence proves the
        // terminal receipt while topological pull hydrates its final revision.
        push::apply_archive_receipt(&pool, "archive-owner", &input, &receipt)
            .await
            .unwrap();

        let stored: (Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT final_revision_id, server_archive_seq
             FROM authored_document_archives WHERE archive_id = 'headless-archive'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, (None, Some(42)));
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT archived_at FROM authored_documents WHERE document_id = ?",
            )
            .bind(document.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap(),
            "2026-08-02T00:00:01Z"
        );
    }

    #[tokio::test]
    async fn accepted_append_reprojects_head_after_pull_before_push() {
        let (_directory, pool) = migrated_pool().await;
        crate::database::local::auth::initialize_auth_state_schema(&pool)
            .await
            .unwrap();
        authenticate(&pool, "alice").await;
        let thread = crate::database::local::agent_threads::create_thread(
            &pool,
            CreateAgentThreadInput {
                request_id: "thread-request".into(),
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
        let appended = crate::database::local::agent_threads::append_messages(
            &pool,
            &thread.id,
            AppendAgentThreadMessagesInput {
                operation_id: "local-append".into(),
                expected_head_message_id: None,
                messages: vec![NewAgentThreadMessage {
                    id: Some("local-tip".into()),
                    role: "user".into(),
                    parts: json!([{"type": "text", "text": "local"}]),
                }],
            },
            Some("alice"),
        )
        .await
        .unwrap();
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].id, "local-tip");

        // Another device's sibling wins a pull while this device's immutable
        // append receipt remains queued from the old empty base.
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut transaction)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO agent_thread_messages
             (id, owner_user_id, principal_key, created_in_thread_id,
              parent_message_id, depth, role, parts_json, created_at)
             VALUES ('remote-tip', 'alice', 'signed-in:alice', ?, NULL, 0,
                     'user', '[]', '2026-08-02T00:00:01Z')",
        )
        .bind(&thread.id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        pull::apply_agent_transcript_head_observation(
            &pool,
            &json!({
                "thread_id": thread.id.clone(),
                "owner_user_id": "alice",
                "head_message_id": "remote-tip",
                "message_count": 1,
                "updated_at": "2026-08-02T00:00:01Z"
            }),
            "alice",
            &thread.id,
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT head_message_id FROM agent_thread_transcript_heads WHERE thread_id = ?",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "remote-tip"
        );

        // The server accepts the queued stale-base receipt in commit order and
        // returns its current projection. Push must materialize that answer
        // before considering the immutable receipt durably delivered.
        let remote = MockRemoteClient::new();
        assert_eq!(push::flush_pending(&pool, &pool, &remote).await.unwrap(), 2);
        assert_eq!(pending::count_pending(&pool).await.unwrap(), 1);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT head_message_id FROM agent_thread_transcript_heads WHERE thread_id = ?",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "remote-tip",
            "an accepted receipt is not dequeued until its server head is observed"
        );
        let failed = pending::list_failed(&pool).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].table_name, "agent_thread_message_appends");
        pending::reset_retry(&pool, failed[0].id).await.unwrap();

        // Exact immutable replay is accepted after response loss. The current
        // server head then closes the local projection before dequeue.
        remote.on_select(
            "agent_thread_transcript_heads",
            vec![json!({
                "thread_id": thread.id.clone(),
                "owner_user_id": "alice",
                "head_message_id": "local-tip",
                "message_count": 1,
                "updated_at": "2026-08-02T00:00:02.345Z"
            })],
        );
        assert_eq!(push::flush_pending(&pool, &pool, &remote).await.unwrap(), 1);
        assert_eq!(pending::count_pending(&pool).await.unwrap(), 0);
        assert!(remote
            .selected_tables()
            .iter()
            .any(|table| table == "agent_thread_transcript_heads"));
        assert_eq!(
            sqlx::query_as::<_, (Option<String>, i64, String)>(
                "SELECT head_message_id, message_count, updated_at
                 FROM agent_thread_transcript_heads WHERE thread_id = ?",
            )
            .bind(&thread.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            (
                Some("local-tip".into()),
                1,
                "2026-08-02T00:00:02.345Z".into()
            )
        );
    }

    // ========================================================================
    // Pending ops tests
    // ========================================================================

    #[tokio::test]
    async fn test_enqueue_upsert() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;

        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "abc-123",
            r#"{"id":"abc-123","name":"Test"}"#,
            "id",
        )
        .await
        .unwrap();

        let count = pending::count_pending(&pool).await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn live_authored_projections_cannot_enter_the_row_sync_push_path() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;

        for (table, record_id, conflict_key) in [
            ("implementations", "implementation", "id"),
            ("track_scores", "clip", "id"),
            (
                "venue_implementation_overrides",
                "venue:pattern",
                "venue_id,pattern_id",
            ),
        ] {
            let error = pending::enqueue_upsert(
                &pool,
                "u-1",
                table,
                record_id,
                r#"{"implementation_id":"stale"}"#,
                conflict_key,
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("not registered"));
        }
        assert_eq!(pending::count_pending(&pool).await.unwrap(), 0);

        // Even a residual row written by an older binary or recovered backup
        // is rejected before any remote call.
        sqlx::query(
            "INSERT INTO pending_ops
             (principal_key, op_type, table_name, record_id, payload_json, conflict_key)
             VALUES
               ('signed-in:u-1', 'upsert', 'implementations', 'legacy-implementation',
                '{\"id\":\"legacy-implementation\",\"graph_json\":\"stale\"}', 'id'),
               ('signed-in:u-1', 'upsert', 'track_scores', 'legacy-clip',
                '{\"id\":\"legacy-clip\",\"args_json\":\"{}\"}', 'id'),
               ('signed-in:u-1', 'delete', 'venue_implementation_overrides', 'venue:pattern',
                NULL, 'venue_id,pattern_id')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let remote = MockRemoteClient::new();
        assert_eq!(push::flush_pending(&pool, &pool, &remote).await.unwrap(), 0);
        assert_eq!(remote.upsert_count(), 0);
        let failed = pending::list_failed(&pool).await.unwrap();
        assert_eq!(failed.len(), 3);
        assert!(failed.iter().all(|op| op
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("not registered"))));
    }

    /// A remote fixture whose footprint the local trigger would refuse must not
    /// wedge the table.
    ///
    /// Before the repair, the trigger aborted the upsert, `stopped_at_failure`
    /// broke out of the page loop with the cursor still on the previous row,
    /// and every table with `fixtures` as a parent was deferred — permanently,
    /// because the same row came back on every subsequent pull.
    #[tokio::test]
    async fn a_fixture_the_local_trigger_would_refuse_is_repaired_rather_than_wedging_the_pull() {
        let (_directory, pool) = migrated_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("u-1"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('v-1', 'u-1', 'Room')")
            .execute(&pool)
            .await
            .unwrap();

        let fixture = |id: &str, address: i64, channels: i64, seq: i64| {
            json!({
                "id": id,
                "uid": "u-1",
                "venue_id": "v-1",
                "universe": 1,
                "address": address,
                "num_channels": channels,
                "manufacturer": "Acme",
                "model": "Mover",
                "mode_name": "16ch",
                "fixture_path": "acme/mover.qxf",
                "address_pinned": 0,
                "label": null,
                "pos_x": 0.0, "pos_y": 0.0, "pos_z": 0.0,
                "rot_x": 0.0, "rot_y": 0.0, "rot_z": 0.0,
                "created_at": "2026-08-01T00:00:00Z",
                "updated_at": "2026-08-01T00:00:00Z",
                "sync_seq": seq,
            })
        };

        let authored_directory = tempfile::tempdir().unwrap();
        let authored = crate::services::authored_documents::AuthoredDocuments::new(
            crate::storage::StorageRoot::from_path(authored_directory.path().join("authored")),
        );
        let workspaces = crate::agent_execution::PythonWorkspaceService::new(
            authored_directory.path().join("python-workspaces"),
            std::sync::Arc::new(|| Err("python is not used by this sync test".into())),
        );
        let graph_runs = crate::agent_execution::GraphRunStore::new();
        let subagents = crate::agent::subagent::SubagentRegistry::default();
        let remote = MockRemoteClient::new();
        // One page: a sound row; one whose 32 channels run off the end of the
        // universe; one whose *width alone* is wider than a universe, which no
        // address can rescue; and a sound row after them all — the one the old
        // break would never have reached.
        remote.on_select_page(
            "fixtures",
            vec![
                fixture("ok-1", 1, 16, 1),
                fixture("broken", 500, 32, 2),
                fixture("too-wide", 1, 600, 3),
                fixture("ok-2", 100, 16, 4),
            ],
        );

        let stats = pull::pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &subagents,
            &remote,
            "token",
            Some("u-1"),
        )
        .await
        .unwrap();

        assert!(
            !stats.errors.iter().any(|error| error.contains("fixtures")),
            "fixtures did not finish: {:?}",
            stats.errors
        );
        assert!(
            !stats
                .errors
                .iter()
                .any(|error| error.contains("deferred because dependency fixtures")),
            "a table downstream of fixtures was deferred: {:?}",
            stats.errors
        );

        let rows: Vec<(String, i64, i64, Option<String>)> =
            sqlx::query_as("SELECT id, address, num_channels, synced_at FROM fixtures ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let by_id = |id: &str| {
            rows.iter()
                .find(|row| row.0 == id)
                .unwrap_or_else(|| panic!("{id} landed"))
                .clone()
        };

        // The row after the broken one arrived, which is the wedge this test
        // is about.
        assert_eq!(by_id("ok-2").1, 100);
        // A sound row is untouched and counts as synced.
        assert_eq!(by_id("ok-1").1, 1);
        assert!(by_id("ok-1").3.is_some());
        // The broken one landed, repaired the way the migration repairs local
        // rows, and is dirty so the repair pushes back.
        let broken = by_id("broken");
        assert_eq!(
            broken.2, 32,
            "the width is the fixture's, not ours to change"
        );
        assert_eq!(broken.1, 1, "an unaddressable footprint moves to channel 1");
        assert!(
            broken.3.is_none(),
            "the repair has to be pushed back, so the row must be dirty"
        );

        // The too-wide one is the case moving the address cannot fix: the
        // trigger's condition holds at every address once the width is over
        // 512, so the width itself has to come down or the pull wedges.
        let wide = by_id("too-wide");
        assert_eq!(wide.2, 512, "a width no universe can hold is clamped");
        assert_eq!(wide.1, 1);
        assert!(wide.3.is_none());

        // And the cursor advanced past the whole page, so the next pull does
        // not replay it.
        let cursor = state::get_last_pulled_seq(&pool, "u-1", "fixtures")
            .await
            .unwrap();
        assert_eq!(cursor, 4);
    }

    /// The pin travels with the address it is a pin on.
    ///
    /// A had this fixture pinned at 1/100; B unpinned it and auto-patched it to
    /// 3/17. Both halves of the footprint are registry-driven now, so A's pull
    /// takes the address *and* the pin. Leaving the pin behind was how A came to
    /// hold a *pinned* 3/17 — a hand-chosen address nobody chose, which no
    /// auto-patch would ever move again.
    #[tokio::test]
    async fn a_pulled_address_brings_the_pushers_pin_with_it() {
        let (_directory, pool) = migrated_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("u-1"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('v-1', 'u-1', 'Room')")
            .execute(&pool)
            .await
            .unwrap();
        // Clean (`synced_at` set), so the pull is allowed to overwrite it.
        sqlx::query(
            "INSERT INTO fixtures
               (id, uid, venue_id, universe, address, num_channels, manufacturer, model,
                mode_name, fixture_path, address_pinned, synced_at)
             VALUES ('f-1', 'u-1', 'v-1', 1, 100, 16, 'Acme', 'Mover', '16ch',
                'acme/mover.qxf', 1, '2026-08-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let authored_directory = tempfile::tempdir().unwrap();
        let authored = crate::services::authored_documents::AuthoredDocuments::new(
            crate::storage::StorageRoot::from_path(authored_directory.path().join("authored")),
        );
        let workspaces = crate::agent_execution::PythonWorkspaceService::new(
            authored_directory.path().join("python-workspaces"),
            std::sync::Arc::new(|| Err("python is not used by this sync test".into())),
        );
        let graph_runs = crate::agent_execution::GraphRunStore::new();
        let subagents = crate::agent::subagent::SubagentRegistry::default();
        let remote = MockRemoteClient::new();
        remote.on_select_page(
            "fixtures",
            vec![json!({
                "id": "f-1",
                "uid": "u-1",
                "venue_id": "v-1",
                "universe": 3,
                "address": 17,
                "num_channels": 16,
                "manufacturer": "Acme",
                "model": "Mover",
                "mode_name": "16ch",
                "fixture_path": "acme/mover.qxf",
                "address_pinned": 0,
                "label": null,
                "pos_x": 0.0, "pos_y": 0.0, "pos_z": 0.0,
                "rot_x": 0.0, "rot_y": 0.0, "rot_z": 0.0,
                "created_at": "2026-08-01T00:00:00Z",
                "updated_at": "2026-08-02T00:00:00Z",
                "sync_seq": 1,
            })],
        );

        pull::pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &subagents,
            &remote,
            "token",
            Some("u-1"),
        )
        .await
        .unwrap();

        let (universe, address, pinned): (i64, i64, i64) = sqlx::query_as(
            "SELECT universe, address, address_pinned FROM fixtures WHERE id = 'f-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (universe, address),
            (3, 17),
            "the address is registry-driven, so the remote's wins"
        );
        assert_eq!(
            pinned, 0,
            "and the pin, being registry-driven too, comes with it"
        );
    }

    #[tokio::test]
    async fn pull_never_requests_authored_relational_blobs() {
        let pool = test_pool().await;
        let authored_directory = tempfile::tempdir().unwrap();
        let authored = crate::services::authored_documents::AuthoredDocuments::new(
            crate::storage::StorageRoot::from_path(authored_directory.path().join("authored")),
        );
        let workspaces = crate::agent_execution::PythonWorkspaceService::new(
            authored_directory.path().join("python-workspaces"),
            std::sync::Arc::new(|| Err("python is not used by this sync test".into())),
        );
        let graph_runs = crate::agent_execution::GraphRunStore::new();
        let subagents = crate::agent::subagent::SubagentRegistry::default();
        let remote = MockRemoteClient::new();
        remote.on_select(
            "implementations",
            vec![json!({
                "id": "remote-implementation",
                "graph_json": "newer remote graph"
            })],
        );
        remote.on_select(
            "track_scores",
            vec![json!({
                "id": "remote-clip",
                "args_json": "{\"newer\":true}"
            })],
        );
        remote.on_select(
            "venue_implementation_overrides",
            vec![json!({
                "venue_id": "venue",
                "pattern_id": "pattern",
                "implementation_id": "stale-implementation"
            })],
        );

        pull::pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &subagents,
            &remote,
            "token",
            Some("u-1"),
        )
        .await
        .unwrap();
        let selected = remote.selected_tables();
        for table in [
            "implementations",
            "track_scores",
            "venue_implementation_overrides",
        ] {
            assert!(!selected.iter().any(|selected| selected == table));
            assert!(registry::get_table(table).is_none());
        }
    }

    #[tokio::test]
    async fn test_enqueue_deduplication() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;

        // Enqueue twice for the same record
        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "abc-123",
            r#"{"id":"abc-123","name":"First"}"#,
            "id",
        )
        .await
        .unwrap();

        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "abc-123",
            r#"{"id":"abc-123","name":"Second"}"#,
            "id",
        )
        .await
        .unwrap();

        // Should still be only 1 op (deduplicated)
        let count = pending::count_pending(&pool).await.unwrap();
        assert_eq!(count, 1);

        // And it should have the latest payload
        let ops = pending::fetch_ready_ops(&pool, "signed-in:u-1")
            .await
            .unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].payload_json.as_ref().unwrap().contains("Second"));
    }

    #[tokio::test]
    async fn test_retry_backoff() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;

        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "abc-123",
            r#"{"id":"abc-123"}"#,
            "id",
        )
        .await
        .unwrap();

        let ops = pending::fetch_ready_ops(&pool, "signed-in:u-1")
            .await
            .unwrap();
        assert_eq!(ops.len(), 1);

        // Record a failure
        pending::record_failure(&pool, &ops[0], 1, "timeout")
            .await
            .unwrap();

        // Op should NOT be ready immediately (backoff pushed next_retry_at forward)
        let ready = pending::fetch_ready_ops(&pool, "signed-in:u-1")
            .await
            .unwrap();
        assert_eq!(ready.len(), 0);

        // But it should appear in the failed list
        let failed = pending::list_failed(&pool).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].attempts, 1);
        assert_eq!(failed[0].last_error.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn test_reset_retry() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;

        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "abc-123",
            r#"{"id":"abc-123"}"#,
            "id",
        )
        .await
        .unwrap();

        let ops = pending::fetch_ready_ops(&pool, "signed-in:u-1")
            .await
            .unwrap();
        pending::record_failure(&pool, &ops[0], 1, "error")
            .await
            .unwrap();

        // Not ready after failure
        assert_eq!(
            pending::fetch_ready_ops(&pool, "signed-in:u-1")
                .await
                .unwrap()
                .len(),
            0
        );

        // Reset retry
        pending::reset_retry(&pool, ops[0].id).await.unwrap();

        // Now it should be ready again
        assert_eq!(
            pending::fetch_ready_ops(&pool, "signed-in:u-1")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn signed_out_tombstone_never_flushes_and_survives_sign_in() {
        let (_directory, pool) = migrated_pool().await;
        crate::database::local::auth::initialize_auth_state_schema(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, name) VALUES ('guest-venue', 'Guest')")
            .execute(&pool)
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        sqlx::query("DELETE FROM venues WHERE id = 'guest-venue'")
            .execute(&pool)
            .await
            .unwrap();

        let queued_principal: String = sqlx::query_scalar(
            "SELECT principal_key FROM pending_ops
             WHERE table_name = 'venues' AND record_id = 'guest-venue'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            queued_principal,
            crate::database::local::auth::SIGNED_OUT_PRINCIPAL_KEY
        );

        let remote = MockRemoteClient::new();
        assert!(matches!(
            push::flush_pending(&pool, &pool, &remote).await,
            Err(SyncError::AuthRequired)
        ));
        authenticate(&pool, "alice").await;
        assert_eq!(push::flush_pending(&pool, &pool, &remote).await.unwrap(), 0);
        assert_eq!(remote.upsert_count(), 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE principal_key = 'signed-out'
                   AND table_name = 'venues' AND record_id = 'guest-venue'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn pending_queue_migration_preserves_and_principalizes_legacy_rows() {
        let (_directory, pool) = migrated_pool().await;
        sqlx::raw_sql(
            "DROP TRIGGER IF EXISTS sync_delete_venues;
             DROP TRIGGER IF EXISTS sync_delete_tracks;
             DROP TRIGGER IF EXISTS sync_delete_pattern_categories;
             DROP TRIGGER IF EXISTS sync_delete_fixtures;
             DROP TRIGGER IF EXISTS sync_delete_patterns;
             DROP TRIGGER IF EXISTS sync_delete_fixture_groups;
             DROP TRIGGER IF EXISTS sync_delete_scores;
             DROP TRIGGER IF EXISTS sync_delete_fixture_group_members;
             DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
             DROP TRIGGER IF EXISTS sync_delete_cues;
             DROP TRIGGER IF EXISTS sync_delete_midi_bindings;
             DROP TRIGGER IF EXISTS sync_delete_venue_nodes;
             DROP TRIGGER IF EXISTS sync_delete_venue_edges;
             DROP TRIGGER IF EXISTS sync_delete_venue_node_params;
             DROP TRIGGER IF EXISTS sync_delete_venue_constraints;
             DROP TABLE pending_ops;
             CREATE TABLE pending_ops (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 op_type TEXT NOT NULL CHECK(op_type IN ('upsert', 'delete')),
                 table_name TEXT NOT NULL,
                 record_id TEXT NOT NULL,
                 payload_json TEXT,
                 conflict_key TEXT NOT NULL DEFAULT 'id',
                 attempts INTEGER NOT NULL DEFAULT 0,
                 last_error TEXT,
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );
             CREATE INDEX idx_pending_ops_next_retry ON pending_ops(next_retry_at);
             CREATE INDEX idx_pending_ops_table_record ON pending_ops(table_name, record_id);
             CREATE UNIQUE INDEX idx_pending_ops_dedup
                 ON pending_ops(table_name, record_id, op_type);
             CREATE TRIGGER sync_delete_venues AFTER DELETE ON venues FOR EACH ROW
             WHEN OLD.origin = 'local'
             BEGIN
                 INSERT OR REPLACE INTO pending_ops
                     (op_type, table_name, record_id, next_retry_at)
                 VALUES ('delete', 'venues', OLD.id, CURRENT_TIMESTAMP);
             END;",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO pending_ops
                (op_type, table_name, record_id, payload_json, conflict_key)
             VALUES
                ('upsert', 'venues', 'owned',
                 '{\"id\":\"owned\",\"uid\":\"alice\"}', 'id'),
                ('upsert', 'venues', 'signed-out',
                 '{\"id\":\"signed-out\",\"uid\":null}', 'id'),
                ('delete', 'venues', 'tombstone', NULL, 'id')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../migrations/20260802930000_pending_ops_principal.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();

        let principals: Vec<(String, String)> =
            sqlx::query_as("SELECT record_id, principal_key FROM pending_ops ORDER BY record_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            principals,
            vec![
                ("owned".into(), "signed-in:alice".into()),
                ("signed-out".into(), "signed-out".into()),
                ("tombstone".into(), "signed-in:alice".into()),
            ]
        );

        sqlx::query(
            "INSERT INTO pending_ops (principal_key, op_type, table_name, record_id)
             VALUES ('signed-in:bob', 'delete', 'venues', 'tombstone')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops
                 WHERE table_name = 'venues' AND record_id = 'tombstone'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn queue_reads_and_manual_transitions_are_scoped_to_alice() {
        let pool = test_pool().await;
        authenticate(&pool, "alice").await;
        sqlx::query(
            "INSERT INTO pending_ops
                (principal_key, op_type, table_name, record_id, payload_json, attempts, last_error)
             VALUES
                ('signed-in:alice', 'upsert', 'venues', 'alice-row',
                 '{\"id\":\"alice-row\",\"uid\":\"alice\"}', 0, NULL),
                ('signed-in:bob', 'upsert', 'venues', 'bob-row',
                 '{\"id\":\"bob-row\",\"uid\":\"bob\"}', 1, 'bob failure')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let alice_key = crate::database::local::auth::principal_key(Some("alice"));
        let ready = pending::fetch_ready_ops(&pool, &alice_key).await.unwrap();
        assert_eq!(
            ready
                .iter()
                .map(|operation| operation.record_id.as_str())
                .collect::<Vec<_>>(),
            vec!["alice-row"]
        );
        assert_eq!(pending::count_pending(&pool).await.unwrap(), 1);
        assert!(pending::list_failed(&pool).await.unwrap().is_empty());

        let bob_id: i64 = sqlx::query_scalar(
            "SELECT id FROM pending_ops
             WHERE principal_key = 'signed-in:bob' AND record_id = 'bob-row'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(pending::reset_retry(&pool, bob_id).await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT attempts FROM pending_ops WHERE id = ?")
                .bind(bob_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn alice_signout_ignores_other_queues_but_refuses_her_own() {
        let (directory, pool) = migrated_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO pending_ops (principal_key, op_type, table_name, record_id)
             VALUES
                ('signed-out', 'delete', 'venues', 'guest-row'),
                ('signed-in:bob', 'delete', 'venues', 'bob-row')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let authored = crate::services::authored_documents::AuthoredDocuments::new(
            crate::storage::StorageRoot::from_path(directory.path().join("authored")),
        );

        crate::dispatch::handlers::auth::wipe_database_pool(&pool, &authored, "alice")
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pending_ops")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );

        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO pending_ops (principal_key, op_type, table_name, record_id)
             VALUES ('signed-in:alice', 'delete', 'venues', 'alice-row')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let error = crate::dispatch::handlers::auth::wipe_database_pool(&pool, &authored, "alice")
            .await
            .unwrap_err();
        assert!(error.contains("1 operation(s)"), "{error}");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops WHERE principal_key = 'signed-in:alice'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
    }

    // ========================================================================
    // Sync state tests
    // ========================================================================

    #[tokio::test]
    async fn test_sync_state_defaults_to_zero() {
        let pool = test_pool().await;

        let sequence = state::get_last_pulled_seq(&pool, "test-uid", "venues")
            .await
            .unwrap();
        assert_eq!(sequence, 0);
    }

    #[tokio::test]
    async fn test_sync_state_set_and_get() {
        let pool = test_pool().await;

        state::advance_last_pulled_seq(&pool, "test-uid", "venues", 42)
            .await
            .unwrap();

        let sequence = state::get_last_pulled_seq(&pool, "test-uid", "venues")
            .await
            .unwrap();
        assert_eq!(sequence, 42);
    }

    #[tokio::test]
    async fn sync_state_replays_and_concurrent_advances_are_monotonic() {
        let pool = test_pool().await;

        state::advance_last_pulled_seq(&pool, "test-uid", "venues", 100)
            .await
            .unwrap();
        // A delayed worker replaying an older page must not move the durable
        // cursor backward.
        state::advance_last_pulled_seq(&pool, "test-uid", "venues", 7)
            .await
            .unwrap();

        let (lower, higher) = tokio::join!(
            state::advance_last_pulled_seq(&pool, "test-uid", "venues", 90),
            state::advance_last_pulled_seq(&pool, "test-uid", "venues", 140),
        );
        lower.unwrap();
        higher.unwrap();
        // An exact response-loss replay is harmless too.
        state::advance_last_pulled_seq(&pool, "test-uid", "venues", 140)
            .await
            .unwrap();

        assert_eq!(
            state::get_last_pulled_seq(&pool, "test-uid", "venues")
                .await
                .unwrap(),
            140
        );
    }

    // ========================================================================
    // Pull: discovery tests
    // ========================================================================

    #[tokio::test]
    async fn test_discover_owned_venues() {
        let pool = test_pool().await;
        authenticate(&pool, "user-1").await;
        let mock = MockRemoteClient::new();

        // Remote has one owned venue
        mock.on_select(
            "venues",
            vec![json!({
                "id": "v-owned-1",
                "uid": "user-1",
                "name": "My Venue",
                "description": null,
                "share_code": null,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-03-28T00:00:00Z"
            })],
        );
        mock.on_select("venue_members", vec![]);

        let venue_ids = pull::discover_venues(&pool, &mock, "user-1", "fake-token")
            .await
            .unwrap();

        assert_eq!(venue_ids, vec!["v-owned-1"]);

        // Verify it was inserted locally
        let local: Option<String> =
            sqlx::query_scalar("SELECT name FROM venues WHERE id = 'v-owned-1'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert_eq!(local, Some("My Venue".to_string()));

        // Verify role is owner
        let role: String = sqlx::query_scalar("SELECT role FROM venues WHERE id = 'v-owned-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(role, "owner");
    }

    #[tokio::test]
    async fn discovering_new_owned_visibility_replays_server_cursors_once() {
        let pool = test_pool().await;
        authenticate(&pool, "user-1").await;
        state::advance_last_pulled_seq(&pool, "user-1", "fixtures", 88)
            .await
            .unwrap();
        let mock = MockRemoteClient::new();
        mock.on_select(
            "venues",
            vec![json!({
                "id": "new-owned",
                "uid": "user-1",
                "name": "New visibility",
                "description": null,
                "share_code": null,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-08-02T00:00:00Z"
            })],
        );
        mock.on_select("venue_members", vec![]);

        pull::discover_venues(&pool, &mock, "user-1", "token")
            .await
            .unwrap();
        assert_eq!(
            state::get_last_pulled_seq(&pool, "user-1", "fixtures")
                .await
                .unwrap(),
            0
        );

        state::advance_last_pulled_seq(&pool, "user-1", "fixtures", 99)
            .await
            .unwrap();
        pull::discover_venues(&pool, &mock, "user-1", "token")
            .await
            .unwrap();
        assert_eq!(
            state::get_last_pulled_seq(&pool, "user-1", "fixtures")
                .await
                .unwrap(),
            99,
            "stable visibility must retain its commit-ordered cursor"
        );
    }

    #[tokio::test]
    async fn test_discover_joined_venues() {
        let pool = test_pool().await;
        authenticate(&pool, "user-1").await;
        let mock = MockRemoteClient::new();

        // User owns no venues
        mock.on_select(
            "venues",
            vec![
                // This will be returned for the "uid=eq.user-1" query (owned)
                // AND for the "id=in.(v-joined-1)" query (joined venue details)
                // Since our mock doesn't distinguish queries, we put the joined
                // venue here — the discovery code handles it either way.
                json!({
                    "id": "v-joined-1",
                    "uid": "owner-uid",
                    "name": "Joined Venue",
                    "description": "A venue I joined",
                    "share_code": "ABC123",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-03-28T00:00:00Z"
                }),
            ],
        );
        // User is a member of one venue
        mock.on_select("venue_members", vec![json!({"venue_id": "v-joined-1"})]);

        let venue_ids = pull::discover_venues(&pool, &mock, "user-1", "fake-token")
            .await
            .unwrap();

        assert!(venue_ids.contains(&"v-joined-1".to_string()));

        // Check it's locally stored
        let exists: bool =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM venues WHERE id = 'v-joined-1'")
                .fetch_one(&pool)
                .await
                .unwrap()
                > 0;
        assert!(exists);
    }

    #[tokio::test]
    async fn test_discover_removes_only_stale_membership_routing() {
        let pool = test_pool().await;
        authenticate(&pool, "user-1").await;
        let mock = MockRemoteClient::new();

        // Insert a member venue locally that no longer exists remotely
        sqlx::query(
            "INSERT INTO venues (id, uid, name, role) VALUES ('v-stale', 'owner-uid', 'Stale', 'member')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venue_memberships (venue_id, user_id, role)
             VALUES ('v-stale', 'user-1', 'member')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Remote returns empty — user has no venues
        mock.on_select("venues", vec![]);
        mock.on_select("venue_members", vec![]);

        let venue_ids = pull::discover_venues(&pool, &mock, "user-1", "fake-token")
            .await
            .unwrap();

        assert!(venue_ids.is_empty());

        // Discovery owns membership routing, not the venue catalog and its
        // potentially durable authored dependents.
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM venue_memberships WHERE venue_id = 'v-stale' AND user_id = 'user-1'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM venues WHERE id = 'v-stale'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    // ========================================================================
    // Push: flush tests
    // ========================================================================

    #[tokio::test]
    async fn test_flush_upsert_op() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();

        // Create a venue locally so mark_synced can find it
        sqlx::query(
            "INSERT INTO venues (id, uid, name, role, updated_at) VALUES ('v-1', 'u-1', 'Test', 'owner', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Enqueue a pending upsert
        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "v-1",
            r#"{"id":"v-1","uid":"u-1","name":"Test"}"#,
            "id",
        )
        .await
        .unwrap();

        assert_eq!(pending::count_pending(&pool).await.unwrap(), 1);

        // We need a state_pool with auth — for this test, we'll call flush
        // directly with the mock, bypassing auth. Let's test execute_op logic
        // by calling the functions individually.

        let ops = pending::fetch_ready_ops(&pool, "signed-in:u-1")
            .await
            .unwrap();
        assert_eq!(ops.len(), 1);

        // Simulate what flush_pending does: execute the op
        let op = &ops[0];
        let payload: Value = serde_json::from_str(op.payload_json.as_ref().unwrap()).unwrap();
        mock.upsert_json("venues", &payload, "id", "fake-token")
            .await
            .unwrap();

        // Remove the op through the same principal-scoped transition as push.
        pending::remove_op(&pool, op).await.unwrap();

        assert_eq!(pending::count_pending(&pool).await.unwrap(), 0);
        assert_eq!(mock.upsert_count(), 1);
    }

    #[tokio::test]
    async fn test_non_fk_conflict_stays_queued_and_dirty() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        mock.fail_next_upsert(
            409,
            r#"{"code":"23505","message":"duplicate key violates unique constraint"}"#,
        );

        sqlx::query(
            "INSERT INTO venues (id, uid, name, role, updated_at)
             VALUES ('v-conflict', 'u-1', 'Local value', 'owner', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "v-conflict",
            r#"{"id":"v-conflict","uid":"u-1","name":"Local value"}"#,
            "id",
        )
        .await
        .unwrap();

        let flushed = push::flush_pending(&pool, &pool, &mock).await.unwrap();

        assert_eq!(flushed, 0);
        assert_eq!(pending::count_pending(&pool).await.unwrap(), 1);
        let failed = pending::list_failed(&pool).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].attempts, 1);
        assert!(failed[0]
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("23505")));
        let synced_at: Option<String> =
            sqlx::query_scalar("SELECT synced_at FROM venues WHERE id = 'v-conflict'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(synced_at, None);
    }

    /// A row this principal may not mark clean is one op's problem, not the
    /// batch's. Before, the refusal escaped the loop with `?`: nothing recorded
    /// a failure, nothing incremented attempts, and every op sorted after it —
    /// thousands, on a real queue — was unreachable for as long as the row
    /// stayed.
    #[tokio::test]
    async fn alice_cannot_mark_bobs_local_row_synced() {
        let pool = test_pool().await;
        authenticate(&pool, "alice").await;
        for (id, uid, name) in [
            ("shared-id", "bob", "Bob value"),
            ("z-alice-id", "alice", "Alice value"),
        ] {
            sqlx::query(
                "INSERT INTO venues (id, uid, name, role, updated_at)
                 VALUES (?, ?, ?, 'owner', '2026-01-01T00:00:00Z')",
            )
            .bind(id)
            .bind(uid)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        }
        for id in ["shared-id", "z-alice-id"] {
            pending::enqueue_upsert(
                &pool,
                "alice",
                "venues",
                id,
                &format!(r#"{{"id":"{id}","uid":"alice","name":"Alice payload"}}"#),
                "id",
            )
            .await
            .unwrap();
        }

        let remote = MockRemoteClient::new();
        let flushed = push::flush_pending(&pool, &pool, &remote).await.unwrap();

        assert_eq!(flushed, 1, "the op behind the refusal still went out");
        assert_eq!(remote.upsert_count(), 2);
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT synced_at FROM venues WHERE id = 'shared-id'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            None
        );
        assert_eq!(pending::count_pending(&pool).await.unwrap(), 1);
        let failed = pending::list_failed(&pool).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].record_id, "shared-id");
        assert_eq!(failed[0].attempts, 1, "a refusal counts toward dead-letter");
        assert!(
            failed[0]
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("not owned")),
            "{:?}",
            failed[0].last_error,
        );
    }

    /// The wedge this all came from: a row created, queued, then deleted 21
    /// seconds later. The tombstone lands under a different `op_type`, so the
    /// queue's uniqueness key cannot retract the upsert in front of it, and
    /// pushing that upsert resurrected the row server-side every cycle while
    /// `mark_synced` refused a receipt no row was left to take.
    #[tokio::test]
    async fn a_queued_upsert_whose_row_is_gone_is_dropped_and_the_batch_continues() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;
        sqlx::query(
            "INSERT INTO venues (id, uid, name, role, updated_at)
             VALUES ('v-live', 'u-1', 'Still here', 'owner', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for id in ["a-v-gone", "v-live"] {
            pending::enqueue_upsert(
                &pool,
                "u-1",
                "venues",
                id,
                &format!(r#"{{"id":"{id}","uid":"u-1","name":"Whatever"}}"#),
                "id",
            )
            .await
            .unwrap();
        }
        // The tombstone the delete trigger would have left behind.
        sqlx::query(
            "INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, conflict_key)
             VALUES ('signed-in:u-1', 'delete', 'venues', 'a-v-gone', 'id')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let remote = MockRemoteClient::new();
        let flushed = push::flush_pending(&pool, &pool, &remote).await.unwrap();

        assert_eq!(flushed, 2, "the tombstone and the live row both went out");
        assert_eq!(pending::count_pending(&pool).await.unwrap(), 0);
        assert!(
            pending::list_failed(&pool).await.unwrap().is_empty(),
            "dropping a superseded upsert is not a failure",
        );
        let pushed = remote.upserted.lock().unwrap().clone();
        assert!(
            !pushed
                .iter()
                .any(|(_, payload)| payload["id"] == json!("a-v-gone")),
            "the deleted row was never resurrected: {pushed:?}",
        );
        assert!(
            pushed
                .iter()
                .any(|(_, payload)| payload.get("deleted_at").is_some()),
            "its tombstone is what reached the remote: {pushed:?}",
        );
    }

    /// Deleting a venue node retracts the upsert still queued for it. The
    /// tombstone the `sync_delete_*` trigger enqueues is the successor state,
    /// and the pair must not cost a remote round trip.
    #[tokio::test]
    async fn deleting_a_row_retracts_the_upsert_still_queued_for_it() {
        let (_directory, pool) = migrated_pool().await;
        crate::database::local::auth::initialize_auth_state_schema(&pool)
            .await
            .unwrap();
        authenticate(&pool, "alice").await;
        seed_venue_graph(&pool).await;
        crate::sync::orchestrator::enqueue_dirty(&pool, "alice")
            .await
            .unwrap();

        sqlx::query("DELETE FROM venue_node_params WHERE node_id = 'run-1' AND key = 'span'")
            .execute(&pool)
            .await
            .unwrap();
        let record_id = format!("run-1{}span", registry::RECORD_ID_SEPARATOR);
        let queued: Vec<String> = sqlx::query_scalar(
            "SELECT op_type FROM pending_ops
             WHERE table_name = 'venue_node_params' AND record_id = ? ORDER BY op_type",
        )
        .bind(&record_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            queued,
            vec!["delete".to_owned(), "upsert".to_owned()],
            "the trigger cannot retract the upsert; the flush has to",
        );

        let remote = MockRemoteClient::new();
        push::flush_pending(&pool, &pool, &remote).await.unwrap();

        let pushed = remote.upserted.lock().unwrap().clone();
        assert!(
            !pushed
                .iter()
                .any(|(table, payload)| table == "venue_node_params"
                    && payload.get("deleted_at").is_none()),
            "the span was never pushed as a live row: {pushed:?}",
        );
        assert!(
            pushed
                .iter()
                .any(|(table, payload)| table == "venue_node_params"
                    && payload.get("deleted_at").is_some()),
            "only its tombstone went out: {pushed:?}",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pending_ops WHERE table_name = 'venue_node_params'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "neither half of the pair is left behind",
        );
    }

    /// The dirty sweep re-offers every unsynced row every ten seconds. If that
    /// zeroed `attempts`, backoff never grew and the dead-letter threshold was
    /// unreachable for catalog rows — the audit's T2.1.
    #[tokio::test]
    async fn re_enqueueing_an_unchanged_payload_keeps_its_retry_state() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;
        let payload = r#"{"id":"abc-123","uid":"u-1","name":"First"}"#;
        pending::enqueue_upsert(&pool, "u-1", "venues", "abc-123", payload, "id")
            .await
            .unwrap();
        let ops = pending::fetch_ready_ops(&pool, "signed-in:u-1")
            .await
            .unwrap();
        pending::record_failure(&pool, &ops[0], 3, "boom")
            .await
            .unwrap();

        pending::enqueue_upsert(&pool, "u-1", "venues", "abc-123", payload, "id")
            .await
            .unwrap();
        let failed = pending::list_failed(&pool).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].attempts, 3, "the same op keeps its counter");
        assert_eq!(failed[0].last_error.as_deref(), Some("boom"));
        assert!(
            pending::fetch_ready_ops(&pool, "signed-in:u-1")
                .await
                .unwrap()
                .is_empty(),
            "and its backoff",
        );

        // New content is a new operation and starts over.
        pending::enqueue_upsert(
            &pool,
            "u-1",
            "venues",
            "abc-123",
            r#"{"id":"abc-123","uid":"u-1","name":"Second"}"#,
            "id",
        )
        .await
        .unwrap();
        assert!(pending::list_failed(&pool).await.unwrap().is_empty());
        assert_eq!(
            pending::fetch_ready_ops(&pool, "signed-in:u-1")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn test_fresh_unsynced_owned_row_is_selected_for_push() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO venues (id, uid, name, role, updated_at)
             VALUES ('v-fresh', 'u-1', 'Fresh', 'owner', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let table = registry::get_table("venues").unwrap();
        let ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(table.dirty_query()))
            .bind("u-1")
            .fetch_all(&pool)
            .await
            .unwrap();

        assert_eq!(ids, vec!["v-fresh"]);
        let synced_at: Option<String> =
            sqlx::query_scalar("SELECT synced_at FROM venues WHERE id = 'v-fresh'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(synced_at, None);
    }

    #[tokio::test]
    async fn test_ops_processed_in_topo_order() {
        let pool = test_pool().await;
        authenticate(&pool, "u-1").await;

        // Enqueue ops in reverse FK-dependency order; fetch_ready_ops should
        // re-sort them by sync registry topological position.
        pending::enqueue_upsert(
            &pool,
            "u-1",
            "fixture_group_members",
            "member-1",
            r#"{"id":"member-1"}"#,
            "id",
        )
        .await
        .unwrap();
        pending::enqueue_upsert(&pool, "u-1", "venues", "v-1", r#"{"id":"v-1"}"#, "id")
            .await
            .unwrap();
        pending::enqueue_upsert(&pool, "u-1", "fixtures", "f-1", r#"{"id":"f-1"}"#, "id")
            .await
            .unwrap();

        let ops = pending::fetch_ready_ops(&pool, "signed-in:u-1")
            .await
            .unwrap();
        assert_eq!(ops.len(), 3);
        // venues has no parents; fixtures depends on venues; group membership
        // depends on fixtures and fixture_groups.
        assert_eq!(ops[0].table_name, "venues");
        assert_eq!(ops[1].table_name, "fixtures");
        assert_eq!(ops[2].table_name, "fixture_group_members");
    }

    /// Where the venue graph's rows live in the repo, so a solve here resolves
    /// the real catalog rather than a stub of it.
    fn fixtures_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/fixtures")
    }

    /// One venue's graph, seeded the way the builder leaves it: a root whose id
    /// is `"<venue>:venue"`, a truss under it, the truss's span, and a far-end
    /// check. Two of the four tables are composite-keyed and the root's id
    /// carries a colon, which is exactly the shape record-id encoding has to
    /// survive.
    async fn seed_venue_graph(pool: &SqlitePool) {
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('v-1', 'alice', 'Room')")
            .execute(pool)
            .await
            .unwrap();
        for (id, kind, catalog_ref, label) in [
            ("v-1:venue", "venue", None, None),
            ("run-1", "run", Some("truss/straight"), Some("Downstage")),
        ] {
            sqlx::query(
                "INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label)
                 VALUES (?, 'alice', 'v-1', ?, ?, ?)",
            )
            .bind(id)
            .bind(kind)
            .bind(catalog_ref)
            .bind(label)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO venue_edges (child_id, uid, parent_id, my_socket, their_socket, roll)
             VALUES ('run-1', 'alice', 'v-1:venue', 'grab', 'floor', 0.0)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venue_node_params (node_id, uid, key, value)
             VALUES ('run-1', 'alice', 'span', 8.0)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venue_constraints (node_id, uid, my_socket, target_node, target_socket)
             VALUES ('run-1', 'alice', 'end_b', 'v-1:venue', 'floor')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// The graph goes out, a second machine's edit comes back, and it still
    /// solves.
    ///
    /// The four tables are what phase 3 moved fixture placement into, so this
    /// is the whole regression the venue-graph deploy was paying off: the sweep
    /// has to find dirty rows under two composite keys, `mark_synced` has to
    /// clean them, pull has to upsert onto the same keys, and the rows it
    /// leaves have to still be a graph.
    #[tokio::test]
    async fn a_venue_graph_pushes_and_pulls_and_still_solves() {
        let (_directory, pool) = migrated_pool().await;
        // `flush_pending` reads the session out of the state database; here the
        // app database stands in for both, so it needs the session schema.
        crate::database::local::auth::initialize_auth_state_schema(&pool)
            .await
            .unwrap();
        authenticate(&pool, "alice").await;
        seed_venue_graph(&pool).await;

        let enqueued = crate::sync::orchestrator::enqueue_dirty(&pool, "alice")
            .await
            .unwrap();
        assert!(enqueued >= 5, "every graph row is dirty on creation");
        let queued: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_name, record_id FROM pending_ops
             WHERE table_name LIKE 'venue\\_%' ESCAPE '\\' ORDER BY table_name, record_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            queued,
            vec![
                (
                    "venue_constraints".to_owned(),
                    format!("run-1{}end_b", registry::RECORD_ID_SEPARATOR)
                ),
                ("venue_edges".to_owned(), "run-1".to_owned()),
                (
                    "venue_node_params".to_owned(),
                    format!("run-1{}span", registry::RECORD_ID_SEPARATOR)
                ),
                ("venue_nodes".to_owned(), "run-1".to_owned()),
                ("venue_nodes".to_owned(), "v-1:venue".to_owned()),
            ],
            "a composite record id names every key column, and never splits an id",
        );

        let remote = MockRemoteClient::new();
        push::flush_pending(&pool, &pool, &remote).await.unwrap();
        let pushed = remote.upserted.lock().unwrap().clone();
        let param = pushed
            .iter()
            .find(|(table, _)| table == "venue_node_params")
            .expect("the span was pushed");
        assert_eq!(param.1["node_id"], json!("run-1"));
        assert_eq!(param.1["key"], json!("span"));
        assert_eq!(param.1["uid"], json!("alice"));
        assert_eq!(
            param.1.as_object().unwrap().len(),
            registry::get_table("venue_node_params")
                .unwrap()
                .remote_columns()
                .len(),
            "the payload is exactly the columns the remote has",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM venue_node_params WHERE synced_at IS NULL"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "a composite-keyed row is marked synced by its whole key",
        );

        // The other machine widens the truss and moves the far end.
        remote.on_select_page(
            "venue_node_params",
            vec![json!({
                "node_id": "run-1",
                "uid": "alice",
                "key": "span",
                "value": 12.0,
                "created_at": "2026-08-01T00:00:00Z",
                "updated_at": "2026-08-03T00:00:00Z",
                "sync_seq": 7,
            })],
        );
        remote.on_select_page(
            "venue_edges",
            vec![json!({
                "child_id": "run-1",
                "uid": "alice",
                "parent_id": "v-1:venue",
                "my_socket": "grab",
                "their_socket": "floor",
                "roll": 0.25,
                "created_at": "2026-08-01T00:00:00Z",
                "updated_at": "2026-08-03T00:00:00Z",
                "sync_seq": 8,
            })],
        );

        let authored_directory = tempfile::tempdir().unwrap();
        let authored = crate::services::authored_documents::AuthoredDocuments::new(
            crate::storage::StorageRoot::from_path(authored_directory.path().join("authored")),
        );
        let workspaces = crate::agent_execution::PythonWorkspaceService::new(
            authored_directory.path().join("python-workspaces"),
            std::sync::Arc::new(|| Err("python is not used by this sync test".into())),
        );
        let graph_runs = crate::agent_execution::GraphRunStore::new();
        let subagents = crate::agent::subagent::SubagentRegistry::default();
        pull::pull_all(
            &pool,
            &authored,
            &workspaces,
            &graph_runs,
            &subagents,
            &remote,
            "token",
            Some("alice"),
        )
        .await
        .unwrap();

        let (span, span_origin, span_synced): (f64, String, Option<String>) = sqlx::query_as(
            "SELECT value, origin, synced_at FROM venue_node_params
             WHERE node_id = 'run-1' AND key = 'span'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!((span - 12.0).abs() < f64::EPSILON, "the remote span landed");
        assert_eq!(
            span_origin, "local",
            "alice's own row, so a later delete pushes"
        );
        assert!(span_synced.is_some(), "a pulled row is not dirty");
        let roll: f64 = sqlx::query_scalar("SELECT roll FROM venue_edges WHERE child_id = 'run-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!((roll - 0.25).abs() < f64::EPSILON);

        let mut access = crate::database::local::venue_access::VenueAccess::<
            crate::database::local::venue_access::Read,
        >::read(
            &pool,
            crate::database::local::venue_access::VenueResource::Venue("v-1"),
        )
        .await
        .unwrap();
        let rows = crate::database::local::venue_graph::get_graph(&mut access)
            .await
            .unwrap();
        drop(access);
        let solved = crate::venue_graph::resolve_rows(&rows, &fixtures_root()).expect("solve");
        assert!(
            solved.pose("run-1").is_some(),
            "the truss is still placed after the round trip",
        );
    }
}
