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
        /// Tombstone PATCHes, which are a different statement from an upsert.
        patched: Mutex<Vec<String>>,
    }

    impl MockRemoteClient {
        fn new() -> Self {
            Self {
                select_responses: Mutex::new(HashMap::new()),
                select_pages: Mutex::new(HashMap::new()),
                selected_tables: Mutex::new(Vec::new()),
                upserted: Mutex::new(Vec::new()),
                next_upsert_error: Mutex::new(None),
                patched: Mutex::new(Vec::new()),
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

        fn patched_tables(&self) -> Vec<String> {
            self.patched.lock().unwrap().clone()
        }

        fn upserted_tables(&self) -> Vec<String> {
            self.upserted
                .lock()
                .unwrap()
                .iter()
                .map(|(table, _)| table.clone())
                .collect()
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
            _payload: &Value,
            _token: &str,
        ) -> Result<(), SyncError> {
            self.patched.lock().unwrap().push(table.to_string());
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
    /// The real schema, every time.
    ///
    /// This used to hand-roll a subset of the tables, which meant the triggers
    /// the sync engine depends on were simply absent from its own tests and the
    /// hand-rolled DDL drifted from the migrations it was copied from. The
    /// state-based push design leans on those triggers — the dirtiness stamp,
    /// the delete guards, the immutability refusals — so a test pool without
    /// them proves nothing.
    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let (directory, pool) = migrated_pool().await;
        // The session row normally lives in the state database. Tests drive
        // push with one pool for both, so the app database carries it here.
        crate::database::local::auth::initialize_auth_state_schema(&pool)
            .await
            .unwrap();
        (directory, pool)
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

    /// Insert a row the way a pull does. The admission triggers refuse a venue
    /// whose uid is not the active principal, which is exactly right for
    /// application code and exactly wrong for a fixture standing in for the
    /// server.
    async fn seed_as_remote(pool: &SqlitePool, sql: &str) {
        let mut transaction = pool.begin().await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut transaction)
            .await
            .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(&mut *transaction)
            .await
            .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
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
        push::flush_pending(&pool, &pool, &remote).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_message_appends WHERE synced_at IS NULL"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
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
        let failed: Vec<(String, i64)> =
            sqlx::query_as("SELECT table_name, attempts FROM sync_push_failures")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            failed,
            vec![("agent_thread_message_appends".to_owned(), 1)],
            "the receipt is the failure, not the whole batch"
        );
        // Stand in for the backoff window elapsing.
        sqlx::query("DELETE FROM sync_push_failures")
            .execute(&pool)
            .await
            .unwrap();

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
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_thread_message_appends WHERE synced_at IS NULL"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
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
                mode_name, fixture_path, address_pinned, updated_at, synced_at)
             VALUES ('f-1', 'u-1', 'v-1', 1, 100, 16, 'Acme', 'Mover', '16ch',
                'acme/mover.qxf', 1, '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z')",
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
        let (_directory, pool) = test_pool().await;
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

    // ========================================================================
    // Sync state tests
    // ========================================================================

    // ========================================================================
    // State-based push
    //
    // Every test here asks the same question in a different shape: does push
    // read the tables, and only the tables? See docs/design/sync-push-v2.md.
    // ========================================================================

    const VENUE: &str = "6f1a9b60-0000-4000-8000-000000000001";
    const OTHER_VENUE: &str = "6f1a9b60-0000-4000-8000-000000000002";

    async fn seed_venue(pool: &SqlitePool, id: &str, uid: &str, name: &str) {
        sqlx::query(
            "INSERT INTO venues (id, uid, name, role, updated_at)
             VALUES (?, ?, ?, 'owner', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(uid)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn delivery_marker(pool: &SqlitePool, id: &str) -> (Option<String>, String) {
        sqlx::query_as("SELECT synced_at, updated_at FROM venues WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A remote that edits the local row while its upsert is in flight. This is
    /// the shape of audit T1.2: under the queue, the payload had been snapshot
    /// before the edit and `mark_synced` cleaned the row anyway, so the edit was
    /// reverted by the next pull.
    struct EditingRemote {
        pool: SqlitePool,
        statement: String,
        inner: MockRemoteClient,
    }

    #[async_trait]
    impl RemoteClient for EditingRemote {
        async fn select_json(
            &self,
            table: &str,
            query: &str,
            token: &str,
        ) -> Result<Vec<Value>, SyncError> {
            self.inner.select_json(table, query, token).await
        }

        async fn upsert_json(
            &self,
            table: &str,
            payload: &Value,
            conflict_key: &str,
            token: &str,
        ) -> Result<(), SyncError> {
            sqlx::query(sqlx::AssertSqlSafe(self.statement.clone()))
                .execute(&self.pool)
                .await
                .unwrap();
            self.inner
                .upsert_json(table, payload, conflict_key, token)
                .await
        }

        async fn patch_json(
            &self,
            table: &str,
            filter: &str,
            payload: &Value,
            token: &str,
        ) -> Result<(), SyncError> {
            self.inner.patch_json(table, filter, payload, token).await
        }

        async fn upload_file(
            &self,
            bucket: &str,
            path: &str,
            bytes: Vec<u8>,
            content_type: &str,
            token: &str,
        ) -> Result<String, SyncError> {
            self.inner
                .upload_file(bucket, path, bytes, content_type, token)
                .await
        }

        async fn download_file(
            &self,
            bucket: &str,
            path: &str,
            token: &str,
        ) -> Result<Vec<u8>, SyncError> {
            self.inner.download_file(bucket, path, token).await
        }
    }

    #[tokio::test]
    async fn a_dirty_row_is_delivered_once_and_then_left_alone() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        seed_venue(&pool, VENUE, "u-1", "Test").await;

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 1);
        let (synced_at, updated_at) = delivery_marker(&pool, VENUE).await;
        assert_eq!(synced_at.as_deref(), Some(updated_at.as_str()));

        // Nothing changed, so there is nothing to say.
        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 0);
        assert_eq!(mock.upsert_count(), 1);
    }

    /// The payload is the row as it stands when the request is made, and the
    /// receipt only lands if the row is still that row.
    #[tokio::test]
    async fn an_edit_during_the_push_leaves_the_row_dirty() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        seed_venue(&pool, VENUE, "u-1", "Before").await;
        let remote = EditingRemote {
            pool: pool.clone(),
            statement: format!("UPDATE venues SET name = 'During' WHERE id = '{VENUE}'"),
            inner: MockRemoteClient::new(),
        };

        push::flush_pending(&pool, &pool, &remote).await.unwrap();

        let (synced_at, updated_at) = delivery_marker(&pool, VENUE).await;
        assert_ne!(
            synced_at.as_deref(),
            Some(updated_at.as_str()),
            "the row moved during the call, so it still owes the server the newer content"
        );
        assert_eq!(
            push::flush_pending(&pool, &pool, &remote).await.unwrap(),
            1,
            "and the next cycle sends it"
        );
    }

    /// The pair that used to wedge the queue — an upsert and a delete for one
    /// identity — cannot both exist. The tables answer once.
    #[tokio::test]
    async fn a_row_and_its_tombstone_cannot_both_be_pending() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        seed_venue(&pool, VENUE, "u-1", "Doomed").await;
        push::flush_pending(&pool, &pool, &mock).await.unwrap();

        let mut connection = pool.acquire().await.unwrap();
        crate::database::local::sync_delete::delete_synced_row(&mut connection, "venues", &[VENUE])
            .await
            .unwrap();
        drop(connection);

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM venues WHERE id = ?")
                .bind(VENUE)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "the row is gone, so no read path can see it"
        );
        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 1);
        assert_eq!(mock.patched_tables(), vec!["venues".to_string()]);
        assert_eq!(mock.upsert_count(), 1, "no second upsert of a deleted row");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sync_tombstones")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "an accepted tombstone is forgotten"
        );
    }

    /// Delete, then recreate the same identity. The row exists, so it is not
    /// deleted — final state wins, and the stale tombstone is dropped rather
    /// than sent.
    #[tokio::test]
    async fn a_row_that_exists_again_outranks_its_tombstone() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        seed_venue(&pool, VENUE, "u-1", "First").await;
        push::flush_pending(&pool, &pool, &mock).await.unwrap();

        let mut connection = pool.acquire().await.unwrap();
        crate::database::local::sync_delete::delete_synced_row(&mut connection, "venues", &[VENUE])
            .await
            .unwrap();
        drop(connection);
        seed_venue(&pool, VENUE, "u-1", "Again").await;

        push::flush_pending(&pool, &pool, &mock).await.unwrap();

        assert!(mock.patched_tables().is_empty(), "nothing was deleted");
        assert_eq!(mock.upsert_count(), 2);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sync_tombstones")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    /// An identity the remote column type cannot hold is recorded once and
    /// never retried, and its children go quiet with it because their parent
    /// never becomes reachable.
    #[tokio::test]
    async fn an_unpushable_venue_takes_its_subtree_quiet_with_it() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        seed_venue(&pool, "djtable-scratch-1", "u-1", "Scratch").await;
        sqlx::query(
            "INSERT INTO fixtures
                (id, uid, venue_id, universe, address, num_channels, manufacturer, model,
                 mode_name, fixture_path, updated_at)
             VALUES (?, 'u-1', 'djtable-scratch-1', 1, 1, 4, 'Acme', 'Par', 'Mode', 'acme/par',
                     '2026-01-01T00:00:00Z')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 0);
        assert_eq!(mock.upsert_count(), 0, "neither row is ever sent");

        let (attempts, permanent, error) = crate::sync::push_state::state_of(
            &pool,
            "signed-in:u-1",
            "venues",
            "djtable-scratch-1",
        )
        .await
        .unwrap()
        .expect("recorded");
        assert_eq!(attempts, 1);
        assert!(permanent);
        assert!(error.is_some_and(|error| error.contains("uuid")));

        assert!(
            crate::sync::push_state::state_of(&pool, "signed-in:u-1", "fixtures", "any")
                .await
                .unwrap()
                .is_none(),
            "the child was skipped, not failed: nothing about it is wrong"
        );

        let blocked = crate::sync::push_state::blocked(&pool, "signed-in:u-1")
            .await
            .unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].table_name, "venues");
        assert_eq!(blocked[0].record_id, "djtable-scratch-1");
    }

    /// Rescanning a dirty row is not an attempt. The backoff belongs to the
    /// failure, and only different content restarts it (audit T2.1).
    #[tokio::test]
    async fn a_rescan_does_not_reset_the_retry_budget() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        mock.fail_next_upsert(409, r#"{"code":"23505","message":"duplicate key"}"#);
        seed_venue(&pool, VENUE, "u-1", "Conflicted").await;

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 0);
        let (attempts, permanent, _) =
            crate::sync::push_state::state_of(&pool, "signed-in:u-1", "venues", VENUE)
                .await
                .unwrap()
                .expect("recorded");
        assert_eq!(attempts, 1);
        assert!(!permanent, "a catalog conflict may resolve itself");

        // A second cycle re-derives the same dirty row and must leave it alone.
        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 0);
        let (attempts, _, _) =
            crate::sync::push_state::state_of(&pool, "signed-in:u-1", "venues", VENUE)
                .await
                .unwrap()
                .expect("still recorded");
        assert_eq!(attempts, 1, "the backoff survived the rescan");

        // Editing the row is different content, so it is tried again at once.
        sqlx::query("UPDATE venues SET name = 'Edited' WHERE id = ?")
            .bind(VENUE)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 1);
        assert!(
            crate::sync::push_state::state_of(&pool, "signed-in:u-1", "venues", VENUE)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn push_delivers_parents_before_children() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        seed_venue(&pool, VENUE, "u-1", "Ordered").await;
        sqlx::query(
            "INSERT INTO fixtures
                (id, uid, venue_id, universe, address, num_channels, manufacturer, model,
                 mode_name, fixture_path, updated_at)
             VALUES (?, 'u-1', ?, 1, 1, 4, 'Acme', 'Par', 'Mode', 'acme/par',
                     '2026-01-01T00:00:00Z')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(VENUE)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 2);
        assert_eq!(
            mock.upserted_tables(),
            vec!["venues".to_string(), "fixtures".to_string()]
        );
    }

    #[tokio::test]
    async fn another_principals_row_is_not_this_principals_to_push() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "alice").await;
        let mock = MockRemoteClient::new();
        seed_venue(&pool, VENUE, "alice", "Alice").await;
        seed_as_remote(
            &pool,
            &format!(
                "INSERT INTO venues (id, uid, name, role, updated_at)
                 VALUES ('{OTHER_VENUE}', 'bob', 'Bob', 'owner', '2026-01-01T00:00:00Z')"
            ),
        )
        .await;

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 1);
        assert_eq!(mock.upsert_count(), 1);
    }

    /// A tombstone recorded while signed out belongs to nobody who can deliver
    /// it, and signing in does not adopt it.
    #[tokio::test]
    async fn a_signed_out_tombstone_never_flushes_after_sign_in() {
        let (_directory, pool) = test_pool().await;
        sqlx::query(
            "INSERT INTO sync_tombstones (principal_key, table_name, record_id)
             VALUES ('signed-out', 'venues', ?)",
        )
        .bind(VENUE)
        .execute(&pool)
        .await
        .unwrap();
        authenticate(&pool, "alice").await;
        let mock = MockRemoteClient::new();

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 0);
        assert!(mock.patched_tables().is_empty());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sync_tombstones WHERE principal_key = 'signed-out'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1,
            "it survives, unspoken"
        );
    }

    /// An upgrade cannot lose an unpushed edit: every queue row becomes a dirty
    /// row or a tombstone, and the queue is gone afterwards.
    #[tokio::test]
    async fn the_legacy_queue_is_drained_into_rows_and_tombstones() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();

        // A row that was already delivered under the old engine, with its
        // re-edit still queued behind it.
        seed_venue(&pool, VENUE, "u-1", "Queued edit").await;
        sqlx::query("UPDATE venues SET synced_at = updated_at, version = version + 1 WHERE id = ?")
            .bind(VENUE)
            .execute(&pool)
            .await
            .unwrap();
        for (op_type, record_id) in [("upsert", VENUE), ("delete", OTHER_VENUE)] {
            sqlx::query(
                "INSERT INTO pending_ops_drain
                    (principal_key, op_type, table_name, record_id, payload_json)
                 VALUES ('signed-in:u-1', ?, 'venues', ?, '{\"id\":\"stale snapshot\"}')",
            )
            .bind(op_type)
            .bind(record_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 2);
        assert_eq!(
            mock.upserted.lock().unwrap()[0].1["name"],
            json!("Queued edit"),
            "the row was re-read, not replayed from the queued snapshot"
        );
        assert_eq!(mock.patched_tables(), vec!["venues".to_string()]);
        assert!(
            sqlx::query_scalar::<_, String>(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'pending_ops_drain'"
            )
            .fetch_optional(&pool)
            .await
            .unwrap()
            .is_none(),
            "the queue is gone, and its absence is the completion flag"
        );
    }

    /// The eleven tables that gained a delivery marker have it NULL on every
    /// historical row, so the migration stamps them: under the old engine those
    /// rows were delivered when their queue entry was removed. The drain then
    /// clears the marker again for exactly the rows an operation still names.
    ///
    /// The stamp belongs to the migration, not to the first flush. A row
    /// created *after* the upgrade but before the first sync — the ten seconds
    /// after launch, or a whole offline session — is genuinely new, and
    /// presuming it delivered would lose it silently.
    #[tokio::test]
    async fn an_upgrade_stamps_delivered_history_and_keeps_the_queued_edit() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();

        let store = AuthoredRevisionStore;
        let mut connection = pool.acquire().await.unwrap();
        let mut ids = Vec::new();
        for (subject, implementation) in [("pattern-a", "impl-a"), ("pattern-b", "impl-b")] {
            let document =
                NewAuthoredDocument::pattern_graph("signed-in:u-1", subject, implementation)
                    .unwrap();
            store
                .insert_document(&mut connection, &document)
                .await
                .unwrap();
            ids.push(document.id.to_string());
        }
        drop(connection);
        // Stand in for the migration having run over both rows, with only the
        // second still named by a queue entry.
        stamp_delivered(&pool, "authored_documents").await;
        sqlx::query(
            "INSERT INTO pending_ops_drain
                (principal_key, op_type, table_name, record_id, payload_json)
             VALUES ('signed-in:u-1', 'insert_immutable', 'authored_documents', ?, '{}')",
        )
        .bind(&ids[1])
        .execute(&pool)
        .await
        .unwrap();

        push::flush_pending(&pool, &pool, &mock).await.unwrap();

        let pushed = mock.upserted.lock().unwrap().clone();
        assert_eq!(pushed.len(), 1, "history is not re-sent");
        assert_eq!(pushed[0].1["document_id"], json!(ids[1]));
        assert!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT synced_at FROM authored_documents WHERE document_id = ?"
            )
            .bind(&ids[0])
            .fetch_one(&pool)
            .await
            .unwrap()
            .is_some(),
            "a row no operation named was already delivered"
        );
    }

    /// A row created after the upgrade is not history, whatever the drain finds
    /// in the queue beside it.
    #[tokio::test]
    async fn a_row_created_after_the_upgrade_is_never_presumed_delivered() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        let document =
            NewAuthoredDocument::pattern_graph("signed-in:u-1", "pattern-new", "impl-new").unwrap();
        let mut connection = pool.acquire().await.unwrap();
        AuthoredRevisionStore
            .insert_document(&mut connection, &document)
            .await
            .unwrap();
        drop(connection);
        // The queue is still there — the upgrade happened, this row did not
        // exist for it, and the drain has not run yet.
        sqlx::query(
            "INSERT INTO pending_ops_drain
                (principal_key, op_type, table_name, record_id, payload_json)
             VALUES ('signed-in:u-1', 'upsert', 'venues', 'unrelated', '{}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        push::flush_pending(&pool, &pool, &mock).await.unwrap();

        assert_eq!(
            mock.upserted_tables(),
            vec!["authored_documents".to_owned()]
        );
    }

    /// Stamp a table's delivery marker the way the migration does.
    async fn stamp_delivered(pool: &SqlitePool, table: &str) {
        let mut transaction = pool.begin().await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut transaction)
            .await
            .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET synced_at = CURRENT_TIMESTAMP WHERE synced_at IS NULL"
        )))
        .execute(&mut *transaction)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }

    /// An operation the old queue had given up on arrives given up on. Carrying
    /// the attempt count over is only half of it: the verdict also has to carry
    /// the row version it was recorded against, or nothing the user does could
    /// ever clear it.
    #[tokio::test]
    async fn a_dead_lettered_operation_arrives_dead_lettered_and_is_still_recoverable() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        seed_venue(&pool, VENUE, "u-1", "Stuck").await;
        sqlx::query(
            "INSERT INTO pending_ops_drain
                (principal_key, op_type, table_name, record_id, payload_json, attempts, last_error)
             VALUES ('signed-in:u-1', 'upsert', 'venues', ?, '{}', 20, 'boom')",
        )
        .bind(VENUE)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 0);
        assert_eq!(mock.upsert_count(), 0, "a spent budget is not refilled");
        let (attempts, permanent, error) =
            crate::sync::push_state::state_of(&pool, "signed-in:u-1", "venues", VENUE)
                .await
                .unwrap()
                .expect("the verdict came across");
        assert_eq!(attempts, 20);
        assert!(permanent);
        assert_eq!(error.as_deref(), Some("boom"));

        // Editing the row is new content, and new content is worth a new try.
        sqlx::query("UPDATE venues SET name = 'Edited' WHERE id = ?")
            .bind(VENUE)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(push::flush_pending(&pool, &pool, &mock).await.unwrap(), 1);
    }

    /// Deleting a synced row without recording the deletion is refused by the
    /// database, so no future call site can quietly diverge from the server.
    #[tokio::test]
    async fn an_unrecorded_delete_of_a_synced_row_is_refused() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        seed_venue(&pool, VENUE, "u-1", "Guarded").await;

        let error = sqlx::query("DELETE FROM venues WHERE id = ?")
            .bind(VENUE)
            .execute(&pool)
            .await
            .expect_err("the guard trigger refuses it");
        assert!(error.to_string().contains("not recorded as a tombstone"));
    }

    #[tokio::test]
    async fn test_sync_state_defaults_to_zero() {
        let (_directory, pool) = test_pool().await;

        let sequence = state::get_last_pulled_seq(&pool, "test-uid", "venues")
            .await
            .unwrap();
        assert_eq!(sequence, 0);
    }

    #[tokio::test]
    async fn test_sync_state_set_and_get() {
        let (_directory, pool) = test_pool().await;

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
        let (_directory, pool) = test_pool().await;

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
        let (_directory, pool) = test_pool().await;
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
        let (_directory, pool) = test_pool().await;
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
        let (_directory, pool) = test_pool().await;
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
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "user-1").await;
        let mock = MockRemoteClient::new();

        // Insert a member venue locally that no longer exists remotely
        seed_as_remote(
            &pool,
            "INSERT INTO venues (id, uid, name, role)
             VALUES ('v-stale', 'owner-uid', 'Stale', 'member')",
        )
        .await;
        seed_as_remote(
            &pool,
            "INSERT INTO venue_memberships (venue_id, user_id, role)
             VALUES ('v-stale', 'user-1', 'member')",
        )
        .await;

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
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('6f1a9b60-0000-4000-8000-0000000000aa', 'alice', 'Room')")
            .execute(pool)
            .await
            .unwrap();
        for (id, kind, catalog_ref, label) in [
            (
                "6f1a9b60-0000-4000-8000-0000000000aa:venue",
                "venue",
                None,
                None,
            ),
            ("run-1", "run", Some("truss/straight"), Some("Downstage")),
        ] {
            sqlx::query(
                "INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label)
                 VALUES (?, 'alice', '6f1a9b60-0000-4000-8000-0000000000aa', ?, ?, ?)",
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
             VALUES ('run-1', 'alice', '6f1a9b60-0000-4000-8000-0000000000aa:venue', 'grab', 'floor', 0.0)",
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
             VALUES ('run-1', 'alice', 'end_b', '6f1a9b60-0000-4000-8000-0000000000aa:venue', 'floor')",
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

        let remote = MockRemoteClient::new();
        push::flush_pending(&pool, &pool, &remote).await.unwrap();
        let pushed = remote.upserted.lock().unwrap().clone();
        // Every graph row is dirty on creation, and one cycle delivers all of
        // them under their whole composite keys: a child is only reachable once
        // its node has landed, and topological order is what makes that true
        // inside a single flush.
        let mut delivered: Vec<String> = pushed
            .iter()
            .filter(|(table, _)| table.starts_with("venue_"))
            .map(|(table, _)| table.clone())
            .collect();
        delivered.sort();
        assert_eq!(
            delivered,
            vec![
                "venue_constraints",
                "venue_edges",
                "venue_node_params",
                "venue_nodes",
                "venue_nodes",
            ]
        );
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
                "parent_id": "6f1a9b60-0000-4000-8000-0000000000aa:venue",
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
            crate::database::local::venue_access::VenueResource::Venue(
                "6f1a9b60-0000-4000-8000-0000000000aa",
            ),
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

    // ========================================================================
    // Adversarial review additions (2026-09-01)
    // ========================================================================

    /// A remote that refuses every tombstone PATCH with a plain 4xx.
    struct RefusingPatchRemote {
        inner: MockRemoteClient,
        patch_attempts: std::sync::atomic::AtomicUsize,
    }

    impl RefusingPatchRemote {
        fn new() -> Self {
            Self {
                inner: MockRemoteClient::new(),
                patch_attempts: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn patch_attempts(&self) -> usize {
            self.patch_attempts
                .load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl RemoteClient for RefusingPatchRemote {
        async fn select_json(
            &self,
            table: &str,
            query: &str,
            token: &str,
        ) -> Result<Vec<Value>, SyncError> {
            self.inner.select_json(table, query, token).await
        }
        async fn upsert_json(
            &self,
            table: &str,
            payload: &Value,
            conflict_key: &str,
            token: &str,
        ) -> Result<(), SyncError> {
            self.inner
                .upsert_json(table, payload, conflict_key, token)
                .await
        }
        async fn patch_json(
            &self,
            _table: &str,
            _filter: &str,
            _payload: &Value,
            _token: &str,
        ) -> Result<(), SyncError> {
            self.patch_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(SyncError::Api {
                status: 403,
                message: "row level security".into(),
            })
        }
        async fn upload_file(
            &self,
            bucket: &str,
            path: &str,
            bytes: Vec<u8>,
            content_type: &str,
            token: &str,
        ) -> Result<String, SyncError> {
            self.inner
                .upload_file(bucket, path, bytes, content_type, token)
                .await
        }
        async fn download_file(
            &self,
            bucket: &str,
            path: &str,
            token: &str,
        ) -> Result<Vec<u8>, SyncError> {
            self.inner.download_file(bucket, path, token).await
        }
    }

    /// A refused tombstone must respect the backoff `sync_push_failures`
    /// records for it — §6 of the design says a failure means "attempts + 1,
    /// min(5*2^n, 300) s backoff", and a `permanent` verdict means "skipped and
    /// quiet". `tombstone::pending` never joins the failure table, so a
    /// tombstone the server refuses is re-sent on every flush forever.
    #[tokio::test]
    async fn a_refused_tombstone_respects_its_backoff() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let remote = RefusingPatchRemote::new();
        seed_venue(&pool, VENUE, "u-1", "Doomed").await;
        push::flush_pending(&pool, &pool, &remote).await.unwrap();

        let mut connection = pool.acquire().await.unwrap();
        crate::database::local::sync_delete::delete_synced_row(&mut connection, "venues", &[VENUE])
            .await
            .unwrap();
        drop(connection);

        // Ten back-to-back flushes, all inside the first backoff window.
        for _ in 0..10 {
            push::flush_pending(&pool, &pool, &remote).await.unwrap();
        }

        let next_retry: String = sqlx::query_scalar(
            "SELECT next_retry_at FROM sync_push_failures WHERE table_name = 'venues'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            remote.patch_attempts(),
            1,
            "the failure recorded a backoff to {next_retry}, so only the first \
             attempt should have gone out"
        );
    }

    /// A dirty row that fails permanently is supposed to go "quiet": §4/§6 say
    /// it is recorded once and never retried. The sign-out durability audit
    /// counts dirty rows without consulting `sync_push_failures`, so a row push
    /// has given up on makes the wipe impossible rather than quiet.
    #[tokio::test]
    async fn a_permanently_undeliverable_row_is_quiet_for_the_durability_audit() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let mock = MockRemoteClient::new();
        // The one poison `unpushable_reason` names: a non-uuid venue id.
        seed_venue(&pool, "djtable-scratch-1", "u-1", "Debris").await;

        push::flush_pending(&pool, &pool, &mock).await.unwrap();
        let permanent: i64 = sqlx::query_scalar(
            "SELECT permanent FROM sync_push_failures WHERE record_id = 'djtable-scratch-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            permanent, 1,
            "the design records this identity as permanent"
        );

        let sql = crate::sync::registry::get_table("venues")
            .unwrap()
            .undelivered_count_sql()
            .unwrap();
        let outstanding: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind("u-1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            outstanding, 0,
            "a row push has permanently given up on is not outstanding work, \
             but the audit still counts it and refuses the sign-out wipe"
        );
    }

    /// `sync_push_failures` is keyed on `(table, record_id)` and nothing else,
    /// so a tombstone and the row that later reoccupies the same identity share
    /// one budget. Twenty refused tombstone attempts mark that key `permanent`;
    /// recreating the row then finds its own upsert already given up on, with a
    /// `seen_version` of NULL that the "content changed" escape hatch cannot
    /// clear.
    #[tokio::test]
    async fn a_refused_tombstone_does_not_poison_the_recreated_row() {
        let (_directory, pool) = test_pool().await;
        authenticate(&pool, "u-1").await;
        let remote = RefusingPatchRemote::new();
        seed_venue(&pool, VENUE, "u-1", "First").await;
        push::flush_pending(&pool, &pool, &remote).await.unwrap();
        assert_eq!(remote.inner.upsert_count(), 1);

        let mut connection = pool.acquire().await.unwrap();
        crate::database::local::sync_delete::delete_synced_row(&mut connection, "venues", &[VENUE])
            .await
            .unwrap();
        drop(connection);

        // The tombstone has no backoff, so twenty flushes burn the whole budget.
        for _ in 0..25 {
            push::flush_pending(&pool, &pool, &remote).await.unwrap();
        }
        let state = crate::sync::push_state::state_of(&pool, "signed-in:u-1", "venues", VENUE)
            .await
            .unwrap()
            .expect("the refused tombstone recorded a failure");
        assert!(state.1, "twenty attempts made the key permanent");

        // The user makes the venue again. It is a brand-new local row that the
        // server has never refused.
        seed_venue(&pool, VENUE, "u-1", "Again").await;
        push::flush_pending(&pool, &pool, &remote).await.unwrap();
        assert_eq!(
            remote.inner.upsert_count(),
            2,
            "the recreated row is fresh content and must still be pushed"
        );
    }
}
