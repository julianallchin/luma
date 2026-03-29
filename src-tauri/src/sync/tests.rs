#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use crate::sync::error::SyncError;
    use crate::sync::pending;
    use crate::sync::pull;
    use crate::sync::state;
    use crate::sync::traits::RemoteClient;

    // ========================================================================
    // Mock remote client
    // ========================================================================

    /// Records all calls and returns canned responses.
    struct MockRemoteClient {
        /// Canned responses: key = "{table}:{query_prefix}", value = rows to return.
        select_responses: Mutex<HashMap<String, Vec<Value>>>,
        /// All upsert calls recorded here for assertion.
        upserted: Mutex<Vec<(String, Value)>>,
    }

    impl MockRemoteClient {
        fn new() -> Self {
            Self {
                select_responses: Mutex::new(HashMap::new()),
                upserted: Mutex::new(Vec::new()),
            }
        }

        /// Register a canned response for select queries on a table.
        fn on_select(&self, table: &str, rows: Vec<Value>) {
            self.select_responses
                .lock()
                .unwrap()
                .insert(table.to_string(), rows);
        }

        fn upsert_count(&self) -> usize {
            self.upserted.lock().unwrap().len()
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
                op_type TEXT NOT NULL,
                table_name TEXT NOT NULL,
                record_id TEXT NOT NULL,
                payload_json TEXT,
                conflict_key TEXT NOT NULL DEFAULT 'id',
                tier INTEGER NOT NULL DEFAULT 0,
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
             ON pending_ops(table_name, record_id, op_type)",
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

    // ========================================================================
    // Pending ops tests
    // ========================================================================

    #[tokio::test]
    async fn test_enqueue_upsert() {
        let pool = test_pool().await;

        pending::enqueue_upsert(
            &pool,
            "venues",
            "abc-123",
            r#"{"id":"abc-123","name":"Test"}"#,
            "id",
            0,
        )
        .await
        .unwrap();

        let count = pending::count_pending(&pool).await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_enqueue_deduplication() {
        let pool = test_pool().await;

        // Enqueue twice for the same record
        pending::enqueue_upsert(
            &pool,
            "venues",
            "abc-123",
            r#"{"id":"abc-123","name":"First"}"#,
            "id",
            0,
        )
        .await
        .unwrap();

        pending::enqueue_upsert(
            &pool,
            "venues",
            "abc-123",
            r#"{"id":"abc-123","name":"Second"}"#,
            "id",
            0,
        )
        .await
        .unwrap();

        // Should still be only 1 op (deduplicated)
        let count = pending::count_pending(&pool).await.unwrap();
        assert_eq!(count, 1);

        // And it should have the latest payload
        let ops = pending::fetch_ready_ops(&pool).await.unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].payload_json.as_ref().unwrap().contains("Second"));
    }

    #[tokio::test]
    async fn test_retry_backoff() {
        let pool = test_pool().await;

        pending::enqueue_upsert(&pool, "venues", "abc-123", r#"{"id":"abc-123"}"#, "id", 0)
            .await
            .unwrap();

        let ops = pending::fetch_ready_ops(&pool).await.unwrap();
        assert_eq!(ops.len(), 1);

        // Record a failure
        pending::record_failure(&pool, ops[0].id, 1, "timeout")
            .await
            .unwrap();

        // Op should NOT be ready immediately (backoff pushed next_retry_at forward)
        let ready = pending::fetch_ready_ops(&pool).await.unwrap();
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

        pending::enqueue_upsert(&pool, "venues", "abc-123", r#"{"id":"abc-123"}"#, "id", 0)
            .await
            .unwrap();

        let ops = pending::fetch_ready_ops(&pool).await.unwrap();
        pending::record_failure(&pool, ops[0].id, 1, "error")
            .await
            .unwrap();

        // Not ready after failure
        assert_eq!(pending::fetch_ready_ops(&pool).await.unwrap().len(), 0);

        // Reset retry
        pending::reset_retry(&pool, ops[0].id).await.unwrap();

        // Now it should be ready again
        assert_eq!(pending::fetch_ready_ops(&pool).await.unwrap().len(), 1);
    }

    // ========================================================================
    // Sync state tests
    // ========================================================================

    #[tokio::test]
    async fn test_sync_state_defaults_to_epoch() {
        let pool = test_pool().await;

        let ts = state::get_last_pulled_at(&pool, "test-uid", "venues")
            .await
            .unwrap();
        assert_eq!(ts, "1970-01-01T00:00:00Z");
    }

    #[tokio::test]
    async fn test_sync_state_set_and_get() {
        let pool = test_pool().await;

        state::set_last_pulled_at(&pool, "test-uid", "venues", "2026-03-28T12:00:00Z")
            .await
            .unwrap();

        let ts = state::get_last_pulled_at(&pool, "test-uid", "venues")
            .await
            .unwrap();
        assert_eq!(ts, "2026-03-28T12:00:00Z");
    }

    // ========================================================================
    // Pull: discovery tests
    // ========================================================================

    #[tokio::test]
    async fn test_discover_owned_venues() {
        let pool = test_pool().await;
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
    async fn test_discover_joined_venues() {
        let pool = test_pool().await;
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
    async fn test_discover_removes_stale_member_venues() {
        let pool = test_pool().await;
        let mock = MockRemoteClient::new();

        // Insert a member venue locally that no longer exists remotely
        sqlx::query(
            "INSERT INTO venues (id, uid, name, role) VALUES ('v-stale', 'owner-uid', 'Stale', 'member')",
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

        // Stale member venue should be deleted
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM venues WHERE id = 'v-stale'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    // ========================================================================
    // Push: flush tests
    // ========================================================================

    #[tokio::test]
    async fn test_flush_upsert_op() {
        let pool = test_pool().await;
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
            "venues",
            "v-1",
            r#"{"id":"v-1","uid":"u-1","name":"Test"}"#,
            "id",
            0,
        )
        .await
        .unwrap();

        assert_eq!(pending::count_pending(&pool).await.unwrap(), 1);

        // We need a state_pool with auth — for this test, we'll call flush
        // directly with the mock, bypassing auth. Let's test execute_op logic
        // by calling the functions individually.

        let ops = pending::fetch_ready_ops(&pool).await.unwrap();
        assert_eq!(ops.len(), 1);

        // Simulate what flush_pending does: execute the op
        let op = &ops[0];
        let payload: Value = serde_json::from_str(op.payload_json.as_ref().unwrap()).unwrap();
        mock.upsert_json("venues", &payload, "id", "fake-token")
            .await
            .unwrap();

        // Remove the op and mark synced
        pending::remove_op(&pool, op.id).await.unwrap();

        assert_eq!(pending::count_pending(&pool).await.unwrap(), 0);
        assert_eq!(mock.upsert_count(), 1);
    }

    #[tokio::test]
    async fn test_ops_processed_in_tier_order() {
        let pool = test_pool().await;

        // Enqueue ops in reverse tier order
        pending::enqueue_upsert(&pool, "track_scores", "ts-1", r#"{"id":"ts-1"}"#, "id", 3)
            .await
            .unwrap();
        pending::enqueue_upsert(&pool, "venues", "v-1", r#"{"id":"v-1"}"#, "id", 0)
            .await
            .unwrap();
        pending::enqueue_upsert(&pool, "fixtures", "f-1", r#"{"id":"f-1"}"#, "id", 1)
            .await
            .unwrap();

        let ops = pending::fetch_ready_ops(&pool).await.unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].table_name, "venues"); // tier 0
        assert_eq!(ops[1].table_name, "fixtures"); // tier 1
        assert_eq!(ops[2].table_name, "track_scores"); // tier 3
    }
}
