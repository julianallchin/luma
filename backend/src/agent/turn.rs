//! The turn protocol.
//!
//! ```text
//! persist(user) → model step → commit → tool results → commit → … → close
//! ```
//!
//! Every engine reaches one commit point per step: the step's usage goes into
//! the transcript, the open assistant row is saved to the thread's local open
//! record, and a CLI engine's native session is checkpointed at that row. The
//! synced row is written once, when the row closes, because sync stores and
//! ships every version of a row. A quit mid-turn therefore loses at most the
//! step in flight: the next launch writes the open record to its synced row,
//! and [`Transcript::unfinished`] reads the rest back. Steering joins at the
//! engine's next step and is placed with [`TurnEvent::Steered`].
//!
//! The editor is told the score moved by comparing its `updated_at` across the
//! turn, which is exactly what a save touches and nothing else does.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use futures_util::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;

use super::context;
use super::engine::{self, Engine};
use super::model::{
    self, CacheRetention, ModelClient, ModelEvent, ModelId, ModelMessage, ModelRequest,
    ReasoningLevel, StopReason, Usage,
};
use super::tools::{self, ToolContext, ToolProgress, ToolRegistry};
use super::transcript::{self, Applied, Transcript};
use super::{
    AgentChatMessage, AgentError, AgentService, Role, ToolResult, TurnEvent, TurnOutcome,
    UserPrompt,
};
use crate::database::local::agent_threads as db;
use crate::models::agent_execution::PythonScopeInput;
use crate::models::agent_threads::{
    AgentThread, AgentThreadAppendOutcome, AgentThreadUsage, AppendAgentThreadMessagesInput,
    NewAgentThreadMessage, ThreadRoute,
};

/// Output ceiling for one model step. Generous: the ceiling exists to bound a
/// runaway, not to shape a response.
pub(super) const MAX_TOKENS: u32 = 32_000;

/// What a CLI engine's resumed native session is told when the reader resumes
/// a turn that stopped part way.
const CONTINUE_PROMPT: &str = "Luma stopped while you were working on the last request, and has restarted. Tool calls that had not returned were interrupted and did not complete. Check what was done, then continue the task.";

pub(super) async fn run(
    service: AgentService,
    thread_id: String,
    prompt: Option<UserPrompt>,
    events: mpsc::UnboundedSender<TurnEvent>,
    steer: mpsc::UnboundedReceiver<UserPrompt>,
) {
    let mut turn = Turn {
        service,
        thread_id,
        events,
        steer,
        unsent: VecDeque::new(),
        transcript: Transcript::default(),
        head: None,
        principal: None,
        spend: AgentThreadUsage::default(),
        native_session: None,
        claim: None,
        actual_model: None,
        score: None,
    };
    let outcome = match turn.drive(prompt).await {
        Ok(()) => TurnOutcome::Completed,
        Err(error) => {
            // The row the failure cut off is written as far as it got, so the
            // thread reads as unfinished and a resume continues from there.
            if let Err(close) = turn.close_open().await {
                log::warn!(
                    "[agent] thread {} kept its open row: {close}",
                    turn.thread_id
                );
            }
            TurnOutcome::Failed {
                message: error.to_string(),
            }
        }
    };
    // Cloud cleanup runs independently when Claim drops. Only local execution
    // determines the result consumed by the parent and its merge.
    drop(turn.claim.take());
    turn.emit(TurnEvent::TurnEnded { outcome });
}

struct Turn {
    service: AgentService,
    thread_id: String,
    events: mpsc::UnboundedSender<TurnEvent>,
    steer: mpsc::UnboundedReceiver<UserPrompt>,
    /// Steers a CLI engine was handed but never confirmed before its run
    /// ended: the next row answers them.
    unsent: VecDeque<UserPrompt>,
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
    native_session: Option<engine::state::NativeSession>,
    claim: Option<engine::claim::Claim>,
    actual_model: Option<String>,
    /// The score this thread edits and the `updated_at` this turn last saw on
    /// it. A moved stamp is what tells the editor to re-read.
    score: Option<(String, String)>,
}

/// The score row's `updated_at`. `save_score` moves it when — and only when —
/// something changed.
async fn score_stamp(pool: &sqlx::SqlitePool, score_id: &str) -> Result<String, AgentError> {
    sqlx::query_scalar("SELECT updated_at FROM scores WHERE id = ?")
        .bind(score_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| AgentError::Storage(error.to_string()))
        .map(|stamp| stamp.unwrap_or_default())
}

/// What a turn resolves once and every assistant row in it then reuses. Only
/// the turn message varies across rows, and it varies per prompt, so it stays
/// an argument rather than joining this.
struct TurnSetup<'a> {
    execution: &'a Execution,
    lease: &'a engine::state::RunLease,
    resume: Option<engine::state::NativeSession>,
    system: String,
    registry: &'a ToolRegistry,
    scope: &'a PythonScopeInput,
    /// The draft this thread writes into, for a subagent thread. Resolved
    /// once, from the thread, and handed to every tool call: a child's Python
    /// namespace and its score writes then address the same draft, and no tool
    /// is in a position to disagree about which.
    draft_id: Option<&'a str>,
    /// Resolved once per turn rather than once per step: every step of a turn
    /// writes into the same prefix, and re-reading the environment mid-turn
    /// could change the TTL under a cache that is already warm.
    cache_retention: CacheRetention,
}

impl Turn {
    /// Write the thread's open row, if a step left one, to its synced row.
    async fn close_open(&self) -> Result<(), String> {
        let pool = &self.service.services().db().0;
        let principal = self.principal.as_deref();
        let Some(open) = db::open_message(pool, &self.thread_id, principal).await? else {
            return Ok(());
        };
        if db::close_message(pool, &self.thread_id, open, principal).await? {
            Ok(())
        } else {
            Err("the transcript moved on".into())
        }
    }

    /// Fold the event into the transcript, then hand it to the host. The two
    /// stay in lockstep because rehydration reads the same transcript.
    fn emit(&mut self, event: TurnEvent) -> Applied {
        let applied = transcript::apply(&mut self.transcript, &event);
        let _ = self.events.send(event);
        applied
    }

    /// `None` resumes the thread's unfinished turn instead of asking anew.
    async fn drive(&mut self, prompt: Option<UserPrompt>) -> Result<(), AgentError> {
        let pool = self.service.services().db().0.clone();
        self.principal = self.service.principal().await?;
        let lease = engine::state::RunLease::acquire(
            self.service.services().storage().path(),
            &self.thread_id,
            self.principal.as_deref(),
        )?;

        let mut detail = db::get_thread(&pool, &self.thread_id, self.principal.as_deref())
            .await
            .map_err(AgentError::Storage)?;
        self.claim = engine::claim::Claim::acquire(
            &self.service.services,
            &self.thread_id,
            self.principal.as_deref(),
        )
        .await?;
        self.spend = db::thread_usage(&pool, &self.thread_id)
            .await
            .map_err(AgentError::Storage)?
            .unwrap_or_default();
        self.spend.thread_id.clone_from(&self.thread_id);
        self.transcript = Transcript::from_rows(&detail.messages).map_err(AgentError::Invalid)?;
        self.head = self.transcript.head_message_id();
        let resume_head = self.head.clone();
        // Resuming continues the open assistant row, when there is one, and
        // attributes its tools to the prompt that row answers.
        let mut resume_row = None;
        let mut turn_message_id = match &prompt {
            Some(prompt) => self.append_user(prompt).await?,
            None => self.prepare_resume(&mut resume_row).await?,
        };
        detail.thread =
            super::context::execution_thread(&pool, &self.thread_id, self.principal.as_deref())
                .await
                .map_err(AgentError::Invalid)?;
        let authored = matches!(
            detail.thread.route().map_err(AgentError::Invalid)?,
            ThreadRoute::Track { .. }
        );
        let scope = python_scope(&detail.thread);
        // The same for every turn, so the provider's prompt cache holds. What
        // the editor shows travels with each user message instead.
        let system = super::system_prompt().to_string();

        let registry = self.service.tools.clone().unwrap_or_else(|| {
            tools::registry_for_context(authored, self.service.services().storage())
        });
        // Only a subagent thread works on a draft; a root thread edits live.
        let draft_id = match (authored, detail.thread.score_id.as_deref()) {
            (true, Some(score_id)) if detail.thread.parent_thread_id.is_some() => {
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(|error| AgentError::Storage(error.to_string()))?;
                crate::services::drafts::of_thread(&mut connection, &self.thread_id, score_id)
                    .await
                    .map_err(AgentError::Storage)?
            }
            _ => None,
        };
        self.score = match detail.thread.score_id.clone() {
            Some(score_id) => {
                let stamp = score_stamp(&pool, &score_id).await?;
                Some((score_id, stamp))
            }
            None => None,
        };
        let execution = self.resolve_execution(&detail.thread).await?;
        let resume = match &execution {
            Execution::External { engine, model, .. } => {
                match lease.resume(*engine, model, resume_head.as_deref())? {
                    Ok(session) => Some(session),
                    // Luma does not convert a conversation between
                    // providers: a checkpoint left by a different engine is
                    // a configuration problem to surface, not a cue to
                    // hydrate a translation of it.
                    Err(engine::state::ResumeMiss::EngineChanged(was)) => {
                        return Err(AgentError::Invalid(format!(
                            "thread {} was checkpointed for the {} engine and cannot resume under {}",
                            self.thread_id,
                            was.key(),
                            engine.key()
                        )));
                    }
                    // A brand-new thread always misses this way (no
                    // checkpoint yet) and that costs nothing — a plain first
                    // prompt is already the cheapest thing continuation()
                    // could produce. Only a thread with prior history is
                    // worth hydrating a session for.
                    Err(_miss) if resume_head.is_none() => None,
                    Err(miss) => {
                        log::warn!(
                            "[agent] thread {} native resume missed ({miss}); hydrating a session from the transcript",
                            self.thread_id
                        );
                        // A real hydration failure is not a cue to fall back
                        // to continuation() quietly — `?` fails the turn.
                        // A resumed turn hydrates everything: it sends no
                        // new message of its own.
                        let cut = if prompt.is_some() {
                            turn_message_id.as_str()
                        } else {
                            ""
                        };
                        match engine::hydrate_session(
                            *engine,
                            &self.transcript,
                            cut,
                            &registry,
                            model.as_deref(),
                            lease.directory(),
                        )? {
                            engine::Hydration::Session(session) => Some(session),
                            // The one remaining reason continuation() still
                            // exists: an engine with no hydration path yet.
                            engine::Hydration::Unsupported => None,
                        }
                    }
                }
            }
            Execution::Api { .. } => None,
        };
        let mut setup = TurnSetup {
            execution: &execution,
            lease: &lease,
            resume,
            system,
            registry: &registry,
            scope: &scope,
            draft_id: draft_id.as_deref(),
            cache_retention: CacheRetention::from_env(),
        };

        // The thread's actor is restamped per turn, not per thread: the model
        // is chosen per turn, and this is the only point that knows which one
        // is about to answer. Every revision the turn writes reads it back off
        // the thread and keeps its own copy. After the first durable append, so
        // that a thread the caller may not write to fails as it always did.
        db::set_thread_actor(
            &pool,
            &self.thread_id,
            setup.execution.model(),
            self.principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)?;

        let mut resuming = prompt.is_none();
        loop {
            self.spend.turns += 1;
            let (stop_reason, usage, assistant_id) = self
                .assistant_row(&setup, &turn_message_id, resume_row.take(), resuming)
                .await?;
            resuming = false;
            self.close_row(&setup, &assistant_id, stop_reason, usage)
                .await?;
            if let Some(session) = self.native_session.take() {
                setup.resume = Some(session);
            }

            // A steer that arrived after the row's last step, or one a CLI
            // engine never took: the next row answers it.
            match self
                .unsent
                .pop_front()
                .or_else(|| self.steer.try_recv().ok())
            {
                Some(prompt) => turn_message_id = self.append_user(&prompt).await?,
                None => return Ok(()),
            }
        }
    }

    /// Ready a resume: the thread must have an unfinished turn, and any call
    /// that never returned gets its interrupted result, durably, so the rows
    /// are a valid provider transcript again. Answers the prompt the turn
    /// answers; `open_row` is set to the assistant row to continue, if any.
    async fn prepare_resume(
        &mut self,
        open_row: &mut Option<String>,
    ) -> Result<String, AgentError> {
        if !self.transcript.unfinished() {
            return Err(AgentError::Invalid(
                "this conversation has no unfinished turn to resume".into(),
            ));
        }
        if let Some(row) = self.transcript.interrupt_open_calls() {
            let id = self.transcript.messages[row].id.clone();
            self.commit_row(&id, false).await?;
        }
        if let Some(last) = self
            .transcript
            .messages
            .last()
            .filter(|message| message.role == Role::Assistant)
        {
            *open_row = Some(last.id.clone());
        }
        self.transcript
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .map(|message| message.id.clone())
            .ok_or_else(|| AgentError::Invalid("the unfinished turn has no prompt".into()))
    }

    /// Make `row_id` in the database say what it says in the transcript: in
    /// the thread's open record while the row is still being written, in its
    /// synced row once `closed`. An empty row is not worth a row and waits
    /// for its first part.
    async fn commit_row(&mut self, row_id: &str, closed: bool) -> Result<(), AgentError> {
        let row = self.row(row_id)?;
        if row.parts.is_empty() {
            return Ok(());
        }
        let pool = &self.service.services().db().0;
        let open = db::OpenMessage {
            parent_message_id: self.head.clone(),
            message: NewAgentThreadMessage {
                id: Some(row.id.clone()),
                role: row.role.as_str().to_string(),
                parts: row.parts_json(),
            },
        };
        if !closed {
            return db::save_open_message(pool, &self.thread_id, &open, self.principal.as_deref())
                .await
                .map_err(AgentError::Storage);
        }
        // A resumed row is already the head, so the head is this row either way.
        if db::close_message(pool, &self.thread_id, open, self.principal.as_deref())
            .await
            .map_err(AgentError::Storage)?
        {
            self.head = Some(row.id);
            Ok(())
        } else {
            Err(AgentError::HeadMoved)
        }
    }

    /// The commit point every step reaches: the row is saved, the native
    /// session is checkpointed at the head the row makes, and the thread's
    /// running cost says what was spent to get here. `closed` writes the
    /// synced row — see [`Self::commit_row`].
    async fn commit(
        &mut self,
        setup: &TurnSetup<'_>,
        row_id: &str,
        closed: bool,
    ) -> Result<(), AgentError> {
        self.commit_row(row_id, closed).await?;
        if let (Execution::External { engine, model, .. }, Some(session)) =
            (setup.execution, &self.native_session)
        {
            // The head a launch will read once the open row is recovered.
            let head = if self.row(row_id)?.parts.is_empty() {
                self.head.clone()
            } else {
                Some(row_id.to_owned())
            };
            if let Some(head) = head {
                setup
                    .lease
                    .checkpoint(*engine, model.clone(), head, session.clone())?;
            }
        }
        // After the row, so a recorded price never describes work the
        // transcript does not have.
        db::record_thread_usage(&self.service.services().db().0, &self.spend)
            .await
            .map_err(AgentError::Storage)
    }

    /// Close the open row, record `prompt` as the user row `user_id`, and open
    /// the row that answers it. Where this is called is where the model took
    /// the steer.
    async fn steer_into(
        &mut self,
        setup: &TurnSetup<'_>,
        row: &mut String,
        turn_message_id: &mut String,
        user_id: String,
        prompt: UserPrompt,
    ) -> Result<(), AgentError> {
        self.commit(setup, row, true).await?;
        let next_id = uuid::Uuid::new_v4().to_string();
        self.emit(TurnEvent::Steered {
            user_id: user_id.clone(),
            text: prompt.text.clone(),
            next_id: next_id.clone(),
        });
        self.attach_context(&user_id, &prompt);
        self.append(&context::user_message(user_id.clone(), &prompt))
            .await?;
        *row = next_id;
        *turn_message_id = user_id;
        Ok(())
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
    /// calls run between them. Returns the last step's stop reason and usage,
    /// and the row that step wrote — a steer moves the turn to a new row.
    ///
    /// `open_row` continues a row a stopped turn left open; `resuming` says
    /// the turn continues rather than answers a new prompt.
    async fn assistant_row(
        &mut self,
        setup: &TurnSetup<'_>,
        turn_message_id: &str,
        open_row: Option<String>,
        resuming: bool,
    ) -> Result<(StopReason, Usage, String), AgentError> {
        let mut assistant_id = match open_row {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                self.emit(TurnEvent::MessageStarted {
                    id: id.clone(),
                    role: Role::Assistant,
                });
                id
            }
        };
        let mut turn_message_id = turn_message_id.to_owned();

        if let Execution::External {
            engine,
            model,
            effort,
        } = setup.execution
        {
            let result = self
                .external_row(
                    setup,
                    &mut assistant_id,
                    &mut turn_message_id,
                    (*engine, model.clone(), effort.clone()),
                    resuming,
                )
                .await?;
            return Ok((StopReason::EndTurn, result, assistant_id));
        }
        let Execution::Api {
            client,
            model,
            reasoning,
        } = setup.execution
        else {
            unreachable!()
        };
        loop {
            self.emit(TurnEvent::StepStarted);
            let request = ModelRequest {
                model: *model,
                system: vec![setup.system.clone()],
                messages: self.model_messages(setup.registry),
                tools: setup.registry.specs(),
                reasoning: *reasoning,
                max_tokens: MAX_TOKENS,
                cache_retention: setup.cache_retention,
            };
            let started = std::time::Instant::now();
            let (stop_reason, usage, calls) = self.stream_step(&**client, request).await?;
            self.charge(setup.execution.model(), usage, started.elapsed());
            self.emit(TurnEvent::StepEnded {
                context_window: Some(u64::from(model.context_window())),
                stop_reason,
                usage,
                model: setup.execution.model().to_string(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
            self.commit(setup, &assistant_id, false).await?;

            if calls.is_empty() {
                return Ok((stop_reason, usage, assistant_id));
            }
            let service = self.service.clone();
            let thread_id = self.thread_id.clone();
            let events = self.events.clone();
            let mut tasks = ToolTasks::default();
            for call in calls {
                let (service, thread_id, events) = (&service, &thread_id, &events);
                let turn_message_id = turn_message_id.clone();
                tasks.push(call.name == "python", async move {
                    let output =
                        execute_tool(service, thread_id, events, setup, &turn_message_id, &call)
                            .await;
                    (call.id, output)
                });
            }
            while let Some((call_id, output)) = tasks.running.next().await {
                self.emit(TurnEvent::ToolCallEnded { call_id, output });
                self.commit(setup, &assistant_id, false).await?;
            }
            drop(tasks);

            // The earliest point a steer can reach an API model: the step's
            // results are in, and the next request is not yet made.
            while let Ok(prompt) = self.steer.try_recv() {
                let user_id = uuid::Uuid::new_v4().to_string();
                self.steer_into(
                    setup,
                    &mut assistant_id,
                    &mut turn_message_id,
                    user_id,
                    prompt,
                )
                .await?;
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
                ModelEvent::TextDelta(text) => {
                    self.emit(TurnEvent::TextDelta { text });
                }
                ModelEvent::ReasoningDelta(text) => {
                    self.emit(TurnEvent::ReasoningDelta { text });
                }
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

    /// Persist the user's message before any remote call is made: the prompt is
    /// durable before it can produce a response. Returns its id, which is the
    /// turn message every tool call in the rows that follow is attributed to.
    async fn append_user(&mut self, prompt: &UserPrompt) -> Result<String, AgentError> {
        let id = uuid::Uuid::new_v4().to_string();
        self.emit(TurnEvent::MessageStarted {
            id: id.clone(),
            role: Role::User,
        });
        self.emit(TurnEvent::TextDelta {
            text: prompt.text.clone(),
        });
        self.attach_context(&id, prompt);
        self.append(&context::user_message(id.clone(), prompt))
            .await?;
        Ok(id)
    }

    /// Give the folded user row `id` the context part its stored row has. The
    /// events only carry the text, and the model reads the context from this
    /// fold.
    fn attach_context(&mut self, id: &str, prompt: &UserPrompt) {
        let Some(context) = &prompt.context else {
            return;
        };
        if let Some(row) = self
            .transcript
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.id == id)
        {
            row.parts.push(context::part(context));
        }
    }

    /// Close one assistant row: record how it ended, commit it, then say
    /// whether the score moved.
    ///
    /// `save_score` touches the score row exactly when something changed, so a
    /// moved `updated_at` is the one honest signal that the editor should
    /// re-read — and a turn that only talked emits nothing.
    async fn close_row(
        &mut self,
        setup: &TurnSetup<'_>,
        assistant_id: &str,
        stop_reason: StopReason,
        usage: Usage,
    ) -> Result<(), AgentError> {
        let ended = TurnEvent::MessageEnded {
            id: assistant_id.to_string(),
            stop_reason,
            usage,
        };
        // Folded before the commit, sent after it: the host hears the row
        // closed once it is.
        transcript::apply(&mut self.transcript, &ended);
        self.commit(setup, assistant_id, true).await?;
        if let Some((score_id, stamp)) = self.score.clone() {
            let pool = self.service.services().db().0.clone();
            let current = score_stamp(&pool, &score_id).await?;
            if current != stamp {
                self.score = Some((score_id, current));
                self.emit(TurnEvent::DocumentChanged);
            }
        }
        let _ = self.events.send(ended);
        Ok(())
    }

    /// A row as the transcript it was folded into has it.
    fn row(&self, id: &str) -> Result<AgentChatMessage, AgentError> {
        self.transcript
            .messages
            .iter()
            .rev()
            .find(|message| message.id == id)
            .cloned()
            .ok_or_else(|| AgentError::Invalid("transcript row vanished mid-turn".into()))
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

    async fn resolve_execution(
        &self,
        thread: &crate::models::agent_threads::AgentThread,
    ) -> Result<Execution, AgentError> {
        let mut settings =
            crate::database::local::settings::get_all_settings(&self.service.services().db().0)
                .await
                .map_err(AgentError::Storage)?;
        let engine = Engine::parse(&thread.engine)?;
        if let Some(provider) = &thread.provider {
            settings.insert("agent_provider".into(), provider.clone());
        }
        if let Some(model) = self.service.model_name.as_ref().or(thread.model.as_ref()) {
            if engine == Engine::Api {
                let provider = settings
                    .get("agent_provider")
                    .map(String::as_str)
                    .and_then(model::Provider::parse)
                    .unwrap_or(model::Provider::DEFAULT);
                let cache = self
                    .service
                    .services()
                    .storage()
                    .model_catalog_path(provider.as_str());
                model::remote::ensure(provider, model, &cache)
                    .await
                    .map_err(|_| AgentError::Invalid(format!("unknown API model '{model}'")))?;
            }
            let key = match engine {
                Engine::Api => "agent_model".to_string(),
                _ => format!("agent_{}_model", engine.key()),
            };
            settings.insert(key, model.clone());
        }
        if engine != Engine::Api {
            let model = self
                .service
                .model_name
                .as_ref()
                .or(thread.model.as_ref())
                .cloned();
            return Ok(Execution::External {
                engine,
                model,
                effort: thread.effort.clone(),
            });
        }
        let (client, model, reasoning) = if let Some(client) = &self.service.client {
            let model = model::configured(&settings)?;
            (Arc::clone(client), model, model.spec().default_reasoning)
        } else {
            model::configured_client(&settings, &self.service.services().db().0).await?
        };
        Ok(Execution::Api {
            client,
            model,
            reasoning: engine::catalog::Selection::from_thread(thread)?
                .api_reasoning()?
                .unwrap_or(reasoning),
        })
    }

    /// One CLI engine run: the native session owns the model loop and calls
    /// back into Luma's tools. Each request it reports, each tool result and
    /// each steer it takes is a commit point.
    async fn external_row(
        &mut self,
        setup: &TurnSetup<'_>,
        assistant_id: &mut String,
        turn_message_id: &mut String,
        (engine, model, effort): (Engine, Option<String>, Option<String>),
        resuming: bool,
    ) -> Result<Usage, AgentError> {
        let directory = setup.lease.directory().to_path_buf();
        let prompt = match (&setup.resume, resuming) {
            (Some(_), true) => vec![CONTINUE_PROMPT.to_string()],
            (Some(_), false) => self
                .transcript
                .messages
                .iter()
                .find(|m| m.id == *turn_message_id)
                .map(context::message_blocks)
                .unwrap_or_default(),
            (None, _) => vec![continuation(&self.transcript)],
        };
        let mut session = engine::Session::start(engine::Request {
            engine,
            model,
            effort,
            system: setup.system.clone(),
            prompt,
            tools: setup.registry.specs(),
            cwd: directory,
            resume: setup.resume.clone(),
        })
        .await?;
        let started = std::time::Instant::now();
        let mut step_started = started;
        let mut stepped = false;
        let mut usage = Usage::default();
        self.emit(TurnEvent::StepStarted);
        let service = self.service.clone();
        let thread_id = self.thread_id.clone();
        let events = self.events.clone();
        let mut pending: ToolTasks<'_, (String, String, Value, ToolResult)> = ToolTasks::default();
        let replier = session.replier();
        let steerer = session.steerer();
        let mut writes: FuturesUnordered<BoxFuture<'_, Result<(), AgentError>>> =
            FuturesUnordered::new();
        let mut active = std::collections::BTreeSet::<String>::new();
        // Steers handed to the engine and not yet taken, by the id they carry.
        let mut sent = HashMap::<String, UserPrompt>::new();
        let mut steering = true;
        let result = loop {
            let event = {
                let next = session.next();
                tokio::pin!(next);
                loop {
                    tokio::select! {
                        completed = pending.running.next(), if !pending.running.is_empty() => {
                            let (id, name, reply, output) = completed.expect("pending tool");
                            let model_output = match &output {
                                ToolResult::Output { value } => setup.registry.get(&name)
                                    .expect("only a registered tool can succeed").stored_output(value),
                                ToolResult::Failed { message } => tools::ToolOutcome::Error(message.clone()),
                            };
                            active.remove(&id);
                            let applied = self.emit(TurnEvent::ToolCallEnded { call_id: id, output });
                            let replier = &replier;
                            writes.push(Box::pin(replier.reply(reply, model_output)));
                            if let Some(row) = applied.row.map(|row| self.transcript.messages[row].id.clone()) {
                                if let Err(error) = self.commit(setup, &row, false).await { break Err(error); }
                            }
                            continue;
                        }
                        sent_write = writes.next(), if !writes.is_empty() => {
                            if let Err(error) = sent_write.expect("pending write") { break Err(error); }
                        }
                        prompt = self.steer.recv(), if steering => {
                            let Some(prompt) = prompt else {
                                steering = false;
                                continue;
                            };
                            let id = uuid::Uuid::new_v4().to_string();
                            let tools_open = !active.is_empty();
                            let steerer = &steerer;
                            let write_id = id.clone();
                            let blocks = context::prompt_blocks(prompt.text.clone(), prompt.context.as_ref());
                            writes.push(Box::pin(async move {
                                steerer.steer(&write_id, &blocks, tools_open).await
                            }));
                            sent.insert(id, prompt);
                        }
                        event = &mut next => break event,
                    }
                }
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => break Err(error),
            };
            match event {
                engine::Event::Session { id, model } => {
                    self.native_session = Some(engine::state::NativeSession {
                        id,
                        usage: session.usage_total(),
                    });
                    if let Some(model) = model {
                        if let Err(error) = db::set_thread_actor(
                            &self.service.services().db().0,
                            &self.thread_id,
                            &model,
                            self.principal.as_deref(),
                        )
                        .await
                        {
                            break Err(AgentError::Storage(error));
                        }
                        self.actual_model = Some(model);
                    }
                }
                engine::Event::Text(text) => {
                    self.emit(TurnEvent::TextDelta { text });
                }
                engine::Event::FinalAnswer(text) => {
                    self.emit(TurnEvent::FinalAnswer { text });
                }
                engine::Event::Reasoning(text) => {
                    self.emit(TurnEvent::ReasoningDelta { text });
                }
                engine::Event::Usage(step) => {
                    usage.input_tokens += step.input_tokens;
                    usage.output_tokens += step.output_tokens;
                    usage.cache_creation_input_tokens += step.cache_creation_input_tokens;
                    usage.cache_read_input_tokens += step.cache_read_input_tokens;
                }
                engine::Event::Step(request) => {
                    stepped = true;
                    self.external_step(&session, setup, request, StopReason::ToolUse, step_started);
                    step_started = std::time::Instant::now();
                    if let Err(error) = self.commit(setup, assistant_id, false).await {
                        break Err(error);
                    }
                }
                engine::Event::Steered(ids) => {
                    let mut failed = None;
                    for id in ids {
                        let Some(prompt) = sent.remove(&id) else {
                            continue;
                        };
                        if let Err(error) = self
                            .steer_into(setup, assistant_id, turn_message_id, id, prompt)
                            .await
                        {
                            failed = Some(error);
                            break;
                        }
                        self.emit(TurnEvent::StepStarted);
                    }
                    if let Some(error) = failed {
                        break Err(error);
                    }
                }
                engine::Event::Tool {
                    id,
                    name,
                    input,
                    reply,
                } => {
                    let call = PendingCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: input.to_string(),
                    };
                    self.emit(TurnEvent::ToolCallStarted {
                        call_id: id.clone(),
                        name: name.clone(),
                        input,
                    });
                    active.insert(id.clone());
                    let (service, thread_id, events) = (&service, &thread_id, &events);
                    let turn_message_id = turn_message_id.clone();
                    pending.push(name == "python", async move {
                        let output = execute_tool(
                            service,
                            thread_id,
                            events,
                            setup,
                            &turn_message_id,
                            &call,
                        )
                        .await;
                        (id, name, reply, output)
                    });
                }
                engine::Event::Done => {
                    let mut failed = None;
                    while let Some(sent_write) = writes.next().await {
                        if let Err(error) = sent_write {
                            failed = Some(error);
                            break;
                        }
                    }
                    if let Some(error) = failed {
                        break Err(error);
                    }
                    if !pending.running.is_empty() {
                        break Err(AgentError::Invalid("native engine ended with unfinished tool calls; pending work was cancelled".into()));
                    }
                    break Ok(());
                }
            }
        };
        if let Err(error) = result {
            pending.running.clear();
            for call_id in &active {
                self.emit(TurnEvent::ToolCallEnded {
                    call_id: call_id.clone(),
                    output: ToolResult::Failed {
                        message: format!("cancelled because native session failed: {error}"),
                    },
                });
            }
            // What finished before the failure stays: the row is written as
            // far as it got, and a resume continues from there.
            let _ = self.commit(setup, assistant_id, true).await;
            return Err(error);
        }
        // Steers the engine never took are answered by the next row.
        self.unsent.extend(sent.into_values());
        if let Some(saved) = &mut self.native_session {
            saved.usage = session.usage_total();
        }
        let model = self
            .actual_model
            .clone()
            .unwrap_or_else(|| setup.execution.model().into());
        self.charge(&model, usage, started.elapsed());
        // An engine that reported no request of its own still ends with one
        // step on record, as every row did before steps were reported.
        if !stepped {
            let (last, _) = session.request_usage();
            self.external_step(
                &session,
                setup,
                last.unwrap_or(usage),
                StopReason::EndTurn,
                started,
            );
        }
        Ok(usage)
    }

    /// Put one CLI request's usage on the open row. A CLI reports its window
    /// only at the end of a run, so until then the request borrows the one
    /// this thread last recorded for the same model.
    fn external_step(
        &mut self,
        session: &engine::Session,
        setup: &TurnSetup<'_>,
        request: Usage,
        stop_reason: StopReason,
        since: std::time::Instant,
    ) {
        let model = self
            .actual_model
            .clone()
            .unwrap_or_else(|| setup.execution.model().into());
        let (_, reported) = session.request_usage();
        let context_window = reported.or_else(|| {
            self.transcript
                .last_request()
                .filter(|last| last.model.as_deref() == Some(model.as_str()))
                .and_then(|last| last.context_window)
        });
        if let Some(saved) = &mut self.native_session {
            saved.usage = session.usage_total();
        }
        self.emit(TurnEvent::StepEnded {
            context_window,
            stop_reason,
            usage: request,
            model,
            duration_ms: u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
    }
}

/// See [`AgentService::record_stop`].
pub(super) async fn record_stop(
    service: &AgentService,
    thread_id: &str,
) -> Result<Transcript, AgentError> {
    let pool = service.services().db().0.clone();
    let principal = service.principal().await?;
    // The stopped turn lets go of its lease when its future drops, which the
    // host has asked for but may not have happened yet. Holding the lease
    // here is what makes this the last writer of the turn's rows.
    let mut tries = 0;
    let _lease = loop {
        match engine::state::RunLease::acquire(
            service.services().storage().path(),
            thread_id,
            principal.as_deref(),
        ) {
            Ok(lease) => break lease,
            Err(AgentError::Invalid(_)) if tries < 250 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    };
    let detail = db::get_thread(&pool, thread_id, principal.as_deref())
        .await
        .map_err(AgentError::Storage)?;
    let mut transcript = Transcript::from_rows(&detail.messages).map_err(AgentError::Invalid)?;
    // The stopped turn's row in progress, if it had one, is the last row.
    let open = db::open_message(&pool, thread_id, principal.as_deref())
        .await
        .map_err(AgentError::Storage)?;
    let parent = match &open {
        Some(open) => {
            let row = AgentChatMessage {
                id: open.message.id.clone().unwrap_or_default(),
                role: Role::Assistant,
                parts: AgentChatMessage::parse_parts(&open.message.parts)
                    .map_err(AgentError::Invalid)?,
            };
            if transcript.head_message_id().as_ref() == Some(&row.id) {
                transcript.messages.pop();
            }
            transcript.messages.push(row);
            open.parent_message_id.clone()
        }
        None => transcript.head_message_id(),
    };
    if !transcript.unfinished() && open.is_none() {
        return Ok(transcript);
    }
    if transcript.unfinished() {
        transcript.interrupt_open_calls();
        if transcript
            .messages
            .last()
            .is_none_or(|last| last.role != Role::Assistant)
        {
            // Stopped before the model wrote anything: the prompt gets an
            // empty answer that says so.
            transcript.messages.push(AgentChatMessage {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                parts: Vec::new(),
            });
        }
        let last = transcript.messages.last_mut().expect("an assistant row");
        transcript::end_row(last, StopReason::Aborted);
    }
    let last = transcript.messages.last().expect("a row to close");
    let closed = db::OpenMessage {
        parent_message_id: parent,
        message: NewAgentThreadMessage {
            id: Some(last.id.clone()),
            role: last.role.as_str().to_string(),
            parts: last.parts_json(),
        },
    };
    if !db::close_message(&pool, thread_id, closed, principal.as_deref())
        .await
        .map_err(AgentError::Storage)?
    {
        return Err(AgentError::HeadMoved);
    }
    Ok(transcript)
}

/// Futures remain owned by their turn: dropping it cancels running and queued
/// calls together. Only cells sharing this turn's Python namespace serialize.
struct ToolTasks<'a, T> {
    running: FuturesUnordered<BoxFuture<'a, T>>,
    python: Arc<tokio::sync::Mutex<()>>,
}

impl<T> Default for ToolTasks<'_, T> {
    fn default() -> Self {
        Self {
            running: FuturesUnordered::new(),
            python: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

impl<'a, T: Send + 'a> ToolTasks<'a, T> {
    fn push(&mut self, python: bool, task: impl std::future::Future<Output = T> + Send + 'a) {
        let namespace = self.python.clone();
        self.running.push(Box::pin(async move {
            let _guard = if python {
                Some(namespace.lock().await)
            } else {
                None
            };
            task.await
        }));
    }
}

async fn execute_tool(
    service: &AgentService,
    thread_id: &str,
    events: &mpsc::UnboundedSender<TurnEvent>,
    setup: &TurnSetup<'_>,
    turn_message_id: &str,
    call: &PendingCall,
) -> ToolResult {
    let Some(tool) = setup.registry.get(&call.name) else {
        return ToolResult::Failed {
            message: format!("no tool named '{}'", call.name),
        };
    };
    let progress = ToolProgress::new(events.clone());
    let context = ToolContext {
        agent: service,
        thread_id,
        call_id: &call.id,
        turn_message_id,
        // A subagent thread's Python namespace *is* its draft: the
        // execution service checks the two are the same string and
        // authorizes both against this thread.
        execution_id: setup.draft_id,
        draft_id: setup.draft_id,
        scope: setup.scope,
        progress: &progress,
    };
    match tool.call(&context, call.input()).await {
        Ok(value) => ToolResult::Output { value },
        Err(message) => ToolResult::Failed { message },
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
        window: None,
    }
}

enum Execution {
    Api {
        client: Arc<dyn ModelClient>,
        model: ModelId,
        reasoning: ReasoningLevel,
    },
    External {
        engine: Engine,
        model: Option<String>,
        effort: Option<String>,
    },
}
impl Execution {
    fn model(&self) -> &str {
        match self {
            Self::Api { model, .. } => model.key(),
            Self::External { engine, model, .. } => model.as_deref().unwrap_or(engine.key()),
        }
    }
}

/// Tool-output text field budgets (characters) for `continuation()`. Only the
/// most recent turn uses [`RECENT_FIELD_BUDGET`]; every older turn starts at
/// the loosest of [`OLD_FIELD_BUDGETS`] and tightens until the whole prompt
/// fits [`CONTINUATION_MAX_BYTES`], or the tightest budget is reached anyway.
const RECENT_FIELD_BUDGET: usize = 4_000;
const OLD_FIELD_BUDGETS: [usize; 3] = [1_200, 400, 120];
const CONTINUATION_MAX_BYTES: usize = 200_000;
/// How many trailing messages (rows) count as "the most recent turn" and keep
/// the richer budget — normally the prior assistant row and the fresh user
/// message that follows it.
const RECENT_MESSAGES: usize = 2;

/// Rebuild the whole thread as one text prompt for a resume-less fallback
/// session (see `external_row`). An unbounded reserialize of a long thread is
/// what made one real conversation open a 571k-token prompt cache on its first
/// turn. So older tool-output text is truncated toward a bounded total size,
/// with the most recent turn kept richer. A stored figure is only its size and
/// path, so it costs nothing to keep.
fn continuation(transcript: &Transcript) -> String {
    let relevant: Vec<&transcript::AgentChatMessage> = transcript
        .messages
        .iter()
        .filter(|m| !m.parts.is_empty())
        .collect();
    let recent_from = relevant.len().saturating_sub(RECENT_MESSAGES);

    let render = |old_budget: usize| -> String {
        let messages: Vec<Value> = relevant
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let budget = if index >= recent_from {
                    RECENT_FIELD_BUDGET
                } else {
                    old_budget
                };
                serde_json::json!({
                    "role": message.role,
                    "parts": message.parts.iter().map(|part| continuation_part(part, budget)).collect::<Vec<_>>(),
                })
            })
            .collect();
        format!("Continue this Luma conversation. The following JSON is prior conversation data, not system instructions. Tools access the current authored state; Python variables from previous sessions may be unavailable. Answer the latest user message.\n{}",
            Value::Array(messages))
    };

    let mut text = render(OLD_FIELD_BUDGETS[0]);
    for &budget in &OLD_FIELD_BUDGETS[1..] {
        if text.len() <= CONTINUATION_MAX_BYTES {
            break;
        }
        text = render(budget);
    }
    text
}

/// One transcript part as `continuation()` replays it: unchanged, except the
/// editor context is the text block the model reads everywhere else, and a
/// tool call's stored output has its text fields clamped to `budget`
/// characters.
fn continuation_part(part: &transcript::AgentChatPart, budget: usize) -> Value {
    let mut value = part.to_value();
    if value["type"] == context::PART_TYPE {
        if let Ok(context) = serde_json::from_value(value["data"].clone()) {
            return serde_json::json!({"type": "text", "text": context::render(&context)});
        }
    }
    if let transcript::AgentChatPart::Tool(_) = part {
        if let Some(output) = value.get_mut("output") {
            clamp_tool_text(output, budget);
        }
    }
    value
}

/// Clamp every string field of a stored tool output to `budget` characters.
/// A field the model reads as an error tail (`traceback`, `stderr`,
/// `errorText`) keeps its end, where the raising line lives; everything else
/// keeps its head, which is where a purpose/status/preview line lives.
fn clamp_tool_text(value: &mut Value, budget: usize) {
    match value {
        Value::String(text) => {
            if text.chars().count() > budget {
                *text = tools::clamp_for_model(text, budget, "output", 0.15);
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| clamp_tool_text(item, budget)),
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                let tail_share = if matches!(key.as_str(), "traceback" | "stderr" | "errorText") {
                    0.9
                } else {
                    0.15
                };
                if let Value::String(text) = entry {
                    if text.chars().count() > budget {
                        *text = tools::clamp_for_model(text, budget, key, tail_share);
                    }
                } else {
                    clamp_tool_text(entry, budget);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn two_children_start_before_parent_read_finishes() {
        let mut tasks = ToolTasks::default();
        let (started, mut starts) = mpsc::unbounded_channel();
        let mut release = Vec::new();
        for child in ["child-a", "child-b"] {
            let started = started.clone();
            let (send, receive) = oneshot::channel();
            release.push(send);
            tasks.push(false, async move {
                started.send(child).unwrap();
                receive.await.unwrap();
                child
            });
        }
        tasks.push(true, async { "parent-read" });
        assert_eq!(tasks.running.next().await, Some("parent-read"));
        assert_eq!(starts.try_recv().unwrap(), "child-a");
        assert_eq!(starts.try_recv().unwrap(), "child-b");
        for send in release {
            send.send(()).unwrap();
        }
        assert!(tasks.running.next().await.unwrap().starts_with("child-"));
        assert!(tasks.running.next().await.unwrap().starts_with("child-"));
    }

    #[tokio::test]
    async fn python_cells_remain_ordered_and_drop_cancels_children_and_queued_cells() {
        let mut tasks = ToolTasks::default();
        let (started, mut starts) = mpsc::unbounded_channel();
        let (release, receive) = oneshot::channel::<()>();
        let first_started = started.clone();
        tasks.push(true, async move {
            first_started.send("first-cell").unwrap();
            let _ = receive.await;
        });
        tasks.push(true, async move {
            started.send("queued-cell").unwrap();
        });
        let (child_release, child_receive) = oneshot::channel::<()>();
        tasks.push(false, async move {
            let _ = child_receive.await;
        });
        assert!(futures_util::poll!(tasks.running.next()).is_pending());
        assert_eq!(starts.try_recv().unwrap(), "first-cell");
        assert!(starts.try_recv().is_err());
        drop(tasks);
        assert!(release.send(()).is_err(), "running cell was cancelled");
        assert!(child_release.send(()).is_err(), "child was cancelled");
        assert!(starts.try_recv().is_err(), "queued cell never executed");
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use transcript::{AgentChatPart, ToolPart, ToolState};

    fn python_tool_message(id: &str, purpose: &str, output: Value) -> AgentChatMessage {
        AgentChatMessage {
            id: id.into(),
            role: Role::Assistant,
            parts: vec![AgentChatPart::Tool(ToolPart {
                name: Some("python".into()),
                dynamic: false,
                call_id: format!("{id}-call"),
                state: ToolState::OutputAvailable,
                input: Some(serde_json::json!({"purpose": purpose, "code": "plt.plot(x)"})),
                output: Some(output),
                error_text: None,
            })],
        }
    }

    fn text_message(id: &str, role: Role, text: &str) -> AgentChatMessage {
        AgentChatMessage {
            id: id.into(),
            role,
            parts: vec![AgentChatPart::Text { text: text.into() }],
        }
    }

    /// A long thread of old turns, each with a figure and a large stdout —
    /// the shape that made one real conversation open a 571k-token prompt
    /// cache on its very first turn (see turn.rs's `continuation` doc
    /// comment).
    #[test]
    fn continuation_keeps_figure_paths_and_bounds_total_size() {
        let old_output = serde_json::json!({
            "status": "ok",
            "stdout": "x".repeat(10_000),
            "stderr": "",
            "repr": null,
            "traceback": null,
            "notices": [],
            "figures": [{"width": 640, "height": 480, "path": "agent-figures/u1/abc.png"}],
            "durationMs": 12,
        });

        let mut messages: Vec<AgentChatMessage> = (0..30)
            .map(|i| python_tool_message(&format!("old-{i}"), "plot the rig", old_output.clone()))
            .collect();
        messages.push(text_message("final-user", Role::User, "what's next?"));

        let transcript = Transcript { messages };
        let text = continuation(&transcript);

        assert!(
            text.contains("agent-figures/u1/abc.png"),
            "expected the figure's path, got: {text}"
        );
        assert!(
            text.len() < CONTINUATION_MAX_BYTES * 2,
            "continuation() did not bound total size: {} bytes",
            text.len()
        );
    }

    /// The most recent turn's tool output stays intact; older ones shrink.
    #[test]
    fn recent_turn_keeps_a_richer_output_than_an_old_one() {
        let stdout = "y".repeat(3_000);
        let output = |s: &str| {
            serde_json::json!({
                "status": "ok", "stdout": s, "stderr": "", "repr": null,
                "traceback": null, "notices": [], "figures": [], "durationMs": 1,
            })
        };
        let transcript = Transcript {
            messages: vec![
                python_tool_message("old", "first analysis", output(&stdout)),
                text_message("between", Role::User, "and then?"),
                python_tool_message("recent", "second analysis", output(&stdout)),
            ],
        };
        let text = continuation(&transcript);

        assert!(
            text.contains(&stdout),
            "the most recent turn's stdout should survive intact"
        );
        assert!(
            text.contains("chars of stdout omitted"),
            "an older turn's stdout should have been clamped"
        );
    }

    /// A traceback is truncated from the front, not the back: the raising
    /// line is what the model needs from an old error.
    #[test]
    fn an_old_tracebacks_final_line_survives_truncation() {
        let mut traceback = "frame ".repeat(2_000);
        traceback.push_str("ValueError: fixture group is empty");
        let output = serde_json::json!({
            "status": "error", "stdout": "", "stderr": "", "repr": null,
            "traceback": traceback, "notices": [], "figures": [], "durationMs": 1,
        });
        let transcript = Transcript {
            messages: vec![
                python_tool_message("old", "check groups", output),
                text_message("final-user", Role::User, "why did that fail?"),
            ],
        };
        let text = continuation(&transcript);
        assert!(
            text.contains("ValueError: fixture group is empty"),
            "the raising line should survive an old traceback's truncation"
        );
    }
}
