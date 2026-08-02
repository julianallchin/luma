use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tempfile::TempDir;

use super::super::*;
use crate::models::agent_threads::{AppendAgentThreadMessagesInput, NewAgentThreadMessage};
use crate::models::node_graph::ParamType;
pub(super) use crate::models::node_graph::{Edge, NodeInstance, PatternArgDef, PatternArgType};
pub(super) use crate::models::scores::{CreateTrackScoreInput, UpdateTrackScoreInput};
pub(super) use crate::services::graph_documents::load_graph_document_unscoped;
pub(super) use serde_json::json;
pub(super) use std::path::Path;

pub(super) struct Fixture {
    pub(super) _directory: TempDir,
    pub(super) pool: SqlitePool,
    pub(super) authored: AuthoredDocuments,
}

pub(super) fn created_clip_id(result: &AppliedAuthoredTrackEdit) -> &str {
    result
        .edit
        .created_clip_id
        .as_deref()
        .expect("create result must name its stable clip ID")
}

impl Fixture {
    pub(super) async fn new() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
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
            .expect("migration pool");
        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .expect("migrations");
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
            .expect("test pool");
        let authored =
            AuthoredDocuments::new(StorageRoot::from_path(directory.path().join("storage")));
        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .expect("arm guest write admission");
        Self {
            _directory: directory,
            pool,
            authored,
        }
    }

    pub(super) async fn add_pattern(&self, pattern_id: &str) {
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES (?, NULL, ?)")
            .bind(pattern_id)
            .bind(pattern_id)
            .execute(&self.pool)
            .await
            .expect("pattern");
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES (?, NULL, ?, ?)",
        )
        .bind(format!("implementation-{pattern_id}"))
        .bind(pattern_id)
        .bind(serde_json::to_string(&empty_graph()).unwrap())
        .execute(&self.pool)
        .await
        .expect("implementation");
    }

    pub(super) async fn pattern_thread(&self, pattern_id: &str) -> AgentThread {
        self.pattern_thread_for(pattern_id, &implementation_id(pattern_id))
            .await
    }

    pub(super) async fn pattern_thread_for(
        &self,
        pattern_id: &str,
        implementation_id: &str,
    ) -> AgentThread {
        self.authored
            .create_thread_with_authored_state(
                &self.pool,
                CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: "pattern_graph".into(),
                    subject_kind: Some("pattern".into()),
                    subject_id: Some(pattern_id.into()),
                    implementation_id: Some(implementation_id.into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .expect("pattern thread")
    }

    pub(super) async fn append_assistant(&self, thread_id: &str, message_id: &str) {
        agent_threads::append_messages(
            &self.pool,
            thread_id,
            AppendAgentThreadMessagesInput {
                operation_id: uuid::Uuid::new_v4().to_string(),
                messages: vec![NewAgentThreadMessage {
                    id: Some(message_id.into()),
                    role: "assistant".into(),
                    parts: json!([]),
                }],
            },
            None,
        )
        .await
        .expect("assistant transcript");
    }

    pub(super) async fn add_track_scope(&self) -> TrackScope {
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', NULL, 'venue')")
            .execute(&self.pool)
            .await
            .expect("venue");
        sqlx::query(
            "INSERT INTO tracks
             (id, uid, track_hash, title, duration_seconds, file_path)
             VALUES ('track', NULL, 'hash', 'track', 120.0, '/tmp/track.wav')",
        )
        .execute(&self.pool)
        .await
        .expect("track");
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('score', NULL, 'track', 'venue', 'score')",
        )
        .execute(&self.pool)
        .await
        .expect("score");
        TrackScope {
            score_id: "score".into(),
            track_id: "track".into(),
            venue_id: "venue".into(),
        }
    }

    pub(super) async fn track_thread(&self, scope: &TrackScope) -> AgentThread {
        self.authored
            .create_thread_with_authored_state(
                &self.pool,
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
            .expect("track thread")
    }
}

pub(super) fn implementation_id(pattern_id: &str) -> String {
    format!("implementation-{pattern_id}")
}

pub(super) fn empty_graph() -> Graph {
    Graph {
        nodes: Vec::new(),
        edges: Vec::new(),
        args: Vec::new(),
    }
}

pub(super) fn scalar_arg(id: &str, name: &str, value: f64) -> PatternArgDef {
    PatternArgDef {
        id: id.into(),
        name: name.into(),
        arg_type: PatternArgType::Scalar,
        default_value: json!(value),
    }
}

pub(super) fn selection_arg(id: &str) -> PatternArgDef {
    PatternArgDef {
        id: id.into(),
        name: id.into(),
        arg_type: PatternArgType::Selection,
        default_value: json!({
            "expression": "all",
            "spatialReference": "global"
        }),
    }
}

pub(super) fn graph_with_args(args: Vec<PatternArgDef>) -> Graph {
    Graph {
        nodes: vec![crate::models::node_graph::NodeInstance {
            id: "pattern_args".into(),
            type_id: "pattern_args".into(),
            params: HashMap::new(),
            position_x: Some(0.0),
            position_y: Some(0.0),
        }],
        args,
        edges: Vec::new(),
    }
}

pub(super) fn graph_with_stale_arg_edge(args: Vec<PatternArgDef>) -> Graph {
    Graph {
        nodes: vec![
            NodeInstance {
                id: "pattern_args".into(),
                type_id: "pattern_args".into(),
                params: HashMap::new(),
                position_x: Some(0.0),
                position_y: Some(0.0),
            },
            NodeInstance {
                id: "view".into(),
                type_id: "view_signal".into(),
                params: HashMap::new(),
                position_x: Some(1.0),
                position_y: Some(0.0),
            },
        ],
        edges: vec![Edge {
            id: "stale".into(),
            from_node: "pattern_args".into(),
            from_port: "removed_arg".into(),
            to_node: "view".into(),
            to_port: "in".into(),
        }],
        args,
    }
}

fn math_node(id: &str, position_x: f64) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        type_id: "math".into(),
        params: HashMap::from([("operation".into(), json!("add"))]),
        position_x: Some(position_x),
        position_y: Some(0.0),
    }
}

pub(super) fn catalog_node(id: &str, type_id: &str, position_x: f64) -> NodeInstance {
    let definition = crate::node_graph::nodes::get_node_types()
        .into_iter()
        .find(|definition| definition.id == type_id)
        .expect("test node type");
    let params = definition
        .params
        .into_iter()
        .map(|param| {
            let value = match param.param_type {
                ParamType::Number => json!(param.default_number.unwrap_or(0.0)),
                ParamType::Text => json!(param.default_text.unwrap_or_default()),
            };
            (param.id, value)
        })
        .collect();
    NodeInstance {
        id: id.into(),
        type_id: type_id.into(),
        params,
        position_x: Some(position_x),
        position_y: Some(0.0),
    }
}

pub(super) fn signal_edge(from: &str, to: &str) -> Edge {
    Edge {
        id: format!("{from}:out->{to}:a"),
        from_node: from.into(),
        from_port: "out".into(),
        to_node: to.into(),
        to_port: "a".into(),
    }
}

pub(super) fn two_math_nodes(edges: Vec<Edge>) -> Graph {
    Graph {
        nodes: vec![math_node("a", 0.0), math_node("b", 1.0)],
        edges,
        args: Vec::new(),
    }
}

pub(super) async fn install_test_beat_grid(
    fixture: &Fixture,
    downbeats: &[f32],
    beats_per_bar: i64,
) {
    let beats: Vec<f32> = downbeats
        .windows(2)
        .flat_map(|window| {
            let step = (window[1] - window[0]) / beats_per_bar as f32;
            (0..beats_per_bar).map(move |beat| window[0] + beat as f32 * step)
        })
        .collect();
    crate::database::local::tracks::upsert_track_beats(
        &fixture.pool,
        "track",
        &serde_json::to_string(&beats).unwrap(),
        &serde_json::to_string(downbeats).unwrap(),
        Some(120.0),
        Some(f64::from(downbeats[0])),
        Some(beats_per_bar),
        1,
    )
    .await
    .unwrap();
}

pub(super) async fn invalidate_pattern_context(fixture: &Fixture, pattern_id: &str) {
    sqlx::query("UPDATE patterns SET name = 'renamed_after_commit' WHERE id = ?")
        .bind(pattern_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE implementations SET graph_json = 'not-json' WHERE pattern_id = ?")
        .bind(pattern_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    install_test_beat_grid(fixture, &[0.0, 0.75, 1.5, 2.25], 3).await;
}

pub(super) async fn prepare(
    fixture: &Fixture,
    thread: &AgentThread,
    message_id: &str,
    graph: Graph,
) -> PreparedAuthoredTurn {
    fixture
        .authored
        .prepare_turn(
            &fixture.pool,
            None,
            PrepareAuthoredTurnInput {
                thread_id: thread.id.clone(),
                assistant_message_id: message_id.into(),
                graph: Some(graph),
            },
        )
        .await
        .expect("prepare turn")
}
