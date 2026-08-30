//! The turn protocol.
//!
//! The ordering is a durability contract, not UI logic:
//!
//! ```text
//! persist(user) → model step(s) → prepare_turn → persist(assistant) → finalize_turn
//! ```
//!
//! `prepare_turn` runs **once per assistant row**, immediately before that
//! row's insert. That is the fix for the steering violation of the
//! authored-turn invariant: the TypeScript loop prepared once per *user
//! prompt*, so a steered turn produced two assistant rows and prepared only the
//! last, leaving the other unprepared under a trigger that requires exactly one
//! preparation per assistant row. Pairing preparation with the row rather than
//! with the prompt makes the invariant hold by construction.
//!
//! Preparation is at the row's *close*, not its open, because it snapshots the
//! authored document the turn produced — a snapshot taken before the tools ran
//! would record the wrong state.

use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use super::model::{
    self, CacheRetention, ModelClient, ModelEvent, ModelId, ModelMessage, ModelRequest,
    ReasoningLevel, StopReason, Usage,
};
use super::tools::{self, ToolContext, ToolProgress, ToolRegistry};
use super::transcript::{self, Transcript};
use super::{
    AgentChatMessage, AgentError, AgentKind, AgentService, Role, ToolResult, TurnEvent,
    TurnOutcome, UserPrompt,
};
use crate::database::local::agent_threads as db;
use crate::models::agent_execution::PythonScopeInput;
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AgentThreadUsage, AppendAgentThreadMessagesInput,
    NewAgentThreadMessage, ThreadRoute,
};
use crate::models::authored_state::{
    AuthoredTurnCommit, FinalizeAuthoredTurnInput, PrepareAuthoredTurnInput,
};

/// Output ceiling for one model step. Generous: the ceiling exists to bound a
/// runaway, not to shape a response.
pub(super) const MAX_TOKENS: u32 = 32_000;

pub(super) async fn run(
    service: AgentService,
    thread_id: String,
    prompt: UserPrompt,
    events: mpsc::UnboundedSender<TurnEvent>,
    steer: mpsc::UnboundedReceiver<String>,
) {
    let mut turn = Turn {
        service,
        thread_id,
        events,
        steer,
        transcript: Transcript::default(),
        head: None,
        principal: None,
        spend: AgentThreadUsage::default(),
    };
    let outcome = match turn.drive(prompt).await {
        Ok(()) => TurnOutcome::Completed,
        Err(error) => TurnOutcome::Failed {
            message: error.to_string(),
        },
    };
    turn.emit(TurnEvent::TurnEnded { outcome });
}

struct Turn {
    service: AgentService,
    thread_id: String,
    events: mpsc::UnboundedSender<TurnEvent>,
    steer: mpsc::UnboundedReceiver<String>,
    transcript: Transcript,
    /// The durable transcript tip this turn has observed. Every append is a
    /// compare-and-swap against it.
    head: Option<String>,
    principal: Option<String>,
    /// This thread's running cost, seeded from what is already recorded and
    /// written back at every row boundary. Cumulative rather than per-turn
    /// because the ledger holds one row per thread — see
    /// [`AgentThreadUsage`] — and a turn that only knew its own tokens would
    /// erase the ones spent before it.
    ///
    /// `subagents` stays whatever was stored: a child of an in-app turn is a
    /// thread of its own, so it accounts for itself and its revisions already
    /// carry its id back to the same score.
    spend: AgentThreadUsage,
}

/// What a turn resolves once and every assistant row in it then reuses. Only
/// the turn message varies across rows, and it varies per prompt, so it stays
/// an argument rather than joining this.
struct TurnSetup<'a> {
    client: &'a dyn ModelClient,
    model: ModelId,
    reasoning: ReasoningLevel,
    kind: AgentKind,
    registry: &'a ToolRegistry,
    scope: &'a PythonScopeInput,
    /// The private authored head this thread writes to, for a subagent
    /// thread. Resolved once, from the thread, and handed to every tool call:
    /// a child's Python namespace and its authored writes then address the
    /// same detached state `prepare_turn` will finalize, and no tool is in a
    /// position to disagree about which.
    workspace_id: Option<&'a str>,
    /// Whether this thread's assistant rows reserve authored state. A venue
    /// thread revises the room's relational rig, which has no revision history
    /// to stage against, so its rows close without a preparation — the
    /// invariant is "one preparation per assistant row *of a document
    /// thread*", and this is where the two are told apart.
    authored: bool,
    /// Resolved once per turn rather than once per step: every step of a turn
    /// writes into the same prefix, and re-reading the environment mid-turn
    /// could change the TTL under a cache that is already warm.
    cache_retention: CacheRetention,
}

impl Turn {
    /// Fold the event into the transcript, then hand it to the host. The two
    /// stay in lockstep because rehydration reads the same transcript.
    fn emit(&mut self, event: TurnEvent) {
        transcript::apply(&mut self.transcript, &event);
        let _ = self.events.send(event);
    }

    async fn drive(&mut self, prompt: UserPrompt) -> Result<(), AgentError> {
        let pool = self.service.services().db().0.clone();
        self.principal = self.service.principal().await?;

        let detail = db::get_thread(&pool, &self.thread_id, self.principal.as_deref())
            .await
            .map_err(AgentError::Storage)?;
        self.spend = db::thread_usage(&pool, &self.thread_id)
            .await
            .map_err(AgentError::Storage)?
            .unwrap_or_default();
        self.spend.thread_id.clone_from(&self.thread_id);
        let kind = AgentKind::parse(&detail.thread.agent_kind)?;
        let authored = matches!(
            detail.thread.route().map_err(AgentError::Invalid)?,
            ThreadRoute::Authored(_)
        );
        self.transcript = Transcript::from_rows(&detail.messages).map_err(AgentError::Invalid)?;
        self.head = self.transcript.head_message_id();

        let registry = self
            .service
            .tools
            .clone()
            .unwrap_or_else(|| tools::registry(kind));
        let scope = python_scope(&detail.thread);
        // Only a document thread can have one: a workspace is a detached head
        // of an authored document, and a venue thread has no document to
        // detach from.
        let workspace_id = match authored {
            false => None,
            true => self
                .service
                .services()
                .authored()
                .thread_workspace(&pool, self.principal.as_deref(), &self.thread_id)
                .await
                .map_err(|error| AgentError::Storage(error.to_string()))?,
        };
        let (client, model, reasoning) = self.resolve_model().await?;
        let setup = TurnSetup {
            client: &*client,
            model,
            reasoning,
            kind,
            registry: &registry,
            scope: &scope,
            workspace_id: workspace_id.as_deref(),
            authored,
            cache_retention: CacheRetention::from_env(),
        };

        let mut turn_message_id = self.append_user(&prompt.text).await?;
        // The thread's actor is restamped per turn, not per thread: the model
        // is chosen per turn, and this is the only point that knows which one
        // is about to answer. Every revision the turn writes reads it back off
        // the thread and keeps its own copy. After the first durable append, so
        // that a thread the caller may not write to fails as it always did.
        db::set_thread_actor(
            &pool,
            &self.thread_id,
            setup.model.key(),
            self.principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)?;

        loop {
            let (stop_reason, usage, assistant_id) =
                self.assistant_row(&setup, &turn_message_id).await?;
            self.close_row(&setup, &assistant_id, stop_reason, usage)
                .await?;
            // After the row is durable, so a run's recorded price never
            // describes work the transcript does not have.
            self.spend.turns += 1;
            db::record_thread_usage(&pool, &self.spend)
                .await
                .map_err(AgentError::Storage)?;

            // Steering is applied here and nowhere else: between one durable
            // assistant row and the next, so each row keeps its own preparation.
            match self.steer.try_recv() {
                Ok(text) => turn_message_id = self.append_user(&text).await?,
                Err(_) => return Ok(()),
            }
        }
    }

    /// Add one model step to this thread's running cost.
    ///
    /// Tokens and wall time only. Nothing here prices them: the loop is told
    /// token counts by the provider and would have to guess at dollars from a
    /// rate card kept in the tree, which is a second source of truth that rots
    /// silently. A harness that is *told* the price fills `cost_usd` in.
    fn charge(&mut self, model: &str, usage: Usage, elapsed: std::time::Duration) {
        let count = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        self.spend.model = Some(model.to_string());
        self.spend.input_tokens += count(usage.input_tokens);
        self.spend.output_tokens += count(usage.output_tokens);
        self.spend.cache_creation_tokens += count(usage.cache_creation_input_tokens);
        self.spend.cache_read_tokens += count(usage.cache_read_input_tokens);
        self.spend.duration_ms += i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
    }

    /// One assistant row: as many model steps as the model asks for, with tool
    /// calls run between them. Returns the last step's stop reason and usage.
    async fn assistant_row(
        &mut self,
        setup: &TurnSetup<'_>,
        turn_message_id: &str,
    ) -> Result<(StopReason, Usage, String), AgentError> {
        let assistant_id = uuid::Uuid::new_v4().to_string();
        self.emit(TurnEvent::MessageStarted {
            id: assistant_id.clone(),
            role: Role::Assistant,
        });

        loop {
            self.emit(TurnEvent::StepStarted);
            let request = ModelRequest {
                model: setup.model,
                system: vec![setup.kind.system_prompt().to_string()],
                messages: self.model_messages(setup.registry),
                tools: setup.registry.specs(),
                reasoning: setup.reasoning,
                max_tokens: MAX_TOKENS,
                cache_retention: setup.cache_retention,
            };
            let started = std::time::Instant::now();
            let (stop_reason, usage, calls) = self.stream_step(setup.client, request).await?;
            self.charge(setup.model.key(), usage, started.elapsed());
            self.emit(TurnEvent::StepEnded {
                stop_reason,
                usage,
                model: setup.model.key().to_string(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });

            if calls.is_empty() {
                return Ok((stop_reason, usage, assistant_id));
            }
            for call in calls {
                let output = self.run_tool(setup, turn_message_id, &call).await;
                self.emit(TurnEvent::ToolCallEnded {
                    call_id: call.id,
                    output,
                });
            }
        }
    }

    /// The transcript as the model sees it, minus the row currently being
    /// written — that row *is* the response in progress.
    fn model_messages(&self, registry: &ToolRegistry) -> Vec<ModelMessage> {
        transcript::to_model_messages(&self.transcript, registry)
    }

    async fn stream_step(
        &mut self,
        client: &dyn ModelClient,
        request: ModelRequest,
    ) -> Result<(StopReason, Usage, Vec<PendingCall>), AgentError> {
        let mut stream = client.stream(request);
        let mut pending: Vec<PendingCall> = Vec::new();
        let mut ready: Vec<PendingCall> = Vec::new();

        while let Some(event) = stream.next().await {
            match event? {
                ModelEvent::TextDelta(text) => self.emit(TurnEvent::TextDelta { text }),
                ModelEvent::ReasoningDelta(text) => self.emit(TurnEvent::ReasoningDelta { text }),
                ModelEvent::ToolCallStarted { id, name } => pending.push(PendingCall {
                    id,
                    name,
                    arguments: String::new(),
                }),
                ModelEvent::ToolCallArgsDelta { id, json } => {
                    if let Some(call) = pending.iter_mut().find(|call| call.id == id) {
                        call.arguments.push_str(&json);
                    }
                }
                ModelEvent::ToolCallEnded { id } => {
                    let Some(at) = pending.iter().position(|call| call.id == id) else {
                        continue;
                    };
                    let call = pending.remove(at);
                    // The host sees a call only once its arguments parse: a
                    // half-built object is not something a chip can label.
                    self.emit(TurnEvent::ToolCallStarted {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        input: call.input(),
                    });
                    ready.push(call);
                }
                ModelEvent::StepEnded { stop_reason, usage } => {
                    return Ok((stop_reason, usage, ready))
                }
            }
        }
        // A stream that ends without a step boundary is a provider that hung
        // up; treat it as a finished step rather than hanging the turn.
        Ok((StopReason::EndTurn, Usage::default(), ready))
    }

    async fn run_tool(
        &self,
        setup: &TurnSetup<'_>,
        turn_message_id: &str,
        call: &PendingCall,
    ) -> ToolResult {
        let Some(tool) = setup.registry.get(&call.name) else {
            return ToolResult::Failed {
                message: format!("no tool named '{}'", call.name),
            };
        };
        let progress = ToolProgress::new(self.events.clone());
        let context = ToolContext {
            agent: &self.service,
            thread_id: &self.thread_id,
            call_id: &call.id,
            turn_message_id,
            // A subagent thread's Python namespace *is* its workspace: the
            // execution service checks the two are the same string and
            // authorizes both against this thread.
            execution_id: setup.workspace_id,
            authored_workspace_id: setup.workspace_id,
            scope: setup.scope,
            progress: &progress,
        };
        match tool.call(&context, call.input()).await {
            Ok(value) => ToolResult::Output { value },
            Err(message) => ToolResult::Failed { message },
        }
    }

    /// Persist the user's message before any remote call is made: the prompt is
    /// durable before it can produce a response. Returns its id, which is the
    /// turn message every tool call in the rows that follow is attributed to.
    async fn append_user(&mut self, text: &str) -> Result<String, AgentError> {
        let id = uuid::Uuid::new_v4().to_string();
        self.emit(TurnEvent::MessageStarted {
            id: id.clone(),
            role: Role::User,
        });
        self.emit(TurnEvent::TextDelta {
            text: text.to_string(),
        });
        let message = AgentChatMessage::user(id.clone(), text);
        self.append(&message).await?;
        Ok(id)
    }

    /// Close one assistant row: reserve its authored state, insert it, then
    /// commit. The row becomes durable only after an immutable prepared
    /// revision exists, so a crash between the two is recoverable
    /// (`authored_state_recover_turns`) rather than a lost association.
    ///
    /// A thread that revises no authored document has nothing to reserve, and
    /// its row is simply appended — see [`TurnSetup::authored`].
    async fn close_row(
        &mut self,
        setup: &TurnSetup<'_>,
        assistant_id: &str,
        stop_reason: StopReason,
        usage: Usage,
    ) -> Result<(), AgentError> {
        if !setup.authored {
            let row = self.assistant_row_of(assistant_id)?;
            self.append(&row).await?;
            self.emit(TurnEvent::MessageEnded {
                id: assistant_id.to_string(),
                stop_reason,
                usage,
            });
            return Ok(());
        }
        let pool = self.service.services().db().0.clone();
        let authored = self.service.services().authored().clone();
        let prepared = authored
            .prepare_turn(
                &pool,
                self.principal.as_deref(),
                PrepareAuthoredTurnInput {
                    thread_id: self.thread_id.clone(),
                    assistant_message_id: assistant_id.to_string(),
                    // The authored document is the source of truth; there is no
                    // live editor graph to capture backend-side.
                    graph: None,
                },
            )
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?;

        let row = self.assistant_row_of(assistant_id)?;
        self.append(&row).await?;

        let commit = authored
            .finalize_turn(
                &pool,
                self.principal.as_deref(),
                FinalizeAuthoredTurnInput {
                    thread_id: self.thread_id.clone(),
                    assistant_message_id: assistant_id.to_string(),
                    prepared_revision_id: prepared.prepared_revision_id,
                },
            )
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?;
        match commit {
            AuthoredTurnCommit::Committed {
                revision_id,
                changed: true,
                ..
            } => self.emit(TurnEvent::DocumentChanged {
                revision: revision_id,
            }),
            AuthoredTurnCommit::Committed { .. } => {}
            AuthoredTurnCommit::Conflicted { conflicts, .. } => {
                return Err(AgentError::Storage(format!(
                    "authored turn conflicted on {} path(s); reload before continuing",
                    conflicts.len()
                )))
            }
        }

        self.emit(TurnEvent::MessageEnded {
            id: assistant_id.to_string(),
            stop_reason,
            usage,
        });
        Ok(())
    }

    /// The row this turn just produced, read back out of the transcript it was
    /// folded into.
    fn assistant_row_of(&self, assistant_id: &str) -> Result<AgentChatMessage, AgentError> {
        self.transcript
            .messages
            .iter()
            .rev()
            .find(|message| message.id == assistant_id)
            .cloned()
            .ok_or_else(|| AgentError::Invalid("assistant row vanished mid-turn".into()))
    }

    async fn append(&mut self, message: &AgentChatMessage) -> Result<(), AgentError> {
        let outcome = db::append_messages_at_head(
            &self.service.services().db().0,
            &self.thread_id,
            AppendAgentThreadMessagesInput {
                operation_id: uuid::Uuid::new_v4().to_string(),
                expected_head_message_id: self.head.clone(),
                messages: vec![NewAgentThreadMessage {
                    id: Some(message.id.clone()),
                    role: message.role.as_str().to_string(),
                    parts: message.parts_json(),
                }],
            },
            self.principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)?;
        match outcome {
            AgentThreadAppendOutcome::Appended {
                head_message_id, ..
            } => {
                self.head = Some(head_message_id);
                Ok(())
            }
            AgentThreadAppendOutcome::HeadMoved { .. } => Err(AgentError::HeadMoved),
        }
    }

    /// Which model, over which transport, with which key.
    async fn resolve_model(
        &self,
    ) -> Result<(Arc<dyn ModelClient>, ModelId, ReasoningLevel), AgentError> {
        let settings =
            crate::database::local::settings::get_all_settings(&self.service.services().db().0)
                .await
                .map_err(AgentError::Storage)?;
        if let Some(client) = &self.service.client {
            let id = model::configured(&settings)?;
            return Ok((Arc::clone(client), id, id.spec().default_reasoning));
        }
        Ok(model::configured_client(&settings, &self.service.services().db().0).await?)
    }
}

struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

impl PendingCall {
    /// Arguments that never arrived, or arrived malformed, become an empty
    /// object: the tool's own argument decoding is the one place that decides
    /// whether a call is usable.
    fn input(&self) -> Value {
        serde_json::from_str(self.arguments.trim())
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
    }
}

/// What the agent is looking at, read from the thread rather than asserted by
/// the model.
fn python_scope(thread: &AgentThread) -> PythonScopeInput {
    let subject = |kind: &str| {
        (thread.subject_kind.as_deref() == Some(kind))
            .then(|| thread.subject_id.clone())
            .flatten()
    };
    PythonScopeInput {
        track_id: subject("track"),
        venue_id: thread.venue_id.clone(),
        score_id: thread.score_id.clone(),
        pattern_id: subject("pattern"),
        implementation_id: thread.implementation_id.clone(),
        window: None,
        graph_definition: None,
    }
}
