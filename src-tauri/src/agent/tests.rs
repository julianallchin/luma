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

/// The whole loop over the live wire: a real provider, a real tool, and the
/// rehydration in between. What the scripted tests cannot reach — a scripted
/// model accepts any `messages` array, and every ordering rule the provider
/// enforces on a tool round trip is invisible to it.
///
/// Ignored by default — it costs tokens and needs a network. Run with
/// `cargo test --lib a_live_turn -- --ignored --nocapture`, with
/// `LUMA_AI_GATEWAY_API_KEY` set.
#[tokio::test]
#[ignore = "live: needs LUMA_AI_GATEWAY_API_KEY and a network"]
async fn a_live_turn_runs_a_tool_and_answers_from_its_result() {
    let key = std::env::var(super::model::Provider::VercelAiGateway.key_env_var())
        .expect("no gateway credential: set LUMA_AI_GATEWAY_API_KEY to smoke-test");
    let fixture = fixture().await;
    let service = AgentService::new(Arc::clone(&fixture.services))
        .with_model(Arc::new(super::model::anthropic::AnthropicClient::gateway(
            key,
        )))
        .with_tools(ToolRegistry::new(vec![Arc::new(EchoTool)]));

    let mut stream = service.turn(
        &fixture.thread_id,
        "Call the echo tool with value \"ping\", then tell me what it echoed."
            .to_string()
            .into(),
    );
    let events = drain(&mut stream).await;
    assert_eq!(
        events.last(),
        Some(&TurnEvent::TurnEnded {
            outcome: TurnOutcome::Completed
        }),
        "the live turn did not complete: {events:#?}"
    );

    let called = events.iter().any(
        |event| matches!(event, TurnEvent::ToolCallEnded { output, .. } if matches!(output, ToolResult::Output { .. })),
    );
    assert!(called, "the live turn ran no tool: {events:#?}");

    let rows = crate::database::local::agent_threads::list_messages(
        fixture.pool(),
        &fixture.thread_id,
        None,
    )
    .await
    .expect("messages");
    let transcript = Transcript::from_rows(&rows).expect("transcript");
    let reply: String = transcript
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            AgentChatPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    println!("--- transcript ---\n{reply}");
    assert!(
        reply.contains("ping"),
        "the model never read the tool result back: {reply}"
    );
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

/// The `python` tool is attributed to the durable *user* row, never to the
/// assistant row being written — that one is not inserted until the turn
/// closes, so attributing to it made every cell fail the host's durability
/// check before it could reach a kernel. The stub worker is the proof: the call
/// must get far enough to ask for one.
#[tokio::test]
async fn a_python_call_is_attributed_to_the_durable_user_turn() {
    let fixture = fixture().await;
    // No `with_tools`: the real registry, so the real python tool runs.
    let service = AgentService::new(Arc::clone(&fixture.services)).with_model(Arc::new(
        ScriptedModel::new(vec![
            vec![
                ModelEvent::ToolCallStarted {
                    id: "call_1".into(),
                    name: "python".into(),
                },
                ModelEvent::ToolCallArgsDelta {
                    id: "call_1".into(),
                    json: r#"{"purpose":"section energy","code":"1 + 1"}"#.into(),
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
                ModelEvent::TextDelta("no kernel".into()),
                ModelEvent::StepEnded {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                },
            ],
        ]),
    ));
    let mut stream = service.turn(&fixture.thread_id, "analyse it".to_string().into());
    let events = drain(&mut stream).await;

    // A rejected turn message fails the *call* (`ToolResult::Failed`); a cell
    // that was admitted and then found no kernel comes back as a normal result
    // whose status is `failed`. Which of the two arrives is the whole point.
    let outcome = events
        .iter()
        .find_map(|event| match event {
            TurnEvent::ToolCallEnded { output, .. } => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the python call never ended: {events:#?}"));
    let value = match outcome {
        ToolResult::Output { value } => value,
        ToolResult::Failed { message } => {
            panic!("the python call was rejected before it reached a kernel: {message}")
        }
    };
    assert_eq!(value["status"], json!("failed"));
    assert!(
        value["notices"]
            .as_array()
            .is_some_and(|notices| notices.iter().any(|notice| notice
                .as_str()
                .is_some_and(|notice| notice.contains("no python worker in tests")))),
        "the cell stopped somewhere other than the worker: {value:#?}"
    );
}

fn history_thread(id: &str, title: Option<&str>) -> crate::models::agent_threads::AgentThread {
    crate::models::agent_threads::AgentThread {
        id: id.into(),
        owner_user_id: None,
        agent_kind: "track_copilot".into(),
        subject_kind: Some("track".into()),
        subject_id: Some("track-1".into()),
        implementation_id: None,
        venue_id: Some("venue-1".into()),
        score_id: None,
        forked_from_thread_id: None,
        forked_at_message_id: None,
        title: title.map(ToString::to_string),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn history_message(thread: &str, seq: i64, role: &str, text: &str) -> AgentThreadMessage {
    AgentThreadMessage {
        id: format!("{thread}-{seq}"),
        thread_id: thread.into(),
        parent_message_id: None,
        seq,
        role: role.into(),
        parts: serde_json::json!([{"type": "text", "text": text}]),
        created_at: String::new(),
    }
}

/// A history row is named by its own words: the first thing asked, then the
/// last thing answered, each flattened to one line.
#[test]
fn a_history_row_reads_its_opening_and_its_latest_reply() {
    use super::History;

    let history = History::build(
        vec![
            history_thread("a", None),
            history_thread("b", Some("Named")),
            history_thread("c", None),
        ],
        vec![
            history_message("a", 0, "user", "  where does\nthe ramp peak?  "),
            history_message("a", 1, "assistant", "Bar 3."),
            history_message("a", 2, "user", "and the release?"),
            history_message("a", 3, "assistant", "Two bars\nafter."),
            // A titled thread nobody spoke in is named by its title.
            // An untitled, unspoken one gets the placeholder.
        ],
    );
    let [a, b, c] = history.entries() else {
        panic!("three rows: {:?}", history.entries());
    };
    assert_eq!(a.headline(), "where does the ramp peak?");
    assert_eq!(a.latest.as_deref(), Some("Two bars after."));
    assert_eq!(b.headline(), "Named");
    assert_eq!(b.latest, None);
    assert_eq!(c.headline(), "New chat");
}

/// The grep: case-insensitive, one hit per line, capped per conversation, and
/// the span lands on the original text.
#[test]
fn a_history_search_finds_lines_and_windows_long_ones() {
    use super::History;

    let long = format!("{}Ramp here{}", "x".repeat(150), "y".repeat(200));
    let mut messages = vec![
        history_message(
            "a",
            0,
            "user",
            "Where does the RAMP peak?\nno ramp on this line either",
        ),
        history_message("a", 1, "assistant", &long),
        history_message("b", 0, "user", "nothing relevant"),
    ];
    for seq in 0..10 {
        messages.push(history_message("b", seq + 1, "assistant", "ramp ramp ramp"));
    }
    let history = History::build(
        vec![history_thread("a", None), history_thread("b", None)],
        messages,
    );

    let hits = history.search("  ramp ");
    let a_hits: Vec<_> = hits.iter().filter(|hit| hit.entry == 0).collect();
    assert_eq!(a_hits.len(), 3, "{hits:?}");
    assert_eq!(&a_hits[0].excerpt[a_hits[0].span.clone()], "RAMP");
    assert_eq!(a_hits[0].excerpt, "Where does the RAMP peak?");
    // The long line is windowed around its match, marked on both cut sides.
    let windowed = &a_hits[2];
    assert!(windowed.excerpt.starts_with('…') && windowed.excerpt.ends_with('…'));
    assert_eq!(&windowed.excerpt[windowed.span.clone()], "Ramp");
    // One hit per line, and no more than the cap per conversation.
    assert_eq!(
        hits.iter().filter(|hit| hit.entry == 1).count(),
        History::HITS_PER_ENTRY
    );
    assert!(history.search("   ").is_empty());
}
