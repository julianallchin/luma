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
    services: crate::dispatch::SharedServices,
    thread_id: String,
}

impl Fixture {
    fn pool(&self) -> &SqlitePool {
        &self.services.db().0
    }
}

/// The principal these fixtures write as. Every synced row has an owner.
const OWNER: &str = "11111111-2222-3333-4444-555555555555";

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = StorageRoot::from_path(dir.path().to_path_buf());
    let db: Db = init_app_db_at(storage.path()).await.expect("app db");
    let state_db = init_state_db_at(storage.path()).await.expect("state db");
    crate::database::local::auth::install_test_session(&state_db.0, OWNER).await;
    crate::database::local::auth::bootstrap_headless_admission(&db.0, &state_db.0)
        .await
        .expect("admission");
    let workspaces = Arc::new(PythonWorkspaceService::new(
        storage.agent_workspaces_dir(),
        Arc::new(|| Err("no python worker in tests".to_string())),
    ));
    let services = crate::dispatch::AppServices::headless(
        db,
        state_db,
        storage,
        dir.path().join("fixtures"),
        workspaces,
    )
    .into_shared();

    // The thread's authored document is projected from real subject rows.
    sqlx::query(
        "INSERT INTO venues (id, uid, name) VALUES ('venue-1', ?1, 'Venue');
         INSERT INTO tracks (id, uid, track_hash, title, file_path)
         VALUES ('track-1', ?1, 'hash-1', 'Track', '/tmp/track.wav');
         INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score-1', ?1, 'track-1', 'venue-1', 'Score');",
    )
    .bind(OWNER)
    .execute(&services.db().0)
    .await
    .expect("subject rows");

    let agent = AgentService::new(services.clone());
    let scope = ThreadScope::track("track-1", "venue-1", "score-1");
    let thread = agent.resolve_thread(&scope).await.expect("thread");

    Fixture {
        _dir: dir,
        services,
        thread_id: thread.thread.id,
    }
}

/// The turn registry's back-reference is installed by `into_shared`, which is
/// the only constructor of `SharedServices` — so the host that forgets it no
/// longer compiles, and this asserts the wiring the type now guarantees.
#[tokio::test]
async fn into_shared_attaches_the_turn_registry() {
    let fixture = fixture().await;
    assert!(fixture.services.agent_turns().is_attached());
}

fn agent(fixture: &Fixture, steps: Vec<Vec<ModelEvent>>) -> AgentService {
    AgentService::new(fixture.services.clone())
        .with_model(Arc::new(ScriptedModel::new(steps)))
        .with_tools(ToolRegistry::new(vec![Arc::new(EchoTool)]))
}

/// What one tool call saw of where it was running.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Probe {
    thread_id: String,
    /// The draft the loop bound this call to. `Some` exactly when the call is
    /// running inside a subagent thread.
    draft_id: Option<String>,
    /// The *live* score's clips at the moment of the call — what a subagent
    /// must not be able to move.
    live_clips: usize,
}

/// Records where it ran. The instrument for "a child writes its draft and the
/// live score does not move until the merge".
struct ProbeTool(Arc<std::sync::Mutex<Vec<Probe>>>);

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn description(&self) -> std::borrow::Cow<'static, str> {
        "Record where this call is running.".into()
    }

    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(&self, ctx: &ToolContext<'_>, _args: Value) -> Result<Value, String> {
        let services = ctx.services();
        let mut connection = services
            .db()
            .0
            .acquire()
            .await
            .map_err(|error| error.to_string())?;
        let live =
            crate::database::local::scores::rows::load_score(&mut connection, "score-1").await?;
        let probe = Probe {
            thread_id: ctx.thread_id.to_string(),
            draft_id: ctx.draft_id.map(ToString::to_string),
            live_clips: live.clips.len(),
        };
        self.0.lock().expect("poisoned").push(probe.clone());
        Ok(json!({ "threadId": probe.thread_id }))
    }
}

/// A registry that can delegate, plus the probe. The subagent tool is the real
/// one; only the model and the leaf tools are scripted.
fn delegating_agent(
    fixture: &Fixture,
    steps: Vec<Vec<ModelEvent>>,
    probes: &Arc<std::sync::Mutex<Vec<Probe>>>,
) -> AgentService {
    AgentService::new(fixture.services.clone())
        .with_model(Arc::new(ScriptedModel::new(steps)))
        .with_tools(ToolRegistry::new(vec![
            Arc::new(ProbeTool(Arc::clone(probes))),
            Arc::new(super::tools::subagent::SubagentTool),
        ]))
}

/// One step that calls `name` with `arguments`.
fn call_step(id: &str, name: &str, arguments: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallStarted {
            id: id.into(),
            name: name.into(),
        },
        ModelEvent::ToolCallArgsDelta {
            id: id.into(),
            json: arguments.into(),
        },
        ModelEvent::ToolCallEnded { id: id.into() },
        ModelEvent::StepEnded {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        },
    ]
}

/// One step that answers and ends the turn.
fn reply_step(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TextDelta(text.into()),
        ModelEvent::StepEnded {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        },
    ]
}

fn delegate_step(id: &str, description: &str, task: &str) -> Vec<ModelEvent> {
    call_step(
        id,
        "subagent",
        &serde_json::to_string(&json!({ "description": description, "task": task }))
            .expect("arguments"),
    )
}

async fn children_of(pool: &SqlitePool, thread_id: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT id FROM agent_threads WHERE parent_thread_id = ? ORDER BY created_at, id",
    )
    .bind(thread_id)
    .fetch_all(pool)
    .await
    .expect("children")
}

/// How many open drafts a thread has. A subagent's work lives in one; a root
/// thread edits the live rows and owns none.
async fn open_drafts(pool: &SqlitePool, thread_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM drafts WHERE thread_id = ?")
        .bind(thread_id)
        .fetch_one(pool)
        .await
        .expect("drafts")
}

fn subagent_snapshots(events: &[TurnEvent]) -> Vec<super::subagent::SubagentSnapshot> {
    events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::Subagent { snapshot } => {
                Some(serde_json::from_value(snapshot.clone()).expect("snapshot"))
            }
            _ => None,
        })
        .collect()
}

/// The live score as it stands, which is what a merge is supposed to move.
async fn live_clips(fixture: &Fixture) -> usize {
    let mut connection = fixture.pool().acquire().await.expect("connection");
    crate::database::local::scores::rows::load_score(&mut connection, "score-1")
        .await
        .expect("live score")
        .clips
        .len()
}

/// The whole delegation, end to end: a child thread that is a real row, a
/// child turn that writes its own private head, a live document that does not
/// move until the merge, and a result the parent model can read.
#[tokio::test]
async fn a_subagent_runs_on_its_own_thread_and_merges_into_the_parent() {
    let fixture = fixture().await;
    let probes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = delegating_agent(
        &fixture,
        vec![
            delegate_step(
                "call_1",
                "Fitting the ramp",
                "Fit the ramp and report back.",
            ),
            call_step("call_2", "probe", "{}"),
            reply_step("fitted the ramp"),
            reply_step("delegated"),
        ],
        &probes,
    );
    let before = live_clips(&fixture).await;

    let mut stream = service.turn(&fixture.thread_id, "delegate it".to_string().into());
    let events = drain(&mut stream).await;
    assert_eq!(
        events.last(),
        Some(&TurnEvent::TurnEnded {
            outcome: TurnOutcome::Completed
        }),
        "the delegating turn did not complete: {events:#?}"
    );

    // The child is an ordinary thread row that names the call that spawned it.
    let [child_id] = children_of(fixture.pool(), &fixture.thread_id)
        .await
        .try_into()
        .expect("exactly one child thread");
    let child =
        crate::database::local::agent_threads::get_thread(fixture.pool(), &child_id, Some(OWNER))
            .await
            .expect("child thread");
    assert_eq!(child.thread.parent_call_id.as_deref(), Some("call_1"));
    assert_eq!(child.thread.agent_kind, "track_copilot");
    assert_eq!(child.thread.score_id.as_deref(), Some("score-1"));
    // Its own transcript, in its own rows — no second transcript store.
    let child_transcript = Transcript::from_rows(&child.messages).expect("child transcript");
    assert_eq!(child_transcript.messages[0].role, Role::User);
    assert!(child_transcript
        .messages
        .iter()
        .any(|message| message.text().contains("fitted the ramp")));
    // The child ran on a model, and the thread says which.
    assert!(child.thread.actor.is_some(), "the child turn set no actor");

    // The child's tools were bound to its private head, and the live document
    // did not move while it worked.
    let probes = probes.lock().expect("poisoned").clone();
    let [probe] = probes.as_slice() else {
        panic!("expected exactly one probe: {probes:#?}");
    };
    assert_eq!(probe.thread_id, child_id);
    assert!(
        probe.draft_id.is_some(),
        "a subagent's tools must address its draft"
    );
    assert_eq!(
        probe.live_clips, before,
        "the live score moved before the merge"
    );

    // The merge happened, and the draft is gone.
    assert_eq!(live_clips(&fixture).await, before);
    assert_eq!(open_drafts(fixture.pool(), &child_id).await, 0);

    // What the parent model got back.
    let rows = crate::database::local::agent_threads::list_messages(
        fixture.pool(),
        &fixture.thread_id,
        Some(OWNER),
    )
    .await
    .expect("messages");
    let transcript = Transcript::from_rows(&rows).expect("transcript");
    let tool = transcript
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .find_map(|part| match part {
            AgentChatPart::Tool(tool) if tool.tool_name() == "subagent" => Some(tool),
            _ => None,
        })
        .expect("a subagent chip");
    let output = tool.output.clone().expect("subagent output");
    assert_eq!(output["childThreadId"], json!(child_id));
    assert_eq!(output["outcome"]["status"], json!("merged"));
    assert_eq!(output["text"], json!("fitted the ramp"));

    // Live state reached the host and stayed out of the transcript.
    let snapshots = subagent_snapshots(&events);
    assert!(snapshots
        .iter()
        .all(|snapshot| snapshot.child_thread_id == child_id && snapshot.call_id == "call_1"));
    assert_eq!(
        snapshots.first().map(|snapshot| snapshot.phase),
        Some(super::subagent::SubagentPhase::Running)
    );
    assert_eq!(
        snapshots.last().map(|snapshot| snapshot.phase),
        Some(super::subagent::SubagentPhase::Completed)
    );
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains("subagentSnapshot"),
        "live snapshots must not be persisted"
    );
}

/// Nesting works one level and stops there, and the nested child publishes into
/// its parent's workspace rather than the live document.
#[tokio::test]
async fn a_nested_subagent_merges_into_its_parent_and_a_grandchild_is_refused() {
    let fixture = fixture().await;
    let probes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = delegating_agent(
        &fixture,
        vec![
            delegate_step("call_1", "Fitting the ramp", "Delegate again."),
            delegate_step("call_2", "Measuring the ramp", "Delegate once more."),
            // The grandchild is at the depth limit: this call is refused.
            delegate_step("call_3", "Going deeper", "Delegate a fourth time."),
            call_step("call_4", "probe", "{}"),
            reply_step("measured it"),
            reply_step("fitted it"),
            reply_step("delegated"),
        ],
        &probes,
    );
    let before = live_clips(&fixture).await;

    let mut stream = service.turn(&fixture.thread_id, "delegate it".to_string().into());
    let events = drain(&mut stream).await;
    assert_eq!(
        events.last(),
        Some(&TurnEvent::TurnEnded {
            outcome: TurnOutcome::Completed
        }),
        "the nested turn did not complete: {events:#?}"
    );

    let [child_id] = children_of(fixture.pool(), &fixture.thread_id)
        .await
        .try_into()
        .expect("one child");
    let [grandchild_id] = children_of(fixture.pool(), &child_id)
        .await
        .try_into()
        .expect("one grandchild");
    assert!(
        children_of(fixture.pool(), &grandchild_id).await.is_empty(),
        "the depth limit let a fourth generation through"
    );

    // The refusal is a tool error the model can read, and it created nothing.
    let grandchild = crate::database::local::agent_threads::get_thread(
        fixture.pool(),
        &grandchild_id,
        Some(OWNER),
    )
    .await
    .expect("grandchild");
    let refusal = Transcript::from_rows(&grandchild.messages)
        .expect("grandchild transcript")
        .messages
        .iter()
        .flat_map(|message| message.parts.clone())
        .find_map(|part| match part {
            AgentChatPart::Tool(tool) if tool.tool_name() == "subagent" => tool.error_text,
            _ => None,
        })
        .expect("a refused subagent call");
    assert!(refusal.contains("nested"), "{refusal}");

    // Both workspaces are published and retired, and the live document moved
    // exactly once — at the top-level merge.
    assert_eq!(open_drafts(fixture.pool(), &child_id).await, 0);
    assert_eq!(open_drafts(fixture.pool(), &grandchild_id).await, 0);
    assert_eq!(live_clips(&fixture).await, before);
    let probes = probes.lock().expect("poisoned").clone();
    let [probe] = probes.as_slice() else {
        panic!("expected one probe: {probes:#?}");
    };
    assert_eq!(probe.thread_id, grandchild_id);
    assert_eq!(probe.live_clips, before);

    // The nested child published into the *child's* workspace, not the live
    // document — one merge call, two shapes.
    let child =
        crate::database::local::agent_threads::get_thread(fixture.pool(), &child_id, Some(OWNER))
            .await
            .expect("child thread");
    let nested = Transcript::from_rows(&child.messages)
        .expect("child transcript")
        .messages
        .iter()
        .flat_map(|message| message.parts.clone())
        .find_map(|part| match part {
            AgentChatPart::Tool(tool) if tool.tool_name() == "subagent" => tool.output,
            _ => None,
        })
        .expect("the nested subagent chip");
    assert_eq!(nested["childThreadId"], json!(grandchild_id));
    assert_eq!(nested["outcome"]["status"], json!("merged"));
}

/// A real model reading a real tool description, delegating, and reading the
/// child's answer back. What the scripted tests cannot reach: whether the
/// description makes the tool *usable*, and whether a provider tolerates a
/// tool call that takes a whole turn to return.
///
/// Ignored by default — it costs tokens and needs a network. Run with
/// `cargo test --lib a_live_subagent -- --ignored --nocapture`, with
/// `LUMA_AI_GATEWAY_API_KEY` set.
#[tokio::test]
#[ignore = "live: needs LUMA_AI_GATEWAY_API_KEY and a network"]
async fn a_live_subagent_answers_its_parent() {
    let key = std::env::var(super::model::Provider::VercelAiGateway.key_env_var())
        .expect("no gateway credential: set LUMA_AI_GATEWAY_API_KEY to smoke-test");
    let fixture = fixture().await;
    let probes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = AgentService::new(fixture.services.clone())
        .with_model(Arc::new(super::model::anthropic::AnthropicClient::gateway(
            key,
        )))
        .with_tools(ToolRegistry::new(vec![
            Arc::new(ProbeTool(Arc::clone(&probes))),
            Arc::new(super::tools::subagent::SubagentTool),
        ]));

    let mut stream = service.turn(
        &fixture.thread_id,
        "Delegate to a subagent: ask it to reply with exactly the word GERANIUM          and nothing else. Then tell me the word it replied with."
            .to_string()
            .into(),
    );
    let events = drain(&mut stream).await;
    assert_eq!(
        events.last(),
        Some(&TurnEvent::TurnEnded {
            outcome: TurnOutcome::Completed
        }),
        "the live delegation did not complete: {events:#?}"
    );

    let [child_id] = children_of(fixture.pool(), &fixture.thread_id)
        .await
        .try_into()
        .expect("the model never delegated");
    println!("--- child thread {child_id} ---");
    let rows = crate::database::local::agent_threads::list_messages(
        fixture.pool(),
        &fixture.thread_id,
        Some(OWNER),
    )
    .await
    .expect("messages");
    let reply = Transcript::from_rows(&rows)
        .expect("transcript")
        .messages
        .iter()
        .map(AgentChatMessage::text)
        .collect::<Vec<_>>()
        .join("\n");
    println!("{reply}");
    assert!(
        reply.contains("GERANIUM"),
        "the parent never read the child's answer back: {reply}"
    );
    assert_eq!(open_drafts(fixture.pool(), &child_id).await, 0);
}

/// Cancelling the parent cancels the child: the child's turn is awaited inside
/// the parent's, so dropping the stream drops the whole chain.
#[tokio::test]
async fn cancelling_the_parent_turn_cancels_its_child() {
    let fixture = fixture().await;
    let child_id = cancelled_child(&fixture).await;
    let child =
        crate::database::local::agent_threads::get_thread(fixture.pool(), &child_id, Some(OWNER))
            .await
            .expect("child thread");
    assert!(
        child.messages.iter().all(|row| row.role == "user"),
        "a cancelled child must not persist an assistant row: {:#?}",
        child.messages
    );
    // The child's record survives the cancellation; its workspace is left
    // active for the sweep, which is the only other way one ends.
    assert_eq!(
        child.thread.parent_thread_id.as_deref(),
        Some(fixture.thread_id.as_str())
    );
}

/// Start a delegation and drop the parent stream mid-child, returning the
/// child thread that is left holding an active workspace.
async fn cancelled_child(fixture: &Fixture) -> String {
    let probes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = AgentService::new(fixture.services.clone())
        .with_model(Arc::new(
            ScriptedModel::new(vec![
                delegate_step("call_1", "Fitting the ramp", "Take your time."),
                call_step("call_2", "probe", "{}"),
                reply_step("finished anyway"),
                reply_step("delegated"),
            ])
            .with_cadence(std::time::Duration::from_millis(30)),
        ))
        .with_tools(ToolRegistry::new(vec![
            Arc::new(ProbeTool(probes)),
            Arc::new(super::tools::subagent::SubagentTool),
        ]));

    let mut stream = service.turn(&fixture.thread_id, "delegate it".to_string().into());
    while let Some(event) = stream.next().await {
        if matches!(event, TurnEvent::Subagent { .. }) {
            break;
        }
    }
    drop(stream);

    let [child_id] = children_of(fixture.pool(), &fixture.thread_id)
        .await
        .try_into()
        .expect("the child thread was created before the drop");
    assert_eq!(
        open_drafts(fixture.pool(), &child_id).await,
        1,
        "a cancelled child leaves its workspace active"
    );
    child_id
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

#[tokio::test]
async fn a_turn_with_one_tool_call_persists_its_assistant_row() {
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
        Some(OWNER),
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
}

#[tokio::test]
async fn steering_mid_turn_persists_every_assistant_row() {
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
        Some(OWNER),
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
}

#[tokio::test]
async fn rehydration_replays_the_tool_result_to_the_model() {
    let fixture = fixture().await;
    let scripted = Arc::new(ScriptedModel::new(tool_then_reply()));
    let service = AgentService::new(fixture.services.clone())
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
    let service = AgentService::new(fixture.services.clone())
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
        Some(OWNER),
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
        Some(OWNER),
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
    let service =
        AgentService::new(fixture.services.clone()).with_model(Arc::new(ScriptedModel::new(vec![
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
        ])));
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
        parent_thread_id: None,
        parent_call_id: None,
        title: title.map(ToString::to_string),
        actor: None,
        engine: "api".into(),
        model: Some(crate::agent::model::DEFAULT_MODEL.into()),
        provider: Some("vercel-ai-gateway".into()),
        effort: None,
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

#[tokio::test]
async fn a_threads_model_is_independent_of_later_default_changes() {
    use crate::agent::engine::catalog::{Selection, Service};
    let fixture = fixture().await;
    let scripted = Arc::new(ScriptedModel::new(vec![vec![ModelEvent::TextDelta(
        "shared runtime".into(),
    )]]));
    let agent = AgentService::new(fixture.services.clone()).with_model(scripted.clone());
    agent
        .set_thread_selection(
            &fixture.thread_id,
            Selection {
                service: Service::OpenRouter,
                model: Some("kimi-k3-fast".into()),
                effort: Some("low".into()),
            },
        )
        .await
        .unwrap();
    for (key, value) in [("agent_engine", "claude"), ("agent_model", "grok-4.5")] {
        crate::database::local::settings::update_setting(fixture.pool(), key, value)
            .await
            .unwrap();
    }
    let mut turn = agent.turn(&fixture.thread_id, "Answer".to_string().into());
    let mut completed = false;
    while let Some(event) = turn.next().await {
        if let TurnEvent::TurnEnded { outcome } = event {
            assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
            completed = true;
        }
    }
    assert!(completed);
    assert_eq!(scripted.requests()[0].model.spec().key, "kimi-k3-fast");
    assert_eq!(scripted.requests()[0].reasoning, model::ReasoningLevel::Low);
}

struct ContextProbe(Arc<std::sync::Mutex<Vec<crate::models::agent_execution::PythonScopeInput>>>);

#[async_trait]
impl Tool for ContextProbe {
    fn name(&self) -> &'static str {
        "context_probe"
    }
    fn description(&self) -> std::borrow::Cow<'static, str> {
        "Read the working context.".into()
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    async fn call(&self, ctx: &ToolContext<'_>, _: Value) -> Result<Value, String> {
        self.0.lock().unwrap().push(ctx.scope.clone());
        Ok(json!({"track": ctx.scope.track_id, "venue": ctx.scope.venue_id}))
    }
}

#[tokio::test]
async fn one_conversation_follows_turn_context_without_changing_identity() {
    let fixture = fixture().await;
    let service = AgentService::new(fixture.services.clone());
    let thread = service
        .new_thread(&ThreadScope::venue("venue-1"))
        .await
        .unwrap()
        .thread;
    sqlx::query("INSERT INTO tracks (id, uid, track_hash, title, file_path) VALUES ('track-2', NULL, 'hash-2', 'Second', '/tmp/second.wav'); INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES ('score-2', NULL, 'track-2', 'venue-1', 'Second');")
        .execute(fixture.pool()).await.unwrap();
    let contexts = [
        Some(ThreadScope::track("track-1", "venue-1", "score-1")),
        Some(ThreadScope::venue("venue-1")),
        None,
        Some(ThreadScope::track("track-2", "venue-1", "score-2")),
    ];
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let steps = contexts
        .iter()
        .enumerate()
        .flat_map(|(i, _)| {
            [
                call_step(&format!("context-{i}"), "context_probe", "{}"),
                reply_step("Done"),
            ]
        })
        .collect();
    let service = service
        .with_model(Arc::new(ScriptedModel::new(steps)))
        .with_tools(ToolRegistry::new(vec![Arc::new(ContextProbe(
            seen.clone(),
        ))]));
    for scope in contexts {
        let events = drain(&mut service.turn(
            &thread.id,
            UserPrompt {
                text: "Inspect the current context".into(),
                context: Some(TurnContext { scope }),
            },
        ))
        .await;
        assert_eq!(
            events.last(),
            Some(&TurnEvent::TurnEnded {
                outcome: TurnOutcome::Completed
            }),
            "{events:#?}"
        );
    }
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .map(|scope| scope.track_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("track-1"), None, None, Some("track-2")]
    );
    assert_eq!(
        seen.iter()
            .map(|scope| scope.venue_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("venue-1"), Some("venue-1"), None, Some("venue-1")]
    );
    let reopened = service.open_thread(&thread.id).await.unwrap();
    assert_eq!(reopened.messages.len(), 8);
    assert_eq!(reopened.thread.id, thread.id);
    assert_eq!(
        reopened.thread.agent_kind, "venue_rig",
        "creation metadata must remain immutable"
    );
}

/// A real native JSON-lines session delegates to two real child turns. The
/// parent's notebook read cannot return until both children have entered their
/// tool calls, and neither child can finish until that read releases them.
#[cfg(unix)]
#[tokio::test]
async fn native_parallel_children_do_not_block_parent_tools() {
    use std::os::unix::fs::PermissionsExt;
    struct Gate {
        started: tokio::sync::Semaphore,
        release: tokio::sync::Semaphore,
        cancel_release: tokio::sync::Semaphore,
        cancel_started: tokio::sync::mpsc::UnboundedSender<String>,
        cancel_dropped: tokio::sync::mpsc::UnboundedSender<String>,
        late_completions: std::sync::atomic::AtomicUsize,
    }
    struct CancelWitness {
        thread_id: String,
        dropped: tokio::sync::mpsc::UnboundedSender<String>,
    }
    impl Drop for CancelWitness {
        fn drop(&mut self) {
            let _ = self.dropped.send(self.thread_id.clone());
        }
    }
    #[async_trait]
    impl Tool for Gate {
        fn name(&self) -> &'static str {
            "python"
        }
        fn description(&self) -> std::borrow::Cow<'static, str> {
            "Causal test gate".into()
        }
        fn schema(&self) -> Value {
            json!({"type":"object"})
        }
        async fn call(&self, ctx: &ToolContext<'_>, args: Value) -> Result<Value, String> {
            if args["cancel"] == true {
                let _witness = CancelWitness {
                    thread_id: ctx.thread_id.to_owned(),
                    dropped: self.cancel_dropped.clone(),
                };
                self.cancel_started.send(ctx.thread_id.to_owned()).unwrap();
                self.cancel_release.acquire().await.unwrap().forget();
                self.late_completions
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(json!({"unexpectedLateReply":true}));
            }
            if args["child"] == true {
                self.started.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            } else {
                self.started.acquire_many(2).await.unwrap().forget();
                self.release.add_permits(2);
            }
            Ok(json!({"ok":true}))
        }
    }
    let fixture = fixture().await;
    sqlx::query("UPDATE agent_threads SET engine='codex', model=NULL, effort=NULL WHERE id=?")
        .bind(&fixture.thread_id)
        .execute(fixture.pool())
        .await
        .unwrap();
    let executable = fixture._dir.path().join("scripted-codex");
    std::fs::write(&executable, r#"#!/usr/bin/env python3
import json,sys,uuid,os,pathlib
read=lambda: json.loads(sys.stdin.readline())
def send(x): print(json.dumps(x),flush=True)
def call(id,name,args): send({'id':id,'method':'item/tool/call','params':{'callId':id,'tool':name,'arguments':args}})
assert read()['method']=='initialize'
send({'id':1,'result':{}})
assert read()['method']=='initialized'
assert read()['method']=='account/read'
send({'id':4,'result':{'account':{'type':'chatgpt'}}})
assert read()['method']=='config/read'
send({'id':5,'result':{'config':{}}})
start=read()
assert start['method'] in ['thread/start','thread/resume']
for tool in start['params']['dynamicTools']:
    assert tool['inputSchema']['type']=='object', tool
    assert 'anyOf' not in tool['inputSchema'], tool
send({'id':2,'result':{'thread':{'id':str(uuid.uuid4())}}})
turn=read()
send({'id':3,'result':{}})
prompt=turn['params']['input'][0]['text']
if 'CANCEL_CHILD' in prompt:
    pathlib.Path(__file__).with_name('cancel-' + str(os.getpid()) + '.pid').write_text(str(os.getpid()))
    call('cancel-child-read','python',{'cancel':True})
    read()
    pathlib.Path(__file__).with_name('late-reply-' + str(os.getpid())).touch()
elif 'CANCEL_PARENT' in prompt:
    call('cancel-child-a','subagent',{'description':'Cancel first child','task':'CANCEL_CHILD'})
    call('cancel-child-b','subagent',{'description':'Cancel second child','task':'CANCEL_CHILD'})
    read()
    read()
elif 'EARLY_DONE' in prompt:
    call('unfinished','python',{'child':True})
elif 'GATED_CHILD' in turn['params']['input'][0]['text']:
    call('child-read','python',{'child':True})
    assert read()['result']['success']
else:
    call('child-a','subagent',{'description':'First child','task':'GATED_CHILD'})
    call('child-b','subagent',{'description':'Second child','task':'GATED_CHILD'})
    call('parent-read','python',{'child':False})
    replies=[read(),read(),read()]
    assert {x['id'] for x in replies}=={'child-a','child-b','parent-read'}
    assert all(x['result']['success'] for x in replies), replies
send({'method':'item/agentMessage/delta','params':{'delta':'Finished.'}})
send({'method':'item/completed','params':{'item':{'type':'agentMessage','phase':'final_answer','text':'Finished.'}}})
send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
"#).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    struct RestoreEnv(Option<std::ffi::OsString>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            if let Some(value) = &self.0 {
                std::env::set_var("LUMA_CODEX_EXECUTABLE", value);
            } else {
                std::env::remove_var("LUMA_CODEX_EXECUTABLE");
            }
        }
    }
    let _restore = RestoreEnv(std::env::var_os("LUMA_CODEX_EXECUTABLE"));
    std::env::set_var("LUMA_CODEX_EXECUTABLE", &executable);
    let (cancel_started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (cancel_dropped, mut drops) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(Gate {
        started: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        cancel_release: tokio::sync::Semaphore::new(0),
        cancel_started,
        cancel_dropped,
        late_completions: std::sync::atomic::AtomicUsize::new(0),
    });
    let service = AgentService::new(fixture.services.clone()).with_tools(ToolRegistry::new(vec![
        Arc::new(super::tools::subagent::SubagentTool),
        gate.clone(),
    ]));
    let mut stream = service.turn(
        &fixture.thread_id,
        "Run concurrent children".to_string().into(),
    );
    let events = tokio::time::timeout(std::time::Duration::from_secs(15), drain(&mut stream))
        .await
        .expect("parent read must release both children without deadlock");
    assert_eq!(
        events.last(),
        Some(&TurnEvent::TurnEnded {
            outcome: TurnOutcome::Completed
        }),
        "{events:#?}"
    );
    let snapshot = crate::database::local::agent_threads::get_thread(
        fixture.pool(),
        &fixture.thread_id,
        Some(OWNER),
    )
    .await
    .unwrap();
    let transcript = Transcript::from_rows(&snapshot.messages).unwrap();
    let calls: Vec<_> = transcript
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter_map(|p| match p {
            AgentChatPart::Tool(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 3);
    assert!(
        calls.iter().all(|t| t.output.is_some()),
        "overlapping tool replies must all persist"
    );
    for child in children_of(fixture.pool(), &fixture.thread_id).await {
        assert!(!fixture.services.subagents.is_running(&child));
    }
    let mut stream = service.turn(&fixture.thread_id, "EARLY_DONE".to_string().into());
    let events = tokio::time::timeout(std::time::Duration::from_secs(15), drain(&mut stream))
        .await
        .unwrap();
    assert!(
        matches!(events.last(), Some(TurnEvent::TurnEnded { outcome: TurnOutcome::Failed { message } }) if message.contains("unfinished tool calls")),
        "{events:#?}"
    );
    assert!(events.iter().any(|event| matches!(event, TurnEvent::ToolCallEnded { call_id, output: ToolResult::Failed { message } } if call_id == "unfinished" && message.contains("cancelled"))));

    // Dropping the actual native parent stream is a different branch from
    // premature provider Done: no reader remains to consume terminal events.
    // Observe the children's owned tool futures and processes directly instead.
    let head_before = live_clips(&fixture).await;
    let mut stream = service.turn(&fixture.thread_id, "CANCEL_PARENT".to_string().into());
    let children = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let mut children = std::collections::BTreeSet::new();
        while children.len() < 2 {
            tokio::select! {
                child = starts.recv() => { children.insert(child.expect("child gate entered")); }
                event = stream.next() => { assert!(event.is_some(), "parent ended before both children blocked"); }
            }
        }
        children
    }).await.expect("both native child turns must enter their tool gates");
    let pids: Vec<i32> = std::fs::read_dir(fixture._dir.path())
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            (name.starts_with("cancel-") && name.ends_with(".pid")).then(|| {
                std::fs::read_to_string(entry.path())
                    .unwrap()
                    .parse()
                    .unwrap()
            })
        })
        .collect();
    assert_eq!(
        pids.len(),
        2,
        "both actual native child processes were running"
    );
    for child in &children {
        assert!(fixture.services.subagents.is_running(child));
        assert_eq!(open_drafts(fixture.pool(), child).await, 1);
    }
    assert_eq!(live_clips(&fixture).await, head_before);
    drop(stream);
    let cancelled = std::collections::BTreeSet::from([
        drops.try_recv().expect("first child future dropped"),
        drops.try_recv().expect("second child future dropped"),
    ]);
    assert_eq!(
        cancelled, children,
        "parent drop cancelled both child tool futures synchronously"
    );
    for child in &children {
        assert!(!fixture.services.subagents.is_running(child));
    }
    gate.cancel_release.add_permits(2);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            // SAFETY: signal zero only probes these child PIDs; it sends no signal.
            if pids.iter().all(|pid| unsafe { libc::kill(*pid, 0) } == -1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both native child processes must be reaped after cancellation");
    assert_eq!(
        gate.late_completions
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert!(
        std::fs::read_dir(fixture._dir.path())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("late-reply-")),
        "cancelled native tools must never send a late result"
    );
    for child in &children {
        // A drop cannot await, so a cancelled child leaves its draft open. It
        // is a private row nobody reads, and it goes when the thread does.
        assert_eq!(open_drafts(fixture.pool(), child).await, 1);
    }
    assert_eq!(
        live_clips(&fixture).await,
        head_before,
        "cancelled children never published live changes"
    );
}
