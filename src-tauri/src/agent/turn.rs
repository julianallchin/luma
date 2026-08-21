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
    self, ModelClient, ModelEvent, ModelId, ModelMessage, ModelRequest, Provider, ReasoningLevel,
    StopReason, Usage,
};
use super::tools::{self, ToolContext, ToolRegistry};
use super::transcript::{self, Transcript};
use super::{
    AgentChatMessage, AgentError, AgentKind, AgentService, Role, ToolResult, TurnEvent,
    TurnOutcome, UserPrompt,
};
use crate::database::local::agent_threads as db;
use crate::models::agent_execution::PythonScopeInput;
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AppendAgentThreadMessagesInput, NewAgentThreadMessage,
};
use crate::models::authored_state::{
    AuthoredTurnCommit, FinalizeAuthoredTurnInput, PrepareAuthoredTurnInput,
};

/// Output ceiling for one model step. Generous: the ceiling exists to bound a
/// runaway, not to shape a response.
const MAX_TOKENS: u32 = 32_000;

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
        let kind = AgentKind::parse(&detail.thread.agent_kind)?;
        self.transcript = Transcript::from_rows(&detail.messages).map_err(AgentError::Invalid)?;
        self.head = self.transcript.head_message_id();

        let registry = self
            .service
            .tools
            .clone()
            .unwrap_or_else(|| tools::registry(kind));
        let scope = python_scope(&detail.thread);
        let (client, model, reasoning) = self.resolve_model().await?;

        self.append_user(&prompt.text).await?;

        loop {
            let (stop_reason, usage, assistant_id) = self
                .assistant_row(&*client, model, reasoning, kind, &registry, &scope)
                .await?;
            self.close_row(&assistant_id, stop_reason, usage).await?;

            // Steering is applied here and nowhere else: between one durable
            // assistant row and the next, so each row keeps its own preparation.
            match self.steer.try_recv() {
                Ok(text) => self.append_user(&text).await?,
                Err(_) => return Ok(()),
            }
        }
    }

    /// One assistant row: as many model steps as the model asks for, with tool
    /// calls run between them. Returns the last step's stop reason and usage.
    async fn assistant_row(
        &mut self,
        client: &dyn ModelClient,
        model: ModelId,
        reasoning: ReasoningLevel,
        kind: AgentKind,
        registry: &ToolRegistry,
        scope: &PythonScopeInput,
    ) -> Result<(StopReason, Usage, String), AgentError> {
        let assistant_id = uuid::Uuid::new_v4().to_string();
        self.emit(TurnEvent::MessageStarted {
            id: assistant_id.clone(),
            role: Role::Assistant,
        });

        loop {
            self.emit(TurnEvent::StepStarted);
            let request = ModelRequest {
                model,
                system: kind.system_prompt().to_string(),
                messages: self.model_messages(registry),
                tools: registry.specs(),
                reasoning,
                max_tokens: MAX_TOKENS,
            };
            let (stop_reason, usage, calls) = self.stream_step(client, request).await?;
            self.emit(TurnEvent::StepEnded { stop_reason, usage });

            if calls.is_empty() {
                return Ok((stop_reason, usage, assistant_id));
            }
            for call in calls {
                let output = self.run_tool(registry, scope, &assistant_id, &call).await;
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
        registry: &ToolRegistry,
        scope: &PythonScopeInput,
        assistant_id: &str,
        call: &PendingCall,
    ) -> ToolResult {
        let Some(tool) = registry.get(&call.name) else {
            return ToolResult::Failed {
                message: format!("no tool named '{}'", call.name),
            };
        };
        let context = ToolContext {
            services: self.service.services(),
            thread_id: &self.thread_id,
            turn_message_id: assistant_id,
            execution_id: None,
            authored_workspace_id: None,
            scope,
        };
        match tool.call(&context, call.input()).await {
            Ok(value) => ToolResult::Output { value },
            Err(message) => ToolResult::Failed { message },
        }
    }

    /// Persist the user's message before any remote call is made: the prompt is
    /// durable before it can produce a response.
    async fn append_user(&mut self, text: &str) -> Result<(), AgentError> {
        let id = uuid::Uuid::new_v4().to_string();
        self.emit(TurnEvent::MessageStarted {
            id: id.clone(),
            role: Role::User,
        });
        self.emit(TurnEvent::TextDelta {
            text: text.to_string(),
        });
        let message = AgentChatMessage::user(id, text);
        self.append(&message).await
    }

    /// Close one assistant row: reserve its authored state, insert it, then
    /// commit. The row becomes durable only after an immutable prepared
    /// revision exists, so a crash between the two is recoverable
    /// (`authored_state_recover_turns`) rather than a lost association.
    async fn close_row(
        &mut self,
        assistant_id: &str,
        stop_reason: StopReason,
        usage: Usage,
    ) -> Result<(), AgentError> {
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

        let row = self
            .transcript
            .messages
            .iter()
            .rev()
            .find(|message| message.id == assistant_id)
            .cloned()
            .ok_or_else(|| AgentError::Invalid("assistant row vanished mid-turn".into()))?;
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
        let requested = settings
            .get("agent_model")
            .map(String::as_str)
            .unwrap_or(model::DEFAULT_MODEL);
        let id = ModelId::parse(requested)
            .or_else(|| ModelId::parse(model::DEFAULT_MODEL))
            .ok_or_else(|| AgentError::Model(model::ModelError::Unknown(requested.to_string())))?;
        let reasoning = id.spec().default_reasoning;

        if let Some(client) = &self.service.client {
            return Ok((Arc::clone(client), id, reasoning));
        }

        let preferred = match settings.get("agent_provider").map(String::as_str) {
            Some("openrouter") => Provider::OpenRouter,
            _ => Provider::Anthropic,
        };
        let (provider, _) = id.route(preferred).map_err(AgentError::Model)?;
        let key = model::api_key(provider, &self.service.services().db().0).await?;
        let client: Arc<dyn ModelClient> = match provider {
            Provider::Anthropic => Arc::new(model::anthropic::AnthropicClient::new(key)),
            Provider::OpenRouter => Arc::new(model::openrouter::OpenRouterClient::new(key)),
        };
        Ok((client, id, reasoning))
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
