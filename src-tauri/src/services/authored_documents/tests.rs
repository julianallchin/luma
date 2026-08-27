use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tempfile::TempDir;

use super::*;
use crate::models::agent_threads::{
    AppendAgentThreadMessagesInput, CreateAgentThreadInput, NewAgentThreadMessage,
};
use crate::models::authored_state::{
    AuthoredRevisionPosition, AuthoredWorkspaceMerge, CommitAuthoredWorkspaceInput,
    CreateAuthoredWorkspaceInput, ForkAuthoredWorkspaceInput, MergeAuthoredWorkspaceInput,
    MergeAuthoredWorkspaceIntoWorkspaceInput,
};
use crate::models::node_graph::{Graph, NodeInstance};
use crate::sync::authored_remote::{
    HeadIntegrationOutcome, HeadIntegrationResolution, HeadProposalIntegrator,
};
use crate::sync::error::SyncError;
use crate::sync::traits::RemoteClient;

/// A revision authored directly against the store, with no scope to read a
/// writer from. Tests that seed history do not exercise attribution — the
/// tests that do assert it name the actor explicitly.
fn revision_metadata(
    operation_kind: &str,
    operation_id: Option<&str>,
    subject: &str,
) -> Result<RevisionMetadata> {
    Ok(RevisionMetadata {
        operation_kind: operation_kind.to_owned(),
        operation_id: operation_id.map(str::to_owned),
        message: subject.to_owned(),
        actor: Actor::user(),
        author_name: "Luma".into(),
        author_email: "authored-state@luma.local".into(),
        authored_at: Utc::now().to_rfc3339(),
        thread_id: None,
        assistant_message_id: None,
        restored_revision_id: None,
    })
}

/// A migrated app database with write admission armed, plus the authored
/// authority over it. `pub(crate)` so the service-layer tests above the
/// authority — `score_mutations` — seed a score the same way rather than
/// growing a second, drifting copy of this setup.
pub(crate) struct Fixture {
    _directory: TempDir,
    pub(crate) pool: SqlitePool,
    pub(crate) authored: AuthoredDocuments,
    owner: Option<String>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_owner(None).await
    }

    async fn signed_in(owner: &str) -> Self {
        Self::with_owner(Some(owner.to_owned())).await
    }

    pub(crate) async fn with_owner(owner: Option<String>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("luma-test.db");
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
            .max_connections(8)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, owner.as_deref())
            .await
            .unwrap();
        let authored =
            AuthoredDocuments::new(StorageRoot::from_path(directory.path().join("storage")));
        Self {
            _directory: directory,
            pool,
            authored,
            owner,
        }
    }

    async fn pattern_thread(&self) -> AgentThread {
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', ?, 'Pattern')")
            .bind(self.owner.as_deref())
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('implementation', ?, 'pattern', ?)",
        )
        .bind(self.owner.as_deref())
        .bind(exact_graph_json(&empty_graph()).unwrap())
        .execute(&self.pool)
        .await
        .unwrap();
        self.authored
            .create_thread_with_authored_state(
                &self.pool,
                CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: "pattern_graph".into(),
                    subject_kind: Some("pattern".into()),
                    subject_id: Some("pattern".into()),
                    implementation_id: Some("implementation".into()),
                    ..Default::default()
                },
                self.owner.as_deref(),
            )
            .await
            .unwrap()
    }

    /// Seed one track, venue, score and pattern, and name the scope they form.
    pub(crate) async fn track_scope(&self) -> TrackScope {
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, title, duration_seconds, file_path)
             VALUES ('track', ?, 'track-hash', 'Track', 120.0, '/track')",
        )
        .bind(self.owner.as_deref())
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', ?, 'Venue')")
            .bind(self.owner.as_deref())
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('score', ?, 'track', 'venue', 'Score')",
        )
        .bind(self.owner.as_deref())
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', ?, 'Pattern')")
            .bind(self.owner.as_deref())
            .execute(&self.pool)
            .await
            .unwrap();
        TrackScope {
            score_id: "score".into(),
            track_id: "track".into(),
            venue_id: "venue".into(),
        }
    }

    async fn track_thread(&self) -> (AgentThread, TrackScope) {
        let scope = self.track_scope().await;
        let thread = self
            .authored
            .create_thread_with_authored_state(
                &self.pool,
                CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: "track_copilot".into(),
                    subject_kind: Some("track".into()),
                    subject_id: Some("track".into()),
                    venue_id: Some("venue".into()),
                    score_id: Some("score".into()),
                    ..Default::default()
                },
                self.owner.as_deref(),
            )
            .await
            .unwrap();
        (thread, scope)
    }

    /// A subagent thread under `parent`: same authored route, its own private
    /// workspace allocated by creation.
    async fn subagent_thread(&self, parent: &AgentThread) -> AgentThread {
        self.authored
            .create_thread_with_authored_state(
                &self.pool,
                CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: parent.agent_kind.clone(),
                    subject_kind: parent.subject_kind.clone(),
                    subject_id: parent.subject_id.clone(),
                    implementation_id: parent.implementation_id.clone(),
                    venue_id: parent.venue_id.clone(),
                    score_id: parent.score_id.clone(),
                    parent_thread_id: Some(parent.id.clone()),
                    parent_call_id: Some("call-1".into()),
                    ..Default::default()
                },
                self.owner.as_deref(),
            )
            .await
            .unwrap()
    }

    async fn active_workspace(&self, thread_id: &str) -> (String, String) {
        sqlx::query_as(
            "SELECT workspace_id, head_revision_id FROM authored_subagent_workspaces
             WHERE owner_thread_id = ? AND status = 'active'",
        )
        .bind(thread_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn append_assistant(&self, thread_id: &str, message_id: &str) {
        let head = agent_threads::transcript_head(&self.pool, thread_id, self.owner.as_deref())
            .await
            .unwrap();
        agent_threads::append_messages(
            &self.pool,
            thread_id,
            AppendAgentThreadMessagesInput {
                operation_id: uuid::Uuid::new_v4().to_string(),
                expected_head_message_id: head.head_message_id,
                messages: vec![NewAgentThreadMessage {
                    id: Some(message_id.into()),
                    role: "assistant".into(),
                    parts: json!([]),
                }],
            },
            self.owner.as_deref(),
        )
        .await
        .unwrap();
    }
}

struct NoHeadRemote {
    proposal_id: String,
    document_id: String,
    server_head_revision_id: Option<String>,
    principal_key: String,
    rpc_payloads: Mutex<Vec<Value>>,
}

#[async_trait]
impl RemoteClient for NoHeadRemote {
    async fn select_json(
        &self,
        table: &str,
        _query: &str,
        _token: &str,
    ) -> std::result::Result<Vec<Value>, SyncError> {
        assert_eq!(table, "authored_document_heads");
        Ok(self
            .server_head_revision_id
            .as_ref()
            .map(|revision_id| {
                vec![json!({
                    "document_id": self.document_id,
                    "principal_key": self.principal_key,
                    "revision_id": revision_id,
                })]
            })
            .unwrap_or_default())
    }

    async fn upsert_json(
        &self,
        _table: &str,
        _payload: &Value,
        _conflict_key: &str,
        _token: &str,
    ) -> std::result::Result<(), SyncError> {
        Ok(())
    }

    async fn rpc_json(
        &self,
        function: &str,
        payload: &Value,
        _token: &str,
    ) -> std::result::Result<Value, SyncError> {
        assert_eq!(function, "integrate_authored_head_proposal");
        self.rpc_payloads.lock().unwrap().push(payload.clone());
        Ok(json!({
            "proposal_id": self.proposal_id,
            "document_id": self.document_id,
            "outcome": "integrated",
            "proposal_status": "integrated",
            "current_head_revision_id": payload["result_revision_id"],
            "integrated_revision_id": payload["result_revision_id"],
            "resolution": payload["resolution"],
            "integration_seq": 1,
            "integrated_at": "2026-08-02T00:00:00Z"
        }))
    }

    async fn patch_json(
        &self,
        _table: &str,
        _filter: &str,
        _payload: &Value,
        _token: &str,
    ) -> std::result::Result<(), SyncError> {
        Ok(())
    }

    async fn upload_file(
        &self,
        _bucket: &str,
        path: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
        _token: &str,
    ) -> std::result::Result<String, SyncError> {
        Ok(path.to_owned())
    }

    async fn download_file(
        &self,
        _bucket: &str,
        _path: &str,
        _token: &str,
    ) -> std::result::Result<Vec<u8>, SyncError> {
        Ok(Vec::new())
    }
}

struct AdvancingHeadRemote {
    proposal_id: String,
    document_id: String,
    principal_key: String,
    first_head_revision_id: String,
    second_head_revision_id: String,
    rpc_count: Mutex<usize>,
    rpc_payloads: Mutex<Vec<Value>>,
}

#[async_trait]
impl RemoteClient for AdvancingHeadRemote {
    async fn select_json(
        &self,
        table: &str,
        _query: &str,
        _token: &str,
    ) -> std::result::Result<Vec<Value>, SyncError> {
        assert_eq!(table, "authored_document_heads");
        let revision_id = if *self.rpc_count.lock().unwrap() == 0 {
            &self.first_head_revision_id
        } else {
            &self.second_head_revision_id
        };
        Ok(vec![json!({
            "document_id": self.document_id,
            "principal_key": self.principal_key,
            "revision_id": revision_id,
        })])
    }

    async fn upsert_json(
        &self,
        _table: &str,
        _payload: &Value,
        _conflict_key: &str,
        _token: &str,
    ) -> std::result::Result<(), SyncError> {
        Ok(())
    }

    async fn rpc_json(
        &self,
        function: &str,
        payload: &Value,
        _token: &str,
    ) -> std::result::Result<Value, SyncError> {
        assert_eq!(function, "integrate_authored_head_proposal");
        self.rpc_payloads.lock().unwrap().push(payload.clone());
        let mut rpc_count = self.rpc_count.lock().unwrap();
        let response = if *rpc_count == 0 {
            json!({
                "proposal_id": self.proposal_id,
                "document_id": self.document_id,
                "outcome": "not_earliest",
                "proposal_status": "pending",
                "current_head_revision_id": self.first_head_revision_id,
                "integrated_revision_id": null,
                "resolution": null,
                "integration_seq": null,
                "integrated_at": null
            })
        } else {
            json!({
                "proposal_id": self.proposal_id,
                "document_id": self.document_id,
                "outcome": "integrated",
                "proposal_status": "integrated",
                "current_head_revision_id": payload["result_revision_id"],
                "integrated_revision_id": payload["result_revision_id"],
                "resolution": payload["resolution"],
                "integration_seq": 11,
                "integrated_at": "2026-08-02T00:00:01Z"
            })
        };
        *rpc_count += 1;
        Ok(response)
    }

    async fn patch_json(
        &self,
        _table: &str,
        _filter: &str,
        _payload: &Value,
        _token: &str,
    ) -> std::result::Result<(), SyncError> {
        Ok(())
    }

    async fn upload_file(
        &self,
        _bucket: &str,
        path: &str,
        _bytes: Vec<u8>,
        _content_type: &str,
        _token: &str,
    ) -> std::result::Result<String, SyncError> {
        Ok(path.to_owned())
    }

    async fn download_file(
        &self,
        _bucket: &str,
        _path: &str,
        _token: &str,
    ) -> std::result::Result<Vec<u8>, SyncError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn frozen_checkpoint_upgrades_and_bootstraps_terminal_routes() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("checkpoint-upgrade.sqlite");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&database)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    let all = sqlx::migrate!("./migrations");
    let checkpoint = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            all.iter()
                .filter(|migration| migration.version <= 20260802940000)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
        table_name: Cow::Borrowed("_sqlx_migrations"),
        create_schemas: Cow::Borrowed(&[]),
    };
    checkpoint.run(&pool).await.unwrap();
    crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"INSERT INTO venues (id, uid, name) VALUES ('venue-live', 'alice', 'Venue');
         INSERT INTO tracks (id, uid, track_hash, title, file_path)
         VALUES ('track-live', 'alice', 'hash-live', 'Track', '/tmp/track.wav');
         INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score-live', 'alice', 'track-live', 'venue-live', 'Score');

         INSERT INTO authored_state_projections
             (repository_id, document_kind, principal_key, subject_id, track_id,
              venue_id, score_id, implementation_id, projected_commit,
              materialization_state, updated_at)
         VALUES
             ('r-live', 'track_score', 'signed-in:alice', 'track-live',
              'track-live', 'venue-live', 'score-live', NULL, 'git-live',
              'present', '2026-08-01T12:00:00Z'),
             ('r-archived-a', 'pattern_graph', 'signed-in:alice', 'pattern-archived',
              NULL, NULL, NULL, 'implementation-a', 'git-a',
              'archived', '2026-08-01T12:00:00Z'),
             ('r-archived-b', 'pattern_graph', 'signed-in:alice', 'pattern-archived',
              NULL, NULL, NULL, 'implementation-b', 'git-b',
              'archived', '2026-08-01T12:00:00Z');

         INSERT INTO agent_threads
             (id, owner_user_id, agent_kind, subject_kind, subject_id,
              venue_id, score_id, title)
         VALUES
             ('thread-old', 'alice', 'track_copilot', 'track', 'track-live',
              'venue-live', 'score-live', 'Old thread');
         INSERT INTO agent_thread_messages
             (id, thread_id, seq, role, parts_json, created_at)
         VALUES
             ('message-one', 'thread-old', 0, 'user',
              '[{"type":"text","text":"one"}]', '2026-08-01T12:01:00Z'),
             ('message-two', 'thread-old', 1, 'user',
              '[{"type":"text","text":"two"}]', '2026-08-01T12:02:00Z');
         INSERT INTO agent_thread_message_appends
             (thread_id, operation_id, request_fingerprint, first_seq,
              message_count, created_at)
         VALUES
             ('thread-old', 'append-old', 'fingerprint-old', 0, 2,
              '2026-08-01T12:03:00Z');"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // This is also the frozen-checksum regression: sqlx rejects any modified
    // pre-boundary migration before it can apply the additive transition.
    all.run(&pool).await.unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name LIKE 'authored_state_%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, i64)>(
            "SELECT parent_message_id, depth FROM agent_thread_messages
             WHERE id = 'message-two'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (Some("message-one".into()), 1)
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, String, String, i64)>(
            "SELECT base_head_message_id, first_message_id,
                    result_head_message_id, message_count
             FROM agent_thread_message_appends
             WHERE thread_id = 'thread-old' AND operation_id = 'append-old'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (None, "message-one".into(), "message-two".into(), 2)
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, i64)>(
            "SELECT head_message_id, message_count
             FROM agent_thread_transcript_heads WHERE thread_id = 'thread-old'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (Some("message-two".into()), 2)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM relational_upgrade_archived_routes",)
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );

    let authored = AuthoredDocuments::new(StorageRoot::from_path(directory.path().join("state")));
    assert_eq!(
        authored
            .bootstrap_live_projections(&pool, Some("alice"))
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        authored
            .bootstrap_live_projections(&pool, Some("alice"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'relational_upgrade_archived_routes'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_documents WHERE archived_at IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_document_archives
             WHERE requested_revision_id IS NULL AND final_revision_id IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_revisions revision
             JOIN authored_documents document ON document.document_id = revision.document_id
             WHERE document.document_kind = 'track_score'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert!(sqlx::query(
        "INSERT INTO patterns (id, uid, name)
             VALUES ('pattern-archived', 'alice', 'stale')",
    )
    .execute(&pool)
    .await
    .unwrap_err()
    .to_string()
    .contains("cannot recreate an archived authored pattern"));
    let foreign_key_errors: Vec<(String, i64, String, i64)> =
        sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(foreign_key_errors.is_empty(), "{foreign_key_errors:?}");
}

fn empty_graph() -> Graph {
    Graph {
        nodes: vec![],
        edges: vec![],
        args: vec![],
    }
}

fn graph_with_node(id: &str, x: f64) -> Graph {
    Graph {
        nodes: vec![NodeInstance {
            id: id.into(),
            type_id: "view_signal".into(),
            params: HashMap::new(),
            position_x: Some(x),
            position_y: Some(0.0),
        }],
        edges: vec![],
        args: vec![],
    }
}

async fn enrich_proposal_sequence(pool: &SqlitePool, proposal_id: &str, sequence: i64) {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    write_admission::enter_remote_writes(&mut transaction)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE authored_head_proposals SET server_proposal_seq = ?
         WHERE proposal_id = ?",
    )
    .bind(sequence)
    .bind(proposal_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    write_admission::leave_remote_writes(&mut transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn graph_revision_projection_head_and_history_commit_atomically() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let initial = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, Some(20))
        .await
        .unwrap();
    assert_eq!(initial.entries.len(), 1);

    let applied = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            "implementation",
            "graph-edit-1",
            graph_with_node("view", 4.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Add view",
        )
        .await
        .unwrap();
    let history = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, Some(20))
        .await
        .unwrap();
    assert_eq!(history.entries[0].revision_id, applied.revision_id);
    assert_eq!(
        history.entries[0].position,
        AuthoredRevisionPosition::Current
    );
    assert_eq!(
        history.entries[1].position,
        AuthoredRevisionPosition::Ancestor
    );

    let (head, graph_json): (String, String) = sqlx::query_as(
        "SELECT head.revision_id, implementation.graph_json
         FROM authored_document_heads head
         JOIN authored_documents document ON document.document_id = head.document_id
         JOIN implementations implementation
           ON implementation.id = document.implementation_id
         WHERE document.subject_id = 'pattern'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(head, applied.revision_id);
    let projected: Graph = serde_json::from_str(&graph_json).unwrap();
    assert_eq!(projected.nodes[0].id, "view");
}

#[tokio::test]
async fn pulled_server_head_replaces_optimistic_tip_and_preserves_exact_clock() {
    let owner = "head-owner";
    let fixture = Fixture::signed_in(owner).await;
    let thread = fixture.pattern_thread().await;
    let initial = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(20))
        .await
        .unwrap()
        .entries
        .into_iter()
        .next()
        .unwrap()
        .revision_id;
    let optimistic = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            Some(owner),
            "pattern",
            "implementation",
            "optimistic-before-server-pull",
            graph_with_node("optimistic", 4.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Optimistic edit",
        )
        .await
        .unwrap();
    let document_id: String = sqlx::query_scalar(
        "SELECT document_id FROM authored_documents
         WHERE principal_key = 'signed-in:' || ? AND implementation_id = 'implementation'",
    )
    .bind(owner)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();

    fixture
        .authored
        .apply_server_head(
            &fixture.pool,
            owner,
            &document_id,
            &initial,
            7,
            "2026-08-02T01:02:03.456Z",
        )
        .await
        .unwrap();
    let projected: (String, i64, String) = sqlx::query_as(
        "SELECT revision_id, generation, updated_at
         FROM authored_document_heads WHERE document_id = ?",
    )
    .bind(&document_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        projected,
        (initial.clone(), 7, "2026-08-02T01:02:03.456Z".into())
    );
    let graph_json: String =
        sqlx::query_scalar("SELECT graph_json FROM implementations WHERE id = 'implementation'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert!(serde_json::from_str::<Graph>(&graph_json)
        .unwrap()
        .nodes
        .is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_revisions WHERE revision_id = ?",
        )
        .bind(&optimistic.revision_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        1,
        "the superseded optimistic tip remains immutable history"
    );

    // A repeated revision observation is still authoritative for the server
    // projection clock; no local timestamp trigger may rewrite these bytes.
    fixture
        .authored
        .apply_server_head(
            &fixture.pool,
            owner,
            &document_id,
            &initial,
            11,
            "2026-08-02T04:05:06Z",
        )
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_as::<_, (String, i64, String)>(
            "SELECT revision_id, generation, updated_at
             FROM authored_document_heads WHERE document_id = ?",
        )
        .bind(&document_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        (initial, 11, "2026-08-02T04:05:06Z".into())
    );
}

#[tokio::test]
async fn graph_operation_replay_rejects_an_unexpected_stored_result() {
    let fixture = Fixture::new().await;
    fixture.pattern_thread().await;
    let graph = graph_with_node("view", 4.0);
    fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            "implementation",
            "corrupt-replay",
            graph.clone(),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Add view",
        )
        .await
        .unwrap();

    // Simulate on-disk corruption behind the server-side/local immutability
    // guard. A replay whose operation type has no result payload must not
    // silently accept a payload from another operation shape.
    sqlx::query("DROP TRIGGER authored_operation_outcome_is_immutable")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE authored_operation_outcomes SET result_json = '{}'
         WHERE operation_kind = 'graph_edit' AND operation_id = 'corrupt-replay'",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let error = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            "implementation",
            "corrupt-replay",
            graph,
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Add view",
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AuthoredDocumentsError::Storage(message)
            if message.contains("does not match its idempotent replay")
    ));
}

#[tokio::test]
async fn isolated_workspace_commits_then_merges_cleanly_into_live_document() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let base = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, Some(1))
        .await
        .unwrap()
        .entries[0]
        .revision_id
        .clone();
    let workspace = fixture
        .authored
        .create_workspace(
            &fixture.pool,
            None,
            CreateAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "workspace-create".into(),
                expected_base_revision_id: base.clone(),
            },
        )
        .await
        .unwrap();
    std::fs::write(
        std::path::Path::new(&workspace.path).join(GRAPH_PATH),
        crate::services::graph_documents::semantic_graph_json(&graph_with_node("worker", 1.0))
            .unwrap(),
    )
    .unwrap();
    let check = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &workspace.id)
        .await
        .unwrap();
    assert!(check.changed);
    let committed = fixture
        .authored
        .commit_workspace(
            &fixture.pool,
            None,
            CommitAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                workspace_id: workspace.id.clone(),
                expected_head_revision_id: workspace.head_revision_id,
                expected_snapshot_id: check.snapshot_id,
                operation_id: "workspace-commit".into(),
                message: "Worker result".into(),
            },
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
                operation_id: "workspace-merge".into(),
            },
        )
        .await
        .unwrap();
    let crate::models::authored_state::AuthoredWorkspaceMerge::Merged {
        applied_to_current_projection,
        document,
        ..
    } = merged
    else {
        panic!("independent workspace edit should merge")
    };
    assert!(applied_to_current_projection);
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = document else {
        panic!("pattern workspace should return a graph")
    };
    assert_eq!(graph.nodes[0].id, "worker");
}

#[tokio::test]
async fn python_track_apply_advances_only_the_detached_workspace_until_merge() {
    let fixture = Fixture::new().await;
    let (thread, scope) = fixture.track_thread().await;
    sqlx::query(
        "INSERT INTO implementations (id, pattern_id, graph_json)
         VALUES ('track-workspace-pattern', 'pattern', ?)",
    )
    .bind(exact_graph_json(&empty_graph()).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let current = fixture
        .authored
        .current_revision(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let AuthoredProjectedDocument::TrackScore { revision } = current.document else {
        panic!("track thread should project a score")
    };
    let workspace = fixture
        .authored
        .create_workspace(
            &fixture.pool,
            None,
            CreateAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "python-track-workspace".into(),
                expected_base_revision_id: current.revision_id,
            },
        )
        .await
        .unwrap();
    let stale_materialization =
        std::fs::read(std::path::Path::new(&workspace.path).join(SCORE_PATH)).unwrap();

    let applied = fixture
        .authored
        .apply_track_workspace_edit(
            &fixture.pool,
            None,
            &thread.id,
            &workspace.id,
            &scope,
            "python-workspace-apply",
            "python-workspace-fingerprint",
            TrackEditPlan {
                base_revision: revision,
                candidate: vec![TrackClip {
                    id: "new:1".into(),
                    pattern_id: "pattern".into(),
                    start_time: 1.0,
                    end_time: 2.0,
                    z_index: 0,
                    blend_mode: BlendMode::Replace,
                    args: json!({}),
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(applied.added, 1);
    assert_eq!(applied.id_map.get("new:1"), Some(&applied.clips[0].id));
    assert!(uuid::Uuid::parse_str(&applied.clips[0].id).is_ok());

    let mut updated_clip = applied.clips[0].clone();
    updated_clip.end_time = 3.0;
    let updated = fixture
        .authored
        .apply_track_workspace_edit(
            &fixture.pool,
            None,
            &thread.id,
            &workspace.id,
            &scope,
            "python-workspace-update",
            "python-workspace-update-fingerprint",
            TrackEditPlan {
                base_revision: applied.revision.clone(),
                candidate: vec![updated_clip],
            },
        )
        .await
        .unwrap();
    assert!(updated.applied_to_current_projection);
    let replayed = fixture
        .authored
        .replay_track_workspace_edit(
            &fixture.pool,
            None,
            &thread.id,
            &workspace.id,
            &scope,
            "python-workspace-apply",
            "python-workspace-fingerprint",
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!replayed.applied_to_current_projection);

    let live_before_merge: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_scores WHERE score_id = ?")
            .bind(&scope.score_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(live_before_merge, 0);

    // A lost filesystem response after the detached revision commits must not
    // make supervisor finalization commit stale source over the structured edit.
    std::fs::write(
        std::path::Path::new(&workspace.path).join(SCORE_PATH),
        stale_materialization,
    )
    .unwrap();

    let check = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &workspace.id)
        .await
        .unwrap();
    assert!(
        !check.changed,
        "domain apply materializes its detached head"
    );
    assert_ne!(check.head_revision_id, workspace.base_revision_id);
    let AuthoredProjectedDocument::TrackScore { revision } = check.document else {
        panic!("track workspace should project a score")
    };
    assert_eq!(revision, updated.revision);

    // The detached revision is durable even if publication crashes after the
    // old materialization is removed but before its replacement is renamed.
    std::fs::remove_dir_all(&workspace.path).unwrap();
    let recovered = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &workspace.id)
        .await
        .unwrap();
    assert!(!recovered.changed);
    assert_eq!(recovered.head_revision_id, check.head_revision_id);
    assert!(std::path::Path::new(&workspace.path)
        .join(SCORE_PATH)
        .is_file());

    let merged = fixture
        .authored
        .merge_workspace(
            &fixture.pool,
            None,
            MergeAuthoredWorkspaceInput {
                thread_id: thread.id,
                workspace_id: workspace.id,
                expected_head_revision_id: recovered.head_revision_id,
                operation_id: "python-workspace-merge".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        merged,
        crate::models::authored_state::AuthoredWorkspaceMerge::Merged { .. }
    ));
    let live_after_merge: Vec<String> =
        sqlx::query_scalar("SELECT id FROM track_scores WHERE score_id = ?")
            .bind(&scope.score_id)
            .fetch_all(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(live_after_merge, vec![applied.clips[0].id.clone()]);
}

#[tokio::test]
async fn structured_graph_workspace_write_updates_both_files_and_rejects_invalid_graphs() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let current = fixture
        .authored
        .current_revision(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let workspace = fixture
        .authored
        .create_workspace(
            &fixture.pool,
            None,
            CreateAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "structured-graph-write".into(),
                expected_base_revision_id: current.revision_id,
            },
        )
        .await
        .unwrap();

    let written = fixture
        .authored
        .write_workspace_graph(
            &fixture.pool,
            None,
            &thread.id,
            &workspace.id,
            &graph_with_node("structured", 37.0),
        )
        .await
        .unwrap();
    assert_eq!(written.nodes[0].id, "structured");
    assert_eq!(written.nodes[0].position_x, Some(37.0));

    let check = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &workspace.id)
        .await
        .unwrap();
    assert!(check.changed);
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = check.document else {
        panic!("pattern workspace should project a graph")
    };
    assert_eq!(graph.nodes[0].id, "structured");
    assert_eq!(graph.nodes[0].position_x, Some(37.0));

    let mut invalid = graph_with_node("duplicate", 1.0);
    invalid.nodes.push(invalid.nodes[0].clone());
    fixture
        .authored
        .write_workspace_graph(&fixture.pool, None, &thread.id, &workspace.id, &invalid)
        .await
        .unwrap_err();
    let after_rejection = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &workspace.id)
        .await
        .unwrap();
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = after_rejection.document else {
        panic!("pattern workspace should project a graph")
    };
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].id, "structured");
    assert_eq!(graph.nodes[0].position_x, Some(37.0));
}

#[tokio::test]
async fn recursive_workspace_forks_from_the_parent_detached_graph() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let current = fixture
        .authored
        .current_revision(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let parent = fixture
        .authored
        .create_workspace(
            &fixture.pool,
            None,
            CreateAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "recursive-parent".into(),
                expected_base_revision_id: current.revision_id,
            },
        )
        .await
        .unwrap();
    fixture
        .authored
        .write_workspace_graph(
            &fixture.pool,
            None,
            &thread.id,
            &parent.id,
            &graph_with_node("parent-node", 12.0),
        )
        .await
        .unwrap();
    let parent_check = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &parent.id)
        .await
        .unwrap();
    let parent_checkpoint = fixture
        .authored
        .commit_workspace(
            &fixture.pool,
            None,
            CommitAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                workspace_id: parent.id.clone(),
                expected_head_revision_id: parent_check.head_revision_id,
                expected_snapshot_id: parent_check.snapshot_id,
                operation_id: "recursive-parent-checkpoint".into(),
                message: "Checkpoint parent".into(),
            },
        )
        .await
        .unwrap();

    let child = fixture
        .authored
        .fork_workspace(
            &fixture.pool,
            None,
            ForkAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "recursive-child".into(),
                source_workspace_id: parent.id.clone(),
            },
        )
        .await
        .unwrap();
    let child_check = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &child.id)
        .await
        .unwrap();
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = child_check.document else {
        panic!("pattern workspace should project a graph")
    };
    assert_eq!(graph.nodes[0].id, "parent-node");

    fixture
        .authored
        .write_workspace_graph(
            &fixture.pool,
            None,
            &thread.id,
            &child.id,
            &graph_with_node("child-node", 24.0),
        )
        .await
        .unwrap();
    let child_check = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &child.id)
        .await
        .unwrap();
    let child_commit = fixture
        .authored
        .commit_workspace(
            &fixture.pool,
            None,
            CommitAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                workspace_id: child.id.clone(),
                expected_head_revision_id: child_check.head_revision_id,
                expected_snapshot_id: child_check.snapshot_id,
                operation_id: "recursive-child-commit".into(),
                message: "Child result".into(),
            },
        )
        .await
        .unwrap();
    let nested_merge = fixture
        .authored
        .merge_workspace_into_workspace(
            &fixture.pool,
            None,
            MergeAuthoredWorkspaceIntoWorkspaceInput {
                thread_id: thread.id.clone(),
                workspace_id: child.id.clone(),
                target_workspace_id: parent.id.clone(),
                expected_head_revision_id: child_commit.revision_id.clone(),
                operation_id: "recursive-child-merge".into(),
            },
        )
        .await
        .unwrap();
    let AuthoredWorkspaceMerge::Merged {
        revision_id: nested_revision,
        applied_to_current_projection,
        document: AuthoredProjectedDocument::PatternGraph { graph, .. },
        ..
    } = nested_merge
    else {
        panic!("recursive child should merge into its parent workspace")
    };
    assert!(!applied_to_current_projection);
    assert_eq!(graph.nodes[0].id, "child-node");

    let parent_after_child = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &parent.id)
        .await
        .unwrap();
    assert!(!parent_after_child.changed);
    assert_eq!(parent_after_child.head_revision_id, nested_revision);
    assert_ne!(
        parent_after_child.head_revision_id,
        parent_checkpoint.revision_id
    );

    let live = fixture
        .authored
        .current_revision(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = live.document else {
        panic!("pattern thread should project a graph")
    };
    assert!(graph.nodes.is_empty());

    let published = fixture
        .authored
        .merge_workspace(
            &fixture.pool,
            None,
            MergeAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                workspace_id: parent.id.clone(),
                expected_head_revision_id: parent_after_child.head_revision_id,
                operation_id: "recursive-parent-merge".into(),
            },
        )
        .await
        .unwrap();
    let AuthoredWorkspaceMerge::Merged {
        document: AuthoredProjectedDocument::PatternGraph { graph, .. },
        ..
    } = published
    else {
        panic!("root workspace should publish the recursive result")
    };
    assert_eq!(graph.nodes[0].id, "child-node");

    fixture
        .authored
        .write_workspace_graph(
            &fixture.pool,
            None,
            &thread.id,
            &parent.id,
            &graph_with_node("later-parent-edit", 48.0),
        )
        .await
        .unwrap();
    let replay_error = fixture
        .authored
        .merge_workspace_into_workspace(
            &fixture.pool,
            None,
            MergeAuthoredWorkspaceIntoWorkspaceInput {
                thread_id: thread.id.clone(),
                workspace_id: child.id,
                target_workspace_id: parent.id.clone(),
                expected_head_revision_id: child_commit.revision_id,
                operation_id: "recursive-child-merge".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        replay_error,
        AuthoredDocumentsError::Invalid(message) if message.contains("uncommitted changes")
    ));
    let dirty_parent = fixture
        .authored
        .check_workspace(&fixture.pool, None, &thread.id, &parent.id)
        .await
        .unwrap();
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = dirty_parent.document else {
        panic!("parent workspace should preserve its later graph edit")
    };
    assert_eq!(graph.nodes[0].id, "later-parent-edit");
}

#[tokio::test]
async fn thread_deletion_exposes_every_child_execution_namespace_for_cleanup() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let current = fixture
        .authored
        .current_revision(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let parent = fixture
        .authored
        .create_workspace(
            &fixture.pool,
            None,
            CreateAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "cleanup-parent".into(),
                expected_base_revision_id: current.revision_id,
            },
        )
        .await
        .unwrap();
    let child = fixture
        .authored
        .fork_workspace(
            &fixture.pool,
            None,
            ForkAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "cleanup-child".into(),
                source_workspace_id: parent.id.clone(),
            },
        )
        .await
        .unwrap();
    let cleaned = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&cleaned);

    fixture
        .authored
        .delete_thread_with_authored_state(&fixture.pool, None, &thread.id, move |workspace_ids| {
            let captured = Arc::clone(&captured);
            async move {
                *captured.lock().unwrap() = workspace_ids;
                Ok(())
            }
        })
        .await
        .unwrap();

    let mut actual = cleaned.lock().unwrap().clone();
    actual.sort();
    let mut expected = vec![parent.id, child.id];
    expected.sort();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn structured_graph_workspace_write_rejects_track_workspaces() {
    let fixture = Fixture::new().await;
    let (thread, _) = fixture.track_thread().await;
    let current = fixture
        .authored
        .current_revision(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let workspace = fixture
        .authored
        .create_workspace(
            &fixture.pool,
            None,
            CreateAuthoredWorkspaceInput {
                thread_id: thread.id.clone(),
                request_id: "track-rejects-graph-write".into(),
                expected_base_revision_id: current.revision_id,
            },
        )
        .await
        .unwrap();

    let error = fixture
        .authored
        .write_workspace_graph(
            &fixture.pool,
            None,
            &thread.id,
            &workspace.id,
            &empty_graph(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AuthoredDocumentsError::Scope(message)
            if message.contains("require a pattern agent thread")
    ));
}

#[tokio::test]
async fn failed_workspace_materialization_removes_reservation_and_can_retry() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let current = fixture
        .authored
        .current_revision(&fixture.pool, None, &thread.id)
        .await
        .unwrap();
    let document_directory = fixture
        .authored
        .storage
        .authored_document_workspaces_dir(&current.document_id);
    std::fs::create_dir_all(document_directory.parent().unwrap()).unwrap();
    std::fs::write(&document_directory, b"block workspace directory creation").unwrap();
    let input = CreateAuthoredWorkspaceInput {
        thread_id: thread.id.clone(),
        request_id: "materialization-retry".into(),
        expected_base_revision_id: current.revision_id,
    };

    fixture
        .authored
        .create_workspace(&fixture.pool, None, input.clone())
        .await
        .unwrap_err();
    let reservations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_subagent_workspaces
         WHERE owner_thread_id = ? AND request_id = ?",
    )
    .bind(&thread.id)
    .bind(&input.request_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(reservations, 0);

    std::fs::remove_file(&document_directory).unwrap();
    let retried = fixture
        .authored
        .create_workspace(&fixture.pool, None, input)
        .await
        .unwrap();
    assert!(!retried.id.is_empty());
}

#[tokio::test]
async fn parented_proposal_against_missing_server_head_integrates_whole_tip() {
    let owner = "sync-owner";
    let fixture = Fixture::signed_in(owner).await;
    let thread = fixture.pattern_thread().await;
    let initial = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(1))
        .await
        .unwrap()
        .entries[0]
        .revision_id
        .clone();
    let applied = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            Some(owner),
            "pattern",
            "implementation",
            "parented-no-server-head",
            graph_with_node("offline", 3.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Offline graph edit",
        )
        .await
        .unwrap();
    let (proposal_id, document_id, base_revision_id, proposed_revision_id): (
        String,
        String,
        Option<String>,
        String,
    ) = sqlx::query_as(
        "SELECT proposal_id, document_id, base_revision_id, proposed_revision_id
         FROM authored_head_proposals WHERE operation_id = 'parented-no-server-head'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(base_revision_id.as_deref(), Some(initial.as_str()));
    assert_eq!(proposed_revision_id, applied.revision_id);
    enrich_proposal_sequence(&fixture.pool, &proposal_id, 7).await;

    let remote = NoHeadRemote {
        proposal_id: proposal_id.clone(),
        document_id: document_id.clone(),
        server_head_revision_id: None,
        principal_key: format!("signed-in:{owner}"),
        rpc_payloads: Mutex::new(Vec::new()),
    };
    let receipt = fixture
        .authored
        .integrate_pending_proposal(&fixture.pool, &remote, "token", owner, &proposal_id)
        .await
        .unwrap();
    assert!(receipt.is_terminal());
    assert_eq!(receipt.outcome, HeadIntegrationOutcome::Integrated);
    assert_eq!(
        receipt.resolution,
        Some(HeadIntegrationResolution::WholeProposal)
    );
    assert_eq!(
        receipt.current_head_revision_id.as_deref(),
        Some(proposed_revision_id.as_str())
    );
    {
        let payloads = remote.rpc_payloads.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["expected_head_revision_id"], Value::Null);
        assert_eq!(payloads[0]["resolution"], "whole_proposal");
        assert_eq!(payloads[0]["result_revision_id"], proposed_revision_id);
    }

    let local_head: String =
        sqlx::query_scalar("SELECT revision_id FROM authored_document_heads WHERE document_id = ?")
            .bind(document_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(local_head, applied.revision_id);
}

#[tokio::test]
async fn structural_score_merge_preserves_current_trivia_and_proposal_semantics() {
    let owner = "score-trivia-owner";
    let fixture = Fixture::signed_in(owner).await;
    let (thread, track_scope) = fixture.track_thread().await;
    let (document_id, base_revision_id): (String, String) = sqlx::query_as(
        "SELECT document.document_id, head.revision_id
         FROM authored_documents document
         JOIN authored_document_heads head ON head.document_id = document.document_id
         WHERE document.principal_key = 'signed-in:' || ? AND document.score_id = 'score'",
    )
    .bind(owner)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();

    let proposal = fixture
        .authored
        .apply_track_edit_for_thread(
            &fixture.pool,
            Some(owner),
            &thread.id,
            &track_scope,
            "trivia-proposal-edit",
            "trivia-proposal-fingerprint",
            TrackEditPlan {
                base_revision: revision_for_clips(&[]),
                candidate: vec![TrackClip {
                    id: "new:proposal-clip".into(),
                    pattern_id: "pattern".into(),
                    start_time: 0.0,
                    end_time: 1.0,
                    z_index: 0,
                    blend_mode: BlendMode::Replace,
                    args: json!({}),
                }],
            },
            "Proposal semantic edit",
        )
        .await
        .unwrap();
    let (proposal_id, proposed_revision_id): (String, String) = sqlx::query_as(
        "SELECT proposal_id, proposed_revision_id
         FROM authored_head_proposals WHERE operation_id = 'trivia-proposal-edit'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(proposed_revision_id, proposal.authored.revision_id);
    enrich_proposal_sequence(&fixture.pool, &proposal_id, 7).await;

    let parsed_document_id = AuthoredDocumentId::parse(document_id.clone()).unwrap();
    let parsed_base_revision_id = RevisionId::parse(base_revision_id.clone()).unwrap();
    let base_files = {
        let mut connection = fixture.pool.acquire().await.unwrap();
        fixture
            .authored
            .store
            .read_revision(
                &mut connection,
                &parsed_document_id,
                &parsed_base_revision_id,
            )
            .await
            .unwrap()
            .1
    };
    let base_source = std::str::from_utf8(base_files.get(SCORE_PATH).unwrap()).unwrap();
    let current_files = FileMap::from([(
        SCORE_PATH.to_owned(),
        format!("{base_source}\n# current-only comment").into_bytes(),
    )]);
    let mut transaction = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let current_revision = fixture
        .authored
        .store
        .insert_revision(
            &mut transaction,
            &parsed_document_id,
            std::slice::from_ref(&parsed_base_revision_id),
            &current_files,
            &revision_metadata(
                "edit",
                Some("current-comment-only"),
                "Current comment-only edit",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let remote = NoHeadRemote {
        proposal_id: proposal_id.clone(),
        document_id: document_id.clone(),
        server_head_revision_id: Some(current_revision.id.to_string()),
        principal_key: format!("signed-in:{owner}"),
        rpc_payloads: Mutex::new(Vec::new()),
    };
    let receipt = fixture
        .authored
        .integrate_pending_proposal(&fixture.pool, &remote, "token", owner, &proposal_id)
        .await
        .unwrap();
    assert_eq!(receipt.outcome, HeadIntegrationOutcome::Integrated);
    assert_eq!(
        receipt.resolution,
        Some(HeadIntegrationResolution::Structural)
    );
    let merged_revision_id = RevisionId::parse(receipt.current_head_revision_id.unwrap()).unwrap();
    assert_ne!(merged_revision_id, current_revision.id);
    assert_ne!(merged_revision_id.as_str(), proposed_revision_id);

    let merged_files = {
        let mut connection = fixture.pool.acquire().await.unwrap();
        fixture
            .authored
            .store
            .read_revision(&mut connection, &parsed_document_id, &merged_revision_id)
            .await
            .unwrap()
            .1
    };
    let merged_source = std::str::from_utf8(merged_files.get(SCORE_PATH).unwrap()).unwrap();
    assert!(merged_source.contains("# current-only comment"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM track_scores WHERE score_id = 'score' AND pattern_id = 'pattern'",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        1,
        "proposal semantic edit must survive alongside current-side trivia"
    );
    let payloads = remote.rpc_payloads.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["resolution"], "structural");
    assert_eq!(
        payloads[0]["result_revision_id"],
        merged_revision_id.to_string()
    );
}

#[tokio::test]
async fn malformed_legacy_proposal_metadata_terminally_falls_back() {
    let owner = "metadata-owner";
    let fixture = Fixture::signed_in(owner).await;
    let thread = fixture.pattern_thread().await;
    let initial = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(1))
        .await
        .unwrap()
        .entries[0]
        .revision_id
        .clone();
    let proposed = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            Some(owner),
            "pattern",
            "implementation",
            "metadata-proposal-edit",
            graph_with_node("proposal-node", 1.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Proposal edit",
        )
        .await
        .unwrap();
    let document_id: String = sqlx::query_scalar(
        "SELECT document_id FROM authored_documents
         WHERE principal_key = 'signed-in:' || ?
           AND subject_id = 'pattern' AND implementation_id = 'implementation'",
    )
    .bind(owner)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let parsed_document_id = AuthoredDocumentId::parse(document_id.clone()).unwrap();
    let initial_id = RevisionId::parse(initial.clone()).unwrap();
    let server_graph = graph_with_node("server-node", 2.0);
    let server_files = graph_files(&server_graph).unwrap();
    let mut transaction = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let server_revision = fixture
        .authored
        .store
        .insert_revision(
            &mut transaction,
            &parsed_document_id,
            std::slice::from_ref(&initial_id),
            &server_files,
            &revision_metadata(
                "sync_integration",
                Some("metadata-server-head"),
                "Server head",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let proposal_id = "legacy invalid/proposal id";
    sqlx::query(
        "INSERT INTO authored_head_proposals
         (proposal_id, principal_key, document_id, device_id, operation_id,
          base_revision_id, proposed_revision_id, created_at, server_proposal_seq)
         VALUES (?, 'signed-in:' || ?, ?, 'legacy-device', 'legacy-operation',
                 ?, ?, 'not-an-rfc3339-time', 9)",
    )
    .bind(proposal_id)
    .bind(owner)
    .bind(&document_id)
    .bind(&initial)
    .bind(&proposed.revision_id)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let remote = NoHeadRemote {
        proposal_id: proposal_id.into(),
        document_id: document_id.clone(),
        server_head_revision_id: Some(server_revision.id.to_string()),
        principal_key: format!("signed-in:{owner}"),
        rpc_payloads: Mutex::new(Vec::new()),
    };
    let receipt = fixture
        .authored
        .integrate_pending_proposal(&fixture.pool, &remote, "token", owner, proposal_id)
        .await
        .unwrap();
    assert!(receipt.is_terminal());
    assert_eq!(
        receipt.resolution,
        Some(HeadIntegrationResolution::WholeProposal)
    );
    {
        let payloads = remote.rpc_payloads.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["resolution"], "whole_proposal");
        assert_eq!(payloads[0]["result_revision_id"], proposed.revision_id);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_revisions
             WHERE document_id = ? AND operation_kind = 'sync_integration'
               AND operation_id != 'metadata-server-head'",
        )
        .bind(&document_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0,
        "malformed audit metadata must fall back before persisting a structural candidate"
    );
}

#[tokio::test]
async fn not_earliest_retry_persists_a_new_head_specific_merge_candidate() {
    let owner = "retry-owner";
    let fixture = Fixture::signed_in(owner).await;
    let thread = fixture.pattern_thread().await;
    let initial = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(1))
        .await
        .unwrap()
        .entries[0]
        .revision_id
        .clone();
    let proposed = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            Some(owner),
            "pattern",
            "implementation",
            "retry-proposal-edit",
            graph_with_node("proposal-node", 1.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Proposal edit",
        )
        .await
        .unwrap();
    let (proposal_id, document_id): (String, String) = sqlx::query_as(
        "SELECT proposal_id, document_id FROM authored_head_proposals
         WHERE operation_id = 'retry-proposal-edit'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    enrich_proposal_sequence(&fixture.pool, &proposal_id, 7).await;

    let parsed_document_id = AuthoredDocumentId::parse(document_id.clone()).unwrap();
    let initial_id = RevisionId::parse(initial).unwrap();
    let mut transaction = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let first_server_files = graph_files(&graph_with_node("server-one", 2.0)).unwrap();
    let first_server = fixture
        .authored
        .store
        .insert_revision(
            &mut transaction,
            &parsed_document_id,
            std::slice::from_ref(&initial_id),
            &first_server_files,
            &revision_metadata(
                "edit",
                Some("earlier-server-one"),
                "Earlier server proposal",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let second_server_files = graph_files(&graph_with_node("server-two", 3.0)).unwrap();
    let second_server = fixture
        .authored
        .store
        .insert_revision(
            &mut transaction,
            &parsed_document_id,
            std::slice::from_ref(&first_server.id),
            &second_server_files,
            &revision_metadata(
                "edit",
                Some("earlier-server-two"),
                "Earlier server proposal advanced",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let remote = AdvancingHeadRemote {
        proposal_id: proposal_id.clone(),
        document_id: document_id.clone(),
        principal_key: format!("signed-in:{owner}"),
        first_head_revision_id: first_server.id.to_string(),
        second_head_revision_id: second_server.id.to_string(),
        rpc_count: Mutex::new(0),
        rpc_payloads: Mutex::new(Vec::new()),
    };
    let first = fixture
        .authored
        .integrate_pending_proposal(&fixture.pool, &remote, "token", owner, &proposal_id)
        .await
        .unwrap();
    assert_eq!(first.outcome, HeadIntegrationOutcome::NotEarliest);
    assert!(!first.is_terminal());
    let second = fixture
        .authored
        .integrate_pending_proposal(&fixture.pool, &remote, "token", owner, &proposal_id)
        .await
        .unwrap();
    assert_eq!(second.outcome, HeadIntegrationOutcome::Integrated);
    assert_eq!(
        second.resolution,
        Some(HeadIntegrationResolution::Structural)
    );

    {
        let payloads = remote.rpc_payloads.lock().unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["resolution"], "structural");
        assert_eq!(payloads[1]["resolution"], "structural");
        assert_eq!(
            payloads[0]["expected_head_revision_id"],
            first_server.id.to_string()
        );
        assert_eq!(
            payloads[1]["expected_head_revision_id"],
            second_server.id.to_string()
        );
        assert_ne!(
            payloads[0]["result_revision_id"],
            payloads[1]["result_revision_id"]
        );
    }

    let candidates: Vec<(String, String)> = sqlx::query_as(
        "SELECT revision_id, operation_id FROM authored_revisions
         WHERE document_id = ? AND operation_kind = 'sync_integration'
         ORDER BY revision_id",
    )
    .bind(&document_id)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(candidates.len(), 2);
    assert_ne!(candidates[0].0, candidates[1].0);
    assert_ne!(candidates[0].1, candidates[1].1);
    let local_head: String =
        sqlx::query_scalar("SELECT revision_id FROM authored_document_heads WHERE document_id = ?")
            .bind(&document_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(
        second.current_head_revision_id.as_deref(),
        Some(local_head.as_str())
    );
    assert_ne!(second.current_head_revision_id, Some(proposed.revision_id));
}

#[tokio::test]
async fn another_device_discovers_and_integrates_an_offline_proposal() {
    let owner = "sync-owner";
    let fixture = Fixture::signed_in(owner).await;
    let thread = fixture.pattern_thread().await;
    let initial = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(1))
        .await
        .unwrap()
        .entries[0]
        .revision_id
        .clone();
    let applied = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            Some(owner),
            "pattern",
            "implementation",
            "device-b-local-edit",
            graph_with_node("offline", 3.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Offline graph edit",
        )
        .await
        .unwrap();
    let document_id: String = sqlx::query_scalar(
        "SELECT document_id FROM authored_documents
         WHERE principal_key = 'signed-in:' || ?
           AND subject_id = 'pattern' AND implementation_id = 'implementation'",
    )
    .bind(owner)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let current_device: String =
        sqlx::query_scalar("SELECT device_id FROM authored_device_identity WHERE singleton = 1")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let offline_device = "permanently-offline-device-a";
    assert_ne!(current_device, offline_device);

    // Model the row produced by pulling device A's already-submitted proposal.
    // The same immutable revision closure is present locally, while the
    // proposal's device identity is deliberately not this client's identity.
    let proposal_id = "offline-device-a-proposal";
    sqlx::query(
        "INSERT INTO authored_head_proposals
         (proposal_id, principal_key, document_id, device_id, operation_id,
          base_revision_id, proposed_revision_id, created_at, server_proposal_seq)
         VALUES (?, 'signed-in:' || ?, ?, ?, 'offline-device-a-operation',
                 ?, ?, '2026-08-02T00:00:00Z', 7)",
    )
    .bind(proposal_id)
    .bind(owner)
    .bind(&document_id)
    .bind(offline_device)
    .bind(&initial)
    .bind(&applied.revision_id)
    .execute(&fixture.pool)
    .await
    .unwrap();

    // An unrelated partial immutable upload in the same document must not
    // poison ancestry validation for this closed proposal.
    sqlx::query(
        "INSERT INTO authored_revisions
         (revision_id, document_id, principal_key, parent_count, content_hash,
          operation_kind, message, author_name, author_email, authored_at)
         VALUES (?, ?, 'signed-in:' || ?, 1, 'sha256:partial', 'edit',
                 'Unrelated partial upload', 'Luma Sync',
                 'authored-sync@luma.local', '2026-08-02T00:00:00Z')",
    )
    .bind(format!("rv-{}", "e".repeat(64)))
    .bind(&document_id)
    .bind(owner)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let queued = fixture
        .authored
        .enqueue_pending_head_integrations(&fixture.pool, owner)
        .await
        .unwrap();
    assert_eq!(queued, 1);
    let (queued_principal, queued_type): (String, String) = sqlx::query_as(
        "SELECT principal_key, op_type FROM pending_ops
         WHERE table_name = 'authored_head_authority' AND record_id = ?",
    )
    .bind(proposal_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(queued_principal, format!("signed-in:{owner}"));
    assert_eq!(
        queued_type,
        crate::sync::authored_remote::INTEGRATE_HEAD_PROPOSAL_OP
    );

    let remote = NoHeadRemote {
        proposal_id: proposal_id.into(),
        document_id: document_id.clone(),
        server_head_revision_id: Some(initial.clone()),
        principal_key: format!("signed-in:{owner}"),
        rpc_payloads: Mutex::new(Vec::new()),
    };
    let receipt = fixture
        .authored
        .integrate_pending_proposal(&fixture.pool, &remote, "token", owner, proposal_id)
        .await
        .unwrap();
    assert!(receipt.is_terminal());
    assert_eq!(receipt.outcome, HeadIntegrationOutcome::Integrated);
    assert_eq!(
        receipt.current_head_revision_id.as_deref(),
        Some(applied.revision_id.as_str())
    );
}

#[tokio::test]
async fn score_archive_replays_after_its_catalog_projection_is_gone() {
    let owner = "archive-owner";
    let fixture = Fixture::signed_in(owner).await;
    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, file_path)
         VALUES ('track', ?, 'track-hash', '/track')",
    )
    .bind(owner)
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', ?, 'Venue')")
        .bind(owner)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score', ?, 'track', 'venue', 'Score')",
    )
    .bind(owner)
    .execute(&fixture.pool)
    .await
    .unwrap();

    fixture
        .authored
        .archive_score(&fixture.pool, Some(owner), "score")
        .await
        .unwrap();
    let first_receipt: (String, String, String) = sqlx::query_as(
        "SELECT archive.archive_id, archive.operation_id, archive.device_id
         FROM authored_document_archives archive
         JOIN authored_documents document ON document.document_id = archive.document_id
         WHERE document.score_id = 'score' AND document.principal_key = 'signed-in:' || ?",
    )
    .bind(owner)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = 'score'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        0
    );

    // Simulate a lost successful IPC response: the retry has only the stable
    // catalog identity, while the live row it used on the first attempt is gone.
    fixture
        .authored
        .archive_score(&fixture.pool, Some(owner), "score")
        .await
        .unwrap();
    let receipts: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT archive.archive_id, archive.operation_id, archive.device_id
         FROM authored_document_archives archive
         JOIN authored_documents document ON document.document_id = archive.document_id
         WHERE document.score_id = 'score' AND document.principal_key = 'signed-in:' || ?",
    )
    .bind(owner)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(receipts, vec![first_receipt]);
}

#[tokio::test]
async fn multi_implementation_pattern_archive_replays_only_when_every_route_is_terminal() {
    let owner = "archive-owner";
    let fixture = Fixture::signed_in(owner).await;
    sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', ?, 'Pattern')")
        .bind(owner)
        .execute(&fixture.pool)
        .await
        .unwrap();
    for implementation_id in ["implementation-a", "implementation-b"] {
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES (?, ?, 'pattern', ?)",
        )
        .bind(implementation_id)
        .bind(owner)
        .bind(exact_graph_json(&empty_graph()).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();
    }

    fixture
        .authored
        .archive_pattern(&fixture.pool, Some(owner), "pattern")
        .await
        .unwrap();
    let first_receipts: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT archive.archive_id, archive.operation_id, archive.document_id
         FROM authored_document_archives archive
         JOIN authored_documents document ON document.document_id = archive.document_id
         WHERE document.subject_id = 'pattern'
           AND document.principal_key = 'signed-in:' || ?
         ORDER BY archive.document_id",
    )
    .bind(owner)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(first_receipts.len(), 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM authored_documents
             WHERE subject_id = 'pattern' AND archived_at IS NOT NULL",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        2
    );

    fixture
        .authored
        .archive_pattern(&fixture.pool, Some(owner), "pattern")
        .await
        .unwrap();
    let replayed_receipts: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT archive.archive_id, archive.operation_id, archive.document_id
         FROM authored_document_archives archive
         JOIN authored_documents document ON document.document_id = archive.document_id
         WHERE document.subject_id = 'pattern'
           AND document.principal_key = 'signed-in:' || ?
         ORDER BY archive.document_id",
    )
    .bind(owner)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(replayed_receipts, first_receipts);
}

#[test]
fn archive_receipt_identity_is_stable_per_device_and_distinct_across_devices() {
    let first = catalog::archive_request_ids(
        "signed-in:owner",
        "score",
        "score-id",
        "document-id",
        "device-a",
    );
    assert_eq!(
        first,
        catalog::archive_request_ids(
            "signed-in:owner",
            "score",
            "score-id",
            "document-id",
            "device-a",
        )
    );
    assert_ne!(
        first,
        catalog::archive_request_ids(
            "signed-in:owner",
            "score",
            "score-id",
            "document-id",
            "device-b",
        )
    );
}

#[tokio::test]
async fn superseded_proposal_tip_is_visible_and_restorable_as_a_forward_revision() {
    let owner = "history-owner";
    let fixture = Fixture::signed_in(owner).await;
    let thread = fixture.pattern_thread().await;
    let initial = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(1))
        .await
        .unwrap()
        .entries[0]
        .revision_id
        .clone();
    let (initial_proposal_id, initial_document_id): (String, String) = sqlx::query_as(
        "SELECT proposal_id, document_id FROM authored_head_proposals
         WHERE proposed_revision_id = ?",
    )
    .bind(&initial)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    enrich_proposal_sequence(&fixture.pool, &initial_proposal_id, 1).await;
    sqlx::query(
        "INSERT INTO authored_head_integrations
         (proposal_id, principal_key, document_id, prior_revision_id,
          result_revision_id, resolution_kind, server_integration_seq, integrated_at)
         VALUES (?, 'signed-in:' || ?, ?, NULL, ?, 'fast_forward', 2,
                 '2026-08-02T00:00:00Z')",
    )
    .bind(&initial_proposal_id)
    .bind(owner)
    .bind(&initial_document_id)
    .bind(&initial)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let local_tip = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            Some(owner),
            "pattern",
            "implementation",
            "superseded-local-edit",
            graph_with_node("superseded", 1.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Local proposal",
        )
        .await
        .unwrap();
    let (proposal_id, document_id): (String, String) = sqlx::query_as(
        "SELECT proposal_id, document_id FROM authored_head_proposals
         WHERE operation_id = 'superseded-local-edit'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    enrich_proposal_sequence(&fixture.pool, &proposal_id, 7).await;

    // Model a server head from another ordered proposal. It shares the initial
    // base but deliberately does not descend from this client's optimistic tip.
    let document_id = AuthoredDocumentId::parse(document_id).unwrap();
    let initial_id = RevisionId::parse(initial).unwrap();
    let server_graph = graph_with_node("server-winner", 2.0);
    let server_files = graph_files(&server_graph).unwrap();
    let server_metadata =
        revision_metadata("sync_integration", Some("server-winner"), "Server winner").unwrap();
    let mut transaction = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let server_revision = fixture
        .authored
        .store
        .insert_revision(
            &mut transaction,
            &document_id,
            std::slice::from_ref(&initial_id),
            &server_files,
            &server_metadata,
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    fixture
        .authored
        .apply_integrated_server_head(
            &fixture.pool,
            owner,
            document_id.as_str(),
            server_revision.id.as_str(),
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO authored_head_integrations
         (proposal_id, principal_key, document_id, prior_revision_id,
          result_revision_id, resolution_kind, server_integration_seq, integrated_at)
         VALUES (?, 'signed-in:' || ?, ?, ?, ?, 'quarantined_noop', 8,
                 '2026-08-02T00:00:01Z')",
    )
    .bind(&proposal_id)
    .bind(owner)
    .bind(document_id.as_str())
    .bind(server_revision.id.as_str())
    .bind(server_revision.id.as_str())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let history = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(20))
        .await
        .unwrap();
    let superseded = history
        .entries
        .iter()
        .find(|entry| entry.revision_id == local_tip.revision_id)
        .expect("losing proposal tip must remain visible");
    assert_eq!(superseded.position, AuthoredRevisionPosition::Superseded);
    assert_eq!(superseded.proposal_sequence, Some(7));

    let restored = fixture
        .authored
        .restore(
            &fixture.pool,
            Some(owner),
            &thread.id,
            &local_tip.revision_id,
            "restore-superseded-tip",
            crate::models::authored_state::AuthoredRestoreMode::StateOnly,
        )
        .await
        .unwrap();
    assert_ne!(restored.revision_id, local_tip.revision_id);
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = restored.document else {
        panic!("restored proposal tip should project a graph")
    };
    assert_eq!(graph.nodes[0].id, "superseded");
    let mut connection = fixture.pool.acquire().await.unwrap();
    let restored_info = fixture
        .authored
        .store
        .revision_info(
            &mut connection,
            &document_id,
            &RevisionId::parse(restored.revision_id).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        restored_info.metadata.restored_revision_id,
        Some(RevisionId::parse(local_tip.revision_id).unwrap())
    );
    assert_eq!(restored_info.parents, vec![server_revision.id]);
}

#[tokio::test]
async fn unknown_future_operation_superseded_tip_remains_listable_and_restorable() {
    let owner = "future-history-owner";
    let fixture = Fixture::signed_in(owner).await;
    let thread = fixture.pattern_thread().await;
    let initial_entry = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(1))
        .await
        .unwrap()
        .entries
        .into_iter()
        .next()
        .unwrap();
    let document_id: String = sqlx::query_scalar(
        "SELECT document_id FROM authored_documents
         WHERE principal_key = 'signed-in:' || ?
           AND subject_id = 'pattern' AND implementation_id = 'implementation'",
    )
    .bind(owner)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let document_id = AuthoredDocumentId::parse(document_id).unwrap();
    let initial_id = RevisionId::parse(initial_entry.revision_id).unwrap();
    let future_graph = graph_with_node("future-operation", 4.0);
    let future_files = graph_files(&future_graph).unwrap();
    let mut transaction = fixture.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let future_revision = fixture
        .authored
        .store
        .insert_revision(
            &mut transaction,
            &document_id,
            std::slice::from_ref(&initial_id),
            &future_files,
            &RevisionMetadata {
                operation_kind: "future_spatial_transform".into(),
                operation_id: Some("future-operation".into()),
                message: "A revision from a newer producer".into(),
                actor: Actor::user(),
                author_name: "Future Luma".into(),
                author_email: "future@luma.local".into(),
                authored_at: "2026-08-02T00:00:01Z".into(),
                thread_id: None,
                assistant_message_id: None,
                restored_revision_id: None,
            },
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let proposal_id = "future-operation-proposal";
    sqlx::query(
        "INSERT INTO authored_head_proposals
         (proposal_id, principal_key, document_id, device_id, operation_id,
          base_revision_id, proposed_revision_id, created_at, server_proposal_seq)
         VALUES (?, 'signed-in:' || ?, ?, 'future-device', 'future-proposal-op',
                 ?, ?, '2026-08-02T00:00:01Z', 9)",
    )
    .bind(proposal_id)
    .bind(owner)
    .bind(document_id.as_str())
    .bind(initial_id.as_str())
    .bind(future_revision.id.as_str())
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO authored_head_integrations
         (proposal_id, principal_key, document_id, prior_revision_id,
          result_revision_id, resolution_kind, server_integration_seq, integrated_at)
         VALUES (?, 'signed-in:' || ?, ?, ?, ?, 'quarantined_noop', 10,
                 '2026-08-02T00:00:02Z')",
    )
    .bind(proposal_id)
    .bind(owner)
    .bind(document_id.as_str())
    .bind(initial_id.as_str())
    .bind(initial_id.as_str())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let history = fixture
        .authored
        .list_history(&fixture.pool, Some(owner), &thread.id, None, Some(20))
        .await
        .unwrap();
    let entry = history
        .entries
        .iter()
        .find(|entry| entry.revision_id == future_revision.id.to_string())
        .expect("newer operation proposal tip must remain visible");
    assert_eq!(entry.position, AuthoredRevisionPosition::Superseded);
    assert_eq!(
        entry.kind,
        crate::models::authored_state::AuthoredOperationKind::Revision
    );

    let restored = fixture
        .authored
        .restore(
            &fixture.pool,
            Some(owner),
            &thread.id,
            future_revision.id.as_str(),
            "restore-future-operation",
            crate::models::authored_state::AuthoredRestoreMode::StateOnly,
        )
        .await
        .unwrap();
    assert_ne!(restored.revision_id, future_revision.id.to_string());
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = restored.document else {
        panic!("future operation should restore its graph")
    };
    assert_eq!(graph.nodes[0].id, "future-operation");
}

/// Every revision names its writer, and the three writers Luma has are told
/// apart: the operator in the editor, the model serving an agent turn, and an
/// out-of-process MCP client. The thread's own label wins over the host's, and
/// a model id spelled the way the TypeScript loop spells it lands on the same
/// actor as the Rust loop's key.
#[tokio::test]
async fn each_writer_names_itself_on_its_revisions() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;

    let user_edit = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            "implementation",
            "user-edit",
            graph_with_node("from-the-editor", 4.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Edit from the editor",
        )
        .await
        .unwrap();
    assert_eq!(actor_of(&fixture, &user_edit.revision_id).await, "user");

    agent_threads::set_thread_actor(&fixture.pool, &thread.id, "anthropic/claude-opus-5", None)
        .await
        .unwrap();
    let prepared = fixture
        .authored
        .prepare_turn(
            &fixture.pool,
            None,
            PrepareAuthoredTurnInput {
                thread_id: thread.id.clone(),
                assistant_message_id: "assistant-1".into(),
                graph: Some(graph_with_node("from-the-agent", 2.0)),
            },
        )
        .await
        .unwrap();
    fixture.append_assistant(&thread.id, "assistant-1").await;
    let AuthoredTurnCommit::Committed {
        revision_id: turn_revision,
        ..
    } = fixture
        .authored
        .finalize_turn(
            &fixture.pool,
            None,
            FinalizeAuthoredTurnInput {
                thread_id: thread.id.clone(),
                assistant_message_id: "assistant-1".into(),
                prepared_revision_id: prepared.prepared_revision_id,
            },
        )
        .await
        .unwrap()
    else {
        panic!("the turn should commit")
    };
    assert_eq!(
        actor_of(&fixture, &turn_revision).await,
        "claude-opus-5",
        "a provider-qualified model id is stored under its canonical key"
    );

    // An out-of-process client names itself once, for the whole connection.
    fixture
        .authored
        .set_session_actor(Actor::parse("client:claude-code/2.1:claude-opus-5").unwrap());
    let client_edit = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            "implementation",
            "client-edit",
            graph_with_node("from-the-client", 6.0),
            &crate::services::graph_documents::graph_revision(&graph_with_node(
                "from-the-agent",
                2.0,
            ))
            .unwrap(),
            "Edit from an MCP client",
        )
        .await
        .unwrap();
    assert_eq!(
        actor_of(&fixture, &client_edit.revision_id).await,
        "client:claude-code/2.1:claude-opus-5"
    );

    // The thread still answers for its own writes: the host's session actor is
    // only the fallback for an operation with no conversation behind it.
    let history = fixture
        .authored
        .list_history(&fixture.pool, None, &thread.id, None, Some(20))
        .await
        .unwrap();
    let turn = history
        .entries
        .iter()
        .find(|entry| entry.revision_id == turn_revision)
        .expect("the turn is in history");
    assert_eq!(turn.actor, "claude-opus-5");
}

/// A restore is authored by whoever restored, not by whoever wrote the state
/// being restored.
#[tokio::test]
async fn a_restore_is_authored_by_the_writer_who_restored_it() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let first = fixture
        .authored
        .apply_graph_for_scope(
            &fixture.pool,
            None,
            "pattern",
            "implementation",
            "before-restore",
            graph_with_node("before", 4.0),
            &crate::services::graph_documents::graph_revision(&empty_graph()).unwrap(),
            "Edit before the restore",
        )
        .await
        .unwrap();

    agent_threads::set_thread_actor(&fixture.pool, &thread.id, "kimi-k3-fast", None)
        .await
        .unwrap();
    let restored = fixture
        .authored
        .restore(
            &fixture.pool,
            None,
            &thread.id,
            &first.revision_id,
            "restore-1",
            AuthoredRestoreMode::StateOnly,
        )
        .await
        .unwrap();
    assert_eq!(
        actor_of(&fixture, &restored.revision_id).await,
        "kimi-k3-fast"
    );
}

async fn actor_of(fixture: &Fixture, revision_id: &str) -> String {
    sqlx::query_scalar("SELECT actor FROM authored_revisions WHERE revision_id = ?")
        .bind(revision_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn turn_checkpoint_is_immutable_and_finalization_advances_once() {
    let fixture = Fixture::new().await;
    let thread = fixture.pattern_thread().await;
    let prepared = fixture
        .authored
        .prepare_turn(
            &fixture.pool,
            None,
            PrepareAuthoredTurnInput {
                thread_id: thread.id.clone(),
                assistant_message_id: "assistant-1".into(),
                graph: Some(graph_with_node("turn", 2.0)),
            },
        )
        .await
        .unwrap();
    fixture.append_assistant(&thread.id, "assistant-1").await;
    let input = FinalizeAuthoredTurnInput {
        thread_id: thread.id,
        assistant_message_id: "assistant-1".into(),
        prepared_revision_id: prepared.prepared_revision_id,
    };
    let first = fixture
        .authored
        .finalize_turn(&fixture.pool, None, input.clone())
        .await
        .unwrap();
    let replay = fixture
        .authored
        .finalize_turn(&fixture.pool, None, input)
        .await
        .unwrap();
    let (
        AuthoredTurnCommit::Committed {
            revision_id: first_revision,
            ..
        },
        AuthoredTurnCommit::Committed {
            revision_id: replay_revision,
            ..
        },
    ) = (first, replay)
    else {
        panic!("turn should commit")
    };
    assert_eq!(first_revision, replay_revision);
    let outcomes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_turn_outcomes
         WHERE assistant_message_id = 'assistant-1'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(outcomes, 1);
}

#[tokio::test]
async fn subagent_turn_advances_only_its_workspace_head() {
    let fixture = Fixture::signed_in("owner").await;
    let parent = fixture.pattern_thread().await;
    let child = fixture.subagent_thread(&parent).await;
    let before = fixture
        .authored
        .current_revision(&fixture.pool, Some("owner"), &parent.id)
        .await
        .unwrap();
    let (workspace_id, workspace_base) = fixture.active_workspace(&child.id).await;
    assert_eq!(workspace_base, before.revision_id);

    let prepared = fixture
        .authored
        .prepare_turn(
            &fixture.pool,
            Some("owner"),
            PrepareAuthoredTurnInput {
                thread_id: child.id.clone(),
                assistant_message_id: "child-1".into(),
                graph: Some(graph_with_node("child", 3.0)),
            },
        )
        .await
        .unwrap();
    // The assistant row is insertable only because the turn above prepared
    // one: this is the 1811 trigger, satisfied by a child thread with no
    // milestone row anywhere.
    fixture.append_assistant(&child.id, "child-1").await;
    let committed = fixture
        .authored
        .finalize_turn(
            &fixture.pool,
            Some("owner"),
            FinalizeAuthoredTurnInput {
                thread_id: child.id.clone(),
                assistant_message_id: "child-1".into(),
                prepared_revision_id: prepared.prepared_revision_id,
            },
        )
        .await
        .unwrap();
    let AuthoredTurnCommit::Committed {
        revision_id,
        applied_to_current_projection,
        ..
    } = committed
    else {
        panic!("a subagent turn should commit to its workspace")
    };
    assert!(!applied_to_current_projection);

    let after = fixture
        .authored
        .current_revision(&fixture.pool, Some("owner"), &parent.id)
        .await
        .unwrap();
    assert_eq!(after.revision_id, before.revision_id);
    let AuthoredProjectedDocument::PatternGraph { graph, .. } = after.document else {
        panic!("pattern document")
    };
    assert!(graph.nodes.is_empty());

    let (_, workspace_head) = fixture.active_workspace(&child.id).await;
    assert_eq!(workspace_head, revision_id);
    let recorded: Option<String> = sqlx::query_scalar(
        "SELECT workspace_id FROM authored_turn_preparations
         WHERE assistant_message_id = 'child-1'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(recorded.as_deref(), Some(workspace_id.as_str()));
    // A private head is never proposed: the live head did not move.
    let proposals: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM authored_head_proposals WHERE proposed_revision_id = ?",
    )
    .bind(&revision_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(proposals, 0);
    // The workspace directory was brought up to the head the turn wrote.
    let check = fixture
        .authored
        .check_workspace(&fixture.pool, Some("owner"), &child.id, &workspace_id)
        .await
        .unwrap();
    assert!(!check.changed);
    assert_eq!(check.head_revision_id, revision_id);
}

#[tokio::test]
async fn recovery_finalizes_a_live_workspace_turn_and_skips_a_discarded_one() {
    let fixture = Fixture::signed_in("owner").await;
    let parent = fixture.pattern_thread().await;
    let child = fixture.subagent_thread(&parent).await;
    let before = fixture
        .authored
        .current_revision(&fixture.pool, Some("owner"), &parent.id)
        .await
        .unwrap();

    fixture
        .authored
        .prepare_turn(
            &fixture.pool,
            Some("owner"),
            PrepareAuthoredTurnInput {
                thread_id: child.id.clone(),
                assistant_message_id: "child-1".into(),
                graph: Some(graph_with_node("first", 1.0)),
            },
        )
        .await
        .unwrap();
    fixture.append_assistant(&child.id, "child-1").await;
    let recovered = fixture
        .authored
        .recover_turns(&fixture.pool, Some("owner"), &child.id)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    let (workspace_id, workspace_head) = fixture.active_workspace(&child.id).await;
    assert_ne!(workspace_head, before.revision_id);

    fixture
        .authored
        .prepare_turn(
            &fixture.pool,
            Some("owner"),
            PrepareAuthoredTurnInput {
                thread_id: child.id.clone(),
                assistant_message_id: "child-2".into(),
                graph: Some(graph_with_node("second", 2.0)),
            },
        )
        .await
        .unwrap();
    fixture.append_assistant(&child.id, "child-2").await;
    fixture
        .authored
        .remove_workspace(&fixture.pool, Some("owner"), &child.id, &workspace_id)
        .await
        .unwrap();
    // A discarded workspace has no head left to finalize into. The turn is
    // skipped, not redirected onto the live document.
    let recovered = fixture
        .authored
        .recover_turns(&fixture.pool, Some("owner"), &child.id)
        .await
        .unwrap();
    assert!(recovered.is_empty());
    let after = fixture
        .authored
        .current_revision(&fixture.pool, Some("owner"), &parent.id)
        .await
        .unwrap();
    assert_eq!(after.revision_id, before.revision_id);
}
