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
        "INSERT INTO venues (id, uid, name) VALUES ('venue-1', NULL, 'Venue');
         INSERT INTO tracks (id, uid, track_hash, title, file_path)
         VALUES ('track-1', NULL, 'hash-1', 'Track', '/tmp/track.wav');
         INSERT INTO scores (id, uid, track_id, venue_id, name)
         VALUES ('score-1', NULL, 'track-1', 'venue-1', 'Score');",
    )
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
    /// The detached head the loop bound this call to. `Some` exactly when the
    /// call is running inside a subagent thread.
    workspace_id: Option<String>,
    /// The *live* document head at the moment of the call — the fact a
    /// subagent must not be able to move.
    live_head: String,
}

/// Records where it ran. The instrument for "a child writes its workspace and
/// the live document does not move until the merge".
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
        let live = services
            .authored()
            .current_revision(&services.db().0, None, ctx.thread_id)
            .await
            .map_err(|error| error.to_string())?;
        let probe = Probe {
            thread_id: ctx.thread_id.to_string(),
            workspace_id: ctx.authored_workspace_id.map(ToString::to_string),
            live_head: live.revision_id,
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

async fn active_workspaces(pool: &SqlitePool, thread_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM authored_subagent_workspaces
         WHERE owner_thread_id = ? AND status = 'active'",
    )
    .bind(thread_id)
    .fetch_one(pool)
    .await
    .expect("workspaces")
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

async fn live_head(fixture: &Fixture) -> String {
    fixture
        .services
        .authored()
        .current_revision(fixture.pool(), None, &fixture.thread_id)
        .await
        .expect("live head")
        .revision_id
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
    let before = live_head(&fixture).await;

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
    let child = crate::database::local::agent_threads::get_thread(fixture.pool(), &child_id, None)
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
        probe.workspace_id.is_some(),
        "a subagent's tools must address its workspace"
    );
    assert_eq!(
        probe.live_head, before,
        "the live head moved before the merge"
    );

    // Every assistant row the child wrote was prepared against that workspace,
    // never against the live document.
    let prepared: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT assistant_message_id, workspace_id FROM authored_turn_preparations
         WHERE thread_id = ?",
    )
    .bind(&child_id)
    .fetch_all(fixture.pool())
    .await
    .expect("preparations");
    assert!(!prepared.is_empty());
    assert!(
        prepared.iter().all(|(_, workspace)| workspace.is_some()),
        "a child turn prepared against the live head: {prepared:#?}"
    );

    // The merge happened, and the workspace is gone.
    assert_ne!(live_head(&fixture).await, before);
    assert_eq!(active_workspaces(fixture.pool(), &child_id).await, 0);

    // What the parent model got back.
    let rows = crate::database::local::agent_threads::list_messages(
        fixture.pool(),
        &fixture.thread_id,
        None,
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
    let before = live_head(&fixture).await;

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
    let grandchild =
        crate::database::local::agent_threads::get_thread(fixture.pool(), &grandchild_id, None)
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
    assert_eq!(active_workspaces(fixture.pool(), &child_id).await, 0);
    assert_eq!(active_workspaces(fixture.pool(), &grandchild_id).await, 0);
    assert_ne!(live_head(&fixture).await, before);
    let probes = probes.lock().expect("poisoned").clone();
    let [probe] = probes.as_slice() else {
        panic!("expected one probe: {probes:#?}");
    };
    assert_eq!(probe.thread_id, grandchild_id);
    assert_eq!(probe.live_head, before);

    // The nested child published into the *child's* workspace, not the live
    // document — one merge call, two shapes.
    let child = crate::database::local::agent_threads::get_thread(fixture.pool(), &child_id, None)
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
        None,
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
    assert_eq!(active_workspaces(fixture.pool(), &child_id).await, 0);
}

/// Cancelling the parent cancels the child: the child's turn is awaited inside
/// the parent's, so dropping the stream drops the whole chain.
#[tokio::test]
async fn cancelling_the_parent_turn_cancels_its_child() {
    let fixture = fixture().await;
    let child_id = cancelled_child(&fixture).await;
    let child = crate::database::local::agent_threads::get_thread(fixture.pool(), &child_id, None)
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
        active_workspaces(fixture.pool(), &child_id).await,
        1,
        "a cancelled child leaves its workspace active"
    );
    child_id
}

/// The sweep is what makes that leak temporary: a child whose turn is gone
/// loses its workspace, the parent's live document does not move, and a thread
/// that never had a parent is not a subagent and is never a candidate.
#[tokio::test]
async fn the_sweep_retires_a_stranded_childs_workspace_and_leaves_its_parent_alone() {
    let fixture = fixture().await;
    let child_id = cancelled_child(&fixture).await;
    let head_before = live_head(&fixture).await;

    let retired = crate::agent_execution::thread_cleanup::recover_threads(
        fixture.pool(),
        fixture.services.authored(),
        fixture.services.workspaces(),
        fixture.services.graph_runs(),
        &fixture.services.subagents,
    )
    .await
    .expect("sweep")
    .workspaces;

    assert_eq!(retired, 1, "only the child is a subagent thread");
    assert_eq!(active_workspaces(fixture.pool(), &child_id).await, 0);
    assert_eq!(
        active_workspaces(fixture.pool(), &fixture.thread_id).await,
        0,
        "the parent writes the live head and owns no workspace"
    );
    assert_eq!(live_head(&fixture).await, head_before);
    assert!(
        crate::database::local::agent_threads::get_thread(fixture.pool(), &child_id, None)
            .await
            .is_ok(),
        "retiring a workspace does not delete the thread that wrote it"
    );

    // Repeating it finds nothing: an active workspace is the whole candidate
    // set, so the sweep is idempotent by construction.
    let again = crate::agent_execution::thread_cleanup::recover_threads(
        fixture.pool(),
        fixture.services.authored(),
        fixture.services.workspaces(),
        fixture.services.graph_runs(),
        &fixture.services.subagents,
    )
    .await
    .expect("sweep")
    .workspaces;
    assert_eq!(again, 0);
}

/// A child the registry still holds a lease for is mid-turn. Retiring its
/// workspace would pull the head out from under a running write, so the sweep
/// leaves it and takes it on the next pass.
#[tokio::test]
async fn the_sweep_skips_a_child_whose_turn_is_still_running() {
    let fixture = fixture().await;
    let child_id = cancelled_child(&fixture).await;

    let mut lease = Arc::clone(&fixture.services.subagents)
        .acquire(&fixture.thread_id)
        .expect("slot");
    lease.attach(&child_id);

    let skipped = crate::agent_execution::thread_cleanup::recover_threads(
        fixture.pool(),
        fixture.services.authored(),
        fixture.services.workspaces(),
        fixture.services.graph_runs(),
        &fixture.services.subagents,
    )
    .await
    .expect("sweep")
    .workspaces;
    assert_eq!(skipped, 0);
    assert_eq!(active_workspaces(fixture.pool(), &child_id).await, 1);

    drop(lease);
    let retired = crate::agent_execution::thread_cleanup::recover_threads(
        fixture.pool(),
        fixture.services.authored(),
        fixture.services.workspaces(),
        fixture.services.graph_runs(),
        &fixture.services.subagents,
    )
    .await
    .expect("sweep")
    .workspaces;
    assert_eq!(retired, 1);
    assert_eq!(active_workspaces(fixture.pool(), &child_id).await, 0);
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
