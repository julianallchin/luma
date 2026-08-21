//! Turn-protocol tests. No network, no window: a scripted model, a scripted
//! tool, and a temporary database.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tempfile::TempDir;

use super::model::{ModelEvent, StopReason, Usage};
use super::tools::{Tool, ToolContext, ToolRegistry};
use super::*;
use crate::agent::model::scripted::ScriptedModel;
use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::database::local::database::init_app_db_at;
use crate::database::local::state::init_state_db_at;
use crate::database::Db;
use crate::storage::StorageRoot;

/// A tool that never leaves the process, so the loop can be exercised without
/// a Python kernel.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> std::borrow::Cow<'static, str> {
        "Echo the arguments back.".into()
    }

    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": { "value": { "type": "string" } } })
    }

    async fn call(&self, _ctx: &ToolContext<'_>, args: Value) -> Result<Value, String> {
        Ok(json!({ "echoed": args }))
    }
}

struct Fixture {
    _dir: TempDir,
    services: Arc<crate::dispatch::AppServices>,
    thread_id: String,
}

impl Fixture {
    fn pool(&self) -> &SqlitePool {
        &self.services.db().0
    }
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = StorageRoot::from_path(dir.path().to_path_buf());
    let db: Db = init_app_db_at(storage.path()).await.expect("app db");
    let state_db = init_state_db_at(storage.path()).await.expect("state db");
    crate::database::local::auth::arm_write_admission(&db.0, None)
        .await
        .expect("admission");
    let workspaces = Arc::new(PythonWorkspaceService::new(
        storage.agent_workspaces_dir(),
        Arc::new(|| Err("no python worker in tests".to_string())),
    ));
    let services = Arc::new(crate::dispatch::AppServices::headless(
        db,
        state_db,
        storage,
        dir.path().join("fixtures"),
        workspaces,
    ));

    // The thread's authored document is projected from real subject rows.
    sqlx::query(
        "INSERT INTO venues (id, uid, name) VALUES ('venue-1', NULL, 'Venue');
         INSERT INTO tracks (id, uid, track_hash, title, file_path)
         VALUES ('track-1', NULL, 'hash-1', 'Track', '/tmp/track.wav');
         INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score-1', NULL, 'track-1', 'venue-1', 'Score');",
    )
    .execute(&services.db().0)
    .await
    .expect("subject rows");

    let agent = AgentService::new(Arc::clone(&services));
    let scope = ThreadScope::track("track-1", "venue-1", "score-1");
    let thread = agent.resolve_thread(&scope).await.expect("thread");

    Fixture {
        _dir: dir,
        services,
        thread_id: thread.thread.id,
    }
}

fn agent(fixture: &Fixture, steps: Vec<Vec<ModelEvent>>) -> AgentService {
    AgentService::new(Arc::clone(&fixture.services))
        .with_model(Arc::new(ScriptedModel::new(steps)))
        .with_tools(ToolRegistry::new(vec![Arc::new(EchoTool)]))
}

/// One tool call, then a reply.
fn tool_then_reply() -> Vec<Vec<ModelEvent>> {
    vec![
        vec![
            ModelEvent::ToolCallStarted {
                id: "call_1".into(),
                name: "echo".into(),
            },
            ModelEvent::ToolCallArgsDelta {
                id: "call_1".into(),
                json: r#"{"value":"hi"}"#.into(),
            },
            ModelEvent::ToolCallEnded {
                id: "call_1".into(),
            },
            ModelEvent::StepEnded {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
        ],
        vec![
            ModelEvent::TextDelta("all ".into()),
            ModelEvent::TextDelta("done".into()),
            ModelEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ],
    ]
}

async fn drain(stream: &mut TurnStream) -> Vec<TurnEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

async fn preparations(pool: &SqlitePool, thread_id: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT assistant_message_id FROM authored_turn_preparations
         WHERE thread_id = ? ORDER BY rowid",
    )
    .bind(thread_id)
    .fetch_all(pool)
    .await
    .expect("preparations")
}

#[tokio::test]
async fn a_turn_with_one_tool_call_persists_a_prepared_assistant_row() {
    let fixture = fixture().await;
    let service = agent(&fixture, tool_then_reply());
    let mut stream = service.turn(&fixture.thread_id, "make it dark".to_string().into());
    let events = drain(&mut stream).await;

    assert_eq!(
        events.last(),
        Some(&TurnEvent::TurnEnded {
            outcome: TurnOutcome::Completed
        }),
        "turn did not complete: {events:#?}"
    );

    // The durable transcript, read back from the database, is the golden.
    let rows = crate::database::local::agent_threads::list_messages(
        fixture.pool(),
        &fixture.thread_id,
        None,
    )
    .await
    .expect("messages");
    let transcript = Transcript::from_rows(&rows).expect("transcript");
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[0].role, Role::User);

    let assistant = &transcript.messages[1];
    assert_eq!(assistant.role, Role::Assistant);
    let AgentChatPart::Tool(tool) = &assistant.parts[1] else {
        panic!("expected a tool part, got {:#?}", assistant.parts);
    };
    assert_eq!(tool.tool_name(), "echo");
    assert_eq!(tool.state, ToolState::OutputAvailable);
    assert_eq!(tool.output, Some(json!({ "echoed": { "value": "hi" } })));
    assert!(assistant.parts.iter().any(|part| *part
        == AgentChatPart::Text {
            text: "all done".into()
        }));

    // Exactly one preparation per assistant row — the trigger's own invariant,
    // and the insert above proves the trigger let the row through.
    assert_eq!(
        preparations(fixture.pool(), &fixture.thread_id).await,
        vec![assistant.id.clone()]
    );
}

#[tokio::test]
async fn steering_mid_turn_prepares_every_assistant_row() {
    let fixture = fixture().await;
    let mut steps = tool_then_reply();
    steps.push(vec![
        ModelEvent::TextDelta("and darker".into()),
        ModelEvent::StepEnded {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        },
    ]);
    let service = agent(&fixture, steps);

    let mut stream = service.turn(&fixture.thread_id, "make it dark".to_string().into());
    // Steer before the first row closes; it is applied at the row boundary.
    stream.steer("darker");
    let events = drain(&mut stream).await;
    assert_eq!(
        events.last(),
        Some(&TurnEvent::TurnEnded {
            outcome: TurnOutcome::Completed
        }),
        "steered turn did not complete: {events:#?}"
    );

    let rows = crate::database::local::agent_threads::list_messages(
        fixture.pool(),
        &fixture.thread_id,
        None,
    )
    .await
    .expect("messages");
    let transcript = Transcript::from_rows(&rows).expect("transcript");
    let assistants: Vec<_> = transcript
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| message.id.clone())
        .collect();
    assert_eq!(assistants.len(), 2, "steering must open a second row");
    // The regression this rewrite exists for: the TypeScript loop prepared
    // once per prompt, leaving the second row unprepared.
    assert_eq!(
        preparations(fixture.pool(), &fixture.thread_id).await,
        assistants
    );
}

#[tokio::test]
async fn rehydration_replays_the_tool_result_to_the_model() {
    let fixture = fixture().await;
    let scripted = Arc::new(ScriptedModel::new(tool_then_reply()));
    let service = AgentService::new(Arc::clone(&fixture.services))
        .with_model(Arc::clone(&scripted) as Arc<dyn super::model::ModelClient>)
        .with_tools(ToolRegistry::new(vec![Arc::new(EchoTool)]));
    let mut stream = service.turn(&fixture.thread_id, "go".to_string().into());
    drain(&mut stream).await;

    let requests = scripted.requests();
    assert_eq!(requests.len(), 2, "one request per step");
    // The second step must carry the first step's call *and* its result.
    let second = &requests[1];
    assert!(second.messages.iter().any(|message| message
        .content
        .iter()
        .any(|block| matches!(block, super::model::ContentBlock::ToolUse { name, .. } if name == "echo"))));
    assert!(second.messages.iter().any(|message| message
        .content
        .iter()
        .any(|block| matches!(block, super::model::ContentBlock::ToolResult { .. }))));
}

#[tokio::test]
async fn dropping_the_stream_stops_the_turn() {
    let fixture = fixture().await;
    let service = agent(&fixture, tool_then_reply());
    let mut stream = service.turn(&fixture.thread_id, "go".to_string().into());
    // Take a couple of events, then drop: nothing further may be written.
    let _ = stream.next().await;
    drop(stream);

    let rows = crate::database::local::agent_threads::list_messages(
        fixture.pool(),
        &fixture.thread_id,
        None,
    )
    .await
    .expect("messages");
    assert!(
        rows.iter().all(|row| row.role == "user"),
        "a cancelled turn must not persist an assistant row"
    );
}
