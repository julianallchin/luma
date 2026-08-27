//! The agent loop.
//!
//! # Interface
//!
//! Everything a host knows about the agent is on this page:
//!
//! 1. Build an [`AgentService`] from the shared [`AppServices`].
//! 2. [`AgentService::resolve_thread`] for the conversation a screen implies.
//! 3. [`AgentService::turn`] for a [`TurnStream`] — an ordered, typed stream of
//!    [`TurnEvent`]s. **Dropping the stream cancels the turn**, including any
//!    Python cell in flight; there is no `cancel` a caller could forget.
//! 4. [`transcript::apply`] to fold each event into a [`Transcript`]. Both
//!    hosts call that one reducer; neither writes another.
//!
//! Turn deltas deliberately do **not** go through [`crate::dispatch::Events`].
//! That bus is a string-keyed, fire-and-forget, app-wide broadcast of
//! `serde_json::Value`; turn deltas are per-turn, ordered, high-rate and typed.
//! A host that can only receive JSON (the webview) pays for its own adapter —
//! see the `agent_turn_*` commands on the dispatch seam — and the host that can
//! hold a Rust stream does not pay at all.
//!
//! # Implementation
//!
//! The loop lives here, next to the data it protects, rather than in a UI
//! crate: the turn ordering (`persist(user)` → prompt → `prepare_turn` →
//! `persist(assistant)` → `finalize_turn`) is a durability protocol enforced by
//! a database trigger, and it has to run headless — batch lighting runs and CI
//! have no window.

pub mod host;
pub mod model;
pub mod skills;
pub mod subagent;
pub mod tools;
pub mod transcript;
mod turn;

pub use transcript::{
    apply, to_model_messages, AgentChatMessage, AgentChatPart, Applied, RequestUsage, Role,
    ToolPart, ToolState, Transcript,
};

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::dispatch::AppServices;
use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, CreateAgentThreadInput,
};
use model::{ModelClient, ModelError, StopReason, Usage};

/// Which agent a thread belongs to. The durable column stores the snake-case
/// name; a thread is never re-pointed at another kind.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    TrackCopilot,
    PatternGraph,
}

impl AgentKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::TrackCopilot => "track_copilot",
            AgentKind::PatternGraph => "pattern_graph",
        }
    }

    /// # Errors
    ///
    /// [`AgentError::Invalid`] for a kind this build does not implement.
    pub fn parse(value: &str) -> Result<Self, AgentError> {
        match value {
            "track_copilot" => Ok(AgentKind::TrackCopilot),
            "pattern_graph" => Ok(AgentKind::PatternGraph),
            other => Err(AgentError::Invalid(format!(
                "unsupported agent kind '{other}'"
            ))),
        }
    }

    /// The system prompt: the kind's prose plus the `<available_skills>` block
    /// the `skill` tool loads from.
    ///
    /// Byte-stable so it stays a cacheable prefix — the listing is name-sorted
    /// and the whole string is composed once per process, so a prefix cannot
    /// move under a running thread.
    #[must_use]
    pub fn system_prompt(self) -> &'static str {
        static TRACK: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        static GRAPH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let (cell, prose) = match self {
            AgentKind::TrackCopilot => (&TRACK, include_str!("prompts/track.md")),
            AgentKind::PatternGraph => (&GRAPH, include_str!("prompts/graph.md")),
        };
        cell.get_or_init(|| match skills::bundled().listing() {
            "" => prose.to_string(),
            listing => format!("{prose}\n\n{listing}"),
        })
    }
}

/// What a conversation is *about*.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Track,
    Pattern,
}

impl SubjectKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SubjectKind::Track => "track",
            SubjectKind::Pattern => "pattern",
        }
    }
}

/// The identity of a conversation, as a screen implies it.
///
/// The rule that `pattern_graph` requires an implementation and `track_copilot`
/// forbids one is stated once, in the durable model's `authored_route`; this
/// type carries the fields and lets that check own the invariant.
/// One row of the history picker: a conversation, read by its own words.
///
/// Threads are almost never titled, so a row is named by what was said in it —
/// the first thing the reader asked and the last thing the agent answered,
/// each flattened to one line. Both come from the same read that fills the
/// list; a picker that pulled transcripts per row to name them would be N+1
/// against a list that exists to be typed at.
#[derive(Clone, Debug)]
pub struct ThreadEntry {
    pub thread: AgentThread,
    /// The reader's first message, as one line. `None` for a conversation
    /// nobody has spoken in yet.
    pub opening: Option<String>,
    /// The agent's most recent message, as one line.
    pub latest: Option<String>,
}

impl ThreadEntry {
    /// The row's first line: what was asked, else what the thread was named,
    /// else a stable placeholder so an empty conversation still has a name.
    #[must_use]
    pub fn headline(&self) -> String {
        self.opening
            .clone()
            .or_else(|| {
                self.thread
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| "New chat".to_string())
    }
}

/// One grep hit: a line of one conversation, and where in it the query fell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryHit {
    /// Index into [`History::entries`].
    pub entry: usize,
    /// The matched line, windowed around the match when it is long.
    pub excerpt: String,
    /// Byte range of the match within `excerpt`.
    pub span: std::ops::Range<usize>,
}

/// Everything the history picker shows about one subject: its conversations,
/// newest first, and every line said in them.
///
/// The transcripts are held here rather than handed to the picker because the
/// picker needs two views of them — a one-line summary per row, and a grep —
/// and neither wants the parts JSON. One read fills both; the search is a pure
/// function over what was read, so typing never waits on the database.
#[derive(Clone, Debug, Default)]
pub struct History {
    entries: Vec<ThreadEntry>,
    /// Every spoken line of every entry, in `(entry, seq)` order.
    lines: Vec<(usize, String)>,
}

impl History {
    /// How many hits one conversation may contribute to a search. A thread
    /// that says the word on every turn would otherwise crowd every other
    /// thread off the list; six is enough to show it is *that* thread.
    pub const HITS_PER_ENTRY: usize = 6;

    /// The longest a summary or excerpt gets, in characters. Past this the row
    /// is truncated visually anyway, and a picker should not carry essays.
    const LINE_CHARS: usize = 200;

    fn build(threads: Vec<AgentThread>, messages: Vec<AgentThreadMessage>) -> Self {
        let mut entries: Vec<ThreadEntry> = threads
            .into_iter()
            .map(|thread| ThreadEntry {
                thread,
                opening: None,
                latest: None,
            })
            .collect();
        let index: std::collections::HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(at, entry)| (entry.thread.id.clone(), at))
            .collect();
        let mut lines = Vec::new();
        for message in &messages {
            let Some(&at) = index.get(&message.thread_id) else {
                continue;
            };
            let role = match message.role.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => continue,
            };
            let Ok(parts) = AgentChatMessage::parse_parts(&message.parts) else {
                continue;
            };
            let text = AgentChatMessage {
                id: message.id.clone(),
                role,
                parts,
            }
            .text();
            let entry = &mut entries[at];
            match role {
                Role::User if entry.opening.is_none() => entry.opening = one_line(&text),
                Role::Assistant => {
                    if let Some(latest) = one_line(&text) {
                        entry.latest = Some(latest);
                    }
                }
                Role::User => {}
            }
            lines.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(|line| (at, line.to_string())),
            );
        }
        Self { entries, lines }
    }

    /// The conversations, newest first.
    #[must_use]
    pub fn entries(&self) -> &[ThreadEntry] {
        &self.entries
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Case-insensitive grep over every spoken line, grouped by conversation
    /// in the order of [`Self::entries`], at most [`Self::HITS_PER_ENTRY`]
    /// hits each. One hit per line: its first occurrence.
    ///
    /// Matching is per character, each folded to its first lowercase form, so
    /// the span lands on the original text rather than on a lowercased copy
    /// whose byte offsets need not line up.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<HistoryHit> {
        let needle: Vec<char> = query.trim().chars().map(fold).collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        let mut counts = vec![0usize; self.entries.len()];
        for (entry, line) in &self.lines {
            if counts[*entry] >= Self::HITS_PER_ENTRY {
                continue;
            }
            let chars: Vec<char> = line.chars().collect();
            let Some(start) = chars
                .windows(needle.len())
                .position(|window| window.iter().map(|&c| fold(c)).eq(needle.iter().copied()))
            else {
                continue;
            };
            counts[*entry] += 1;
            hits.push(excerpt(*entry, &chars, start..start + needle.len()));
        }
        hits.sort_by_key(|hit| hit.entry);
        hits
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Collapse a message to one trimmed line of at most [`History::LINE_CHARS`].
fn one_line(text: &str) -> Option<String> {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    let mut out: String = joined.chars().take(History::LINE_CHARS).collect();
    if out.chars().count() < joined.chars().count() {
        out.push('…');
    }
    Some(out)
}

/// Window `line` around `matched` (char indices) so a hit on a long line still
/// shows its match, and translate the span to byte offsets in the window.
fn excerpt(entry: usize, line: &[char], matched: std::ops::Range<usize>) -> HistoryHit {
    const LEAD: usize = 40;
    let from = if line.len() <= History::LINE_CHARS {
        0
    } else {
        matched.start.saturating_sub(LEAD)
    };
    let to = line.len().min(from + History::LINE_CHARS);
    let mut text = String::new();
    if from > 0 {
        text.push('…');
    }
    let byte_at = |index: usize| -> usize {
        text.len()
            + line[from..index]
                .iter()
                .map(|c| c.len_utf8())
                .sum::<usize>()
    };
    let start = byte_at(matched.start);
    let end = byte_at(matched.end.min(to));
    text.extend(&line[from..to]);
    if to < line.len() {
        text.push('…');
    }
    HistoryHit {
        entry,
        excerpt: text,
        span: start..end,
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadScope {
    pub agent_kind: AgentKind,
    pub subject_kind: SubjectKind,
    pub subject_id: String,
    pub implementation_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
}

impl ThreadScope {
    /// The track agent's scope for one track in one venue's score.
    #[must_use]
    pub fn track(
        subject_id: impl Into<String>,
        venue_id: impl Into<String>,
        score_id: impl Into<String>,
    ) -> Self {
        Self {
            agent_kind: AgentKind::TrackCopilot,
            subject_kind: SubjectKind::Track,
            subject_id: subject_id.into(),
            implementation_id: None,
            venue_id: Some(venue_id.into()),
            score_id: Some(score_id.into()),
        }
    }

    /// Whether `thread` is one of the conversations this scope names.
    ///
    /// Public because the native shell lists a subject's threads through
    /// `agent_thread_list`, which filters on three of the six fields — the
    /// other three are narrowed here rather than by a second copy of this rule.
    #[must_use]
    pub fn matches(&self, thread: &AgentThread) -> bool {
        thread.agent_kind == self.agent_kind.as_str()
            && thread.subject_kind.as_deref() == Some(self.subject_kind.as_str())
            && thread.subject_id.as_deref() == Some(self.subject_id.as_str())
            && thread.implementation_id == self.implementation_id
            && thread.venue_id == self.venue_id
            && thread.score_id == self.score_id
    }

    fn create_input(&self, request_id: String) -> CreateAgentThreadInput {
        CreateAgentThreadInput {
            request_id,
            agent_kind: self.agent_kind.as_str().to_string(),
            subject_kind: Some(self.subject_kind.as_str().to_string()),
            subject_id: Some(self.subject_id.clone()),
            implementation_id: self.implementation_id.clone(),
            venue_id: self.venue_id.clone(),
            score_id: self.score_id.clone(),
            title: None,
            parent_thread_id: None,
            parent_call_id: None,
        }
    }
}

/// What the user asked for.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UserPrompt {
    pub text: String,
}

impl From<String> for UserPrompt {
    fn from(text: String) -> Self {
        Self { text }
    }
}

/// A finished tool call, as the transcript stores it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ToolResult {
    /// The value the tool persisted. Not necessarily what the model sees — see
    /// [`tools::Tool::stored_output`].
    Output {
        value: Value,
    },
    Failed {
        message: String,
    },
}

/// How a turn ended.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TurnOutcome {
    Completed,
    /// The stream was dropped. Emitted only on the durable side — a dropped
    /// stream by definition has no reader left.
    Cancelled,
    Failed {
        message: String,
    },
}

/// One turn, as it happens.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
#[non_exhaustive]
pub enum TurnEvent {
    MessageStarted {
        id: String,
        role: Role,
    },
    StepStarted,
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    /// Emitted once the call's arguments are complete, not when the provider
    /// opened the block — a half-parsed argument object is of no use to a host.
    ToolCallStarted {
        call_id: String,
        name: String,
        input: Value,
    },
    ToolCallEnded {
        call_id: String,
        output: ToolResult,
    },
    /// One model step within the open assistant row finished.
    ///
    /// The three facts a context-usage readout needs about the request that
    /// just returned, carried together because they are only true together: a
    /// token count means nothing without the window it was spent against, and
    /// the window comes from the model this particular step used.
    StepEnded {
        stop_reason: StopReason,
        usage: Usage,
        /// The [`model::ModelId`] key this step ran on. A `String` because
        /// [`TurnEvent`] crosses the JSON seam to the webview.
        model: String,
        /// Wall time from request to last frame. The provider does not report
        /// it; the loop measures it.
        duration_ms: u64,
    },
    /// Live subagent state — one [`subagent::SubagentSnapshot`], as JSON
    /// because this event crosses the seam to the webview. Never persisted: a
    /// milestone is UI state, not transcript (§2.5).
    Subagent {
        snapshot: Value,
    },
    /// The authored document moved; the editor should re-read it.
    DocumentChanged {
        revision: String,
    },
    /// Ephemeral editor state the host may honour or ignore.
    PreviewSelection {
        expression: Option<String>,
    },
    /// The assistant row is closed and durable.
    MessageEnded {
        id: String,
        stop_reason: StopReason,
        usage: Usage,
    },
    TurnEnded {
        outcome: TurnOutcome,
    },
}

/// Why a turn could not start, or could not finish.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Storage(String),
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The transcript moved under us: another writer appended while this turn
    /// was running. Never rebased implicitly.
    #[error("transcript head moved; reload before continuing")]
    HeadMoved,
}

impl From<AgentError> for crate::dispatch::CommandError {
    fn from(error: AgentError) -> Self {
        match error {
            AgentError::Invalid(message) => crate::dispatch::CommandError::Invalid(message),
            other => crate::dispatch::CommandError::Internal(other.to_string()),
        }
    }
}

/// The agent loop's door.
///
/// Cheap to clone, and `'static`: a turn outlives the call that started it, so
/// it cannot borrow the caller's `&AppServices`.
#[derive(Clone)]
pub struct AgentService {
    services: Arc<AppServices>,
    /// Overrides provider selection. Set by tests and by a host that wants a
    /// scripted model; `None` resolves the model from settings and the key from
    /// the environment or the settings table.
    client: Option<Arc<dyn ModelClient>>,
    /// Overrides the tool set. `None` builds it from the thread's agent kind,
    /// which is also how a subagent gets a surface identical to its parent's.
    tools: Option<tools::ToolRegistry>,
}

impl AgentService {
    #[must_use]
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            client: None,
            tools: None,
        }
    }

    /// Drive turns with `client` instead of a configured provider.
    #[must_use]
    pub fn with_model(mut self, client: Arc<dyn ModelClient>) -> Self {
        self.client = Some(client);
        self
    }

    /// Expose `tools` instead of the agent kind's own set. The one seam a
    /// subagent (and a test) needs; the surface is still built once, so a
    /// child's tool names cannot drift from its parent's.
    #[must_use]
    pub fn with_tools(mut self, tools: tools::ToolRegistry) -> Self {
        self.tools = Some(tools);
        self
    }

    pub(crate) fn services(&self) -> &AppServices {
        &self.services
    }

    /// What the next turn's model is called, for a surface that names it.
    ///
    /// Settings-only: no key is read and no client is built, so a panel can ask
    /// on open without touching a provider.
    ///
    /// # Errors
    ///
    /// [`AgentError::Storage`] if settings cannot be read.
    pub async fn model_label(&self) -> Result<&'static str, AgentError> {
        let settings = crate::database::local::settings::get_all_settings(&self.services.db().0)
            .await
            .map_err(AgentError::Storage)?;
        Ok(model::configured(&settings)?.spec().display)
    }

    /// Everything the history picker shows about `scope`'s subject: its
    /// conversations, newest first, each named by its own words, and a grep.
    ///
    /// Scoped to the *subject* — agent, subject kind, subject id — and not to
    /// the whole [`ThreadScope`]: "what have I said about this track" does not
    /// change with the score it was hung on, and a listing that hid last
    /// week's conversation because it was had over another score would look
    /// like it lost it.
    ///
    /// # Errors
    ///
    /// [`AgentError::Storage`] if the threads or their messages cannot be read.
    pub async fn history(&self, scope: &ThreadScope) -> Result<History, AgentError> {
        let pool = &self.services.db().0;
        let principal = self.principal().await?;
        let threads = crate::database::local::agent_threads::list_threads(
            pool,
            Some(scope.agent_kind.as_str()),
            Some(scope.subject_kind.as_str()),
            Some(&scope.subject_id),
            principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)?;
        let messages = crate::database::local::agent_threads::list_subject_messages(
            pool,
            scope.agent_kind.as_str(),
            scope.subject_kind.as_str(),
            &scope.subject_id,
            principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)?;
        Ok(History::build(threads, messages))
    }

    /// One specific conversation, by id.
    ///
    /// The history picker's open path, and the reason it exists: [`Self::resolve_thread`]
    /// answers "the newest thread for this subject", so routing a picked row
    /// through it would land the reader in whichever conversation about that
    /// track happens to be newest — not the one they clicked. An id is the only
    /// unambiguous name a conversation has.
    ///
    /// # Errors
    ///
    /// [`AgentError::Storage`] if the thread does not exist or cannot be read.
    pub async fn open_thread(&self, thread_id: &str) -> Result<AgentThreadDetail, AgentError> {
        let principal = self.principal().await?;
        crate::database::local::agent_threads::get_thread(
            &self.services.db().0,
            thread_id,
            principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)
    }

    /// A brand new conversation about `scope`, always created.
    ///
    /// Distinct from [`Self::resolve_thread`] for the same reason [`Self::open_thread`]
    /// is: "start a new chat" is a statement, not a lookup, and a resolve would
    /// hand back the existing one whenever there was one — which is precisely
    /// the button not working.
    ///
    /// # Errors
    ///
    /// [`AgentError::Storage`] if the thread cannot be created.
    pub async fn new_thread(&self, scope: &ThreadScope) -> Result<AgentThreadDetail, AgentError> {
        let principal = self.principal().await?;
        let created = self
            .services
            .authored()
            .create_thread_with_authored_state(
                &self.services.db().0,
                scope.create_input(uuid::Uuid::new_v4().to_string()),
                principal.as_deref(),
            )
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?;
        Ok(AgentThreadDetail {
            thread: created,
            messages: Vec::new(),
        })
    }

    /// The newest thread matching `scope`, creating one if none exists.
    ///
    /// "Newest matching wins" is carried forward from the TypeScript stack
    /// deliberately; it is ambient, and the thread picker is where a better
    /// rule belongs.
    ///
    /// # Errors
    ///
    /// [`AgentError::Storage`] if the thread cannot be read or created.
    pub async fn resolve_thread(
        &self,
        scope: &ThreadScope,
    ) -> Result<AgentThreadDetail, AgentError> {
        let pool = &self.services.db().0;
        let principal = self.principal().await?;
        let threads = crate::database::local::agent_threads::list_threads(
            pool,
            Some(scope.agent_kind.as_str()),
            Some(scope.subject_kind.as_str()),
            Some(&scope.subject_id),
            principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)?;

        if let Some(thread) = threads.iter().find(|thread| scope.matches(thread)) {
            return crate::database::local::agent_threads::get_thread(
                pool,
                &thread.id,
                principal.as_deref(),
            )
            .await
            .map_err(AgentError::Storage);
        }

        let created = self
            .services
            .authored()
            .create_thread_with_authored_state(
                pool,
                scope.create_input(uuid::Uuid::new_v4().to_string()),
                principal.as_deref(),
            )
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?;
        Ok(AgentThreadDetail {
            thread: created,
            messages: Vec::new(),
        })
    }

    /// Every thread for `scope`'s subject, newest first.
    ///
    /// # Errors
    ///
    /// [`AgentError::Storage`] if the list cannot be read.
    pub async fn list_threads(&self, scope: &ThreadScope) -> Result<Vec<AgentThread>, AgentError> {
        let principal = self.principal().await?;
        let threads = crate::database::local::agent_threads::list_threads(
            &self.services.db().0,
            Some(scope.agent_kind.as_str()),
            Some(scope.subject_kind.as_str()),
            Some(&scope.subject_id),
            principal.as_deref(),
        )
        .await
        .map_err(AgentError::Storage)?;
        Ok(threads
            .into_iter()
            .filter(|thread| scope.matches(thread))
            .collect())
    }

    /// Run one turn. The returned stream *is* the turn: nothing happens until
    /// it is polled, and dropping it cancels everything in flight.
    #[must_use]
    pub fn turn(&self, thread_id: &str, prompt: UserPrompt) -> TurnStream {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (steer_tx, steer_rx) = mpsc::unbounded_channel();
        let run = turn::run(
            self.clone(),
            thread_id.to_string(),
            prompt,
            events_tx,
            steer_rx,
        );
        TurnStream {
            events: events_rx,
            run: Some(Box::pin(run)),
            steer: steer_tx,
        }
    }

    pub(crate) async fn principal(&self) -> Result<Option<String>, AgentError> {
        self.services
            .admitted_principal()
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))
    }
}

/// One turn's events. `Stream<Item = TurnEvent>`; dropping it cancels the turn.
///
/// The turn's work is driven by whoever polls the stream — there is no detached
/// task — so cancellation is Rust's ordinary "drop the future" and needs no
/// abort token, no registry, and no cleanup the caller could skip.
pub struct TurnStream {
    events: mpsc::UnboundedReceiver<TurnEvent>,
    run: Option<BoxFuture<'static, ()>>,
    steer: mpsc::UnboundedSender<String>,
}

impl TurnStream {
    /// A handle that can steer this turn from elsewhere — what the seam
    /// adapter keeps once it has moved the stream into a task.
    #[must_use]
    pub fn steering(&self) -> TurnSteer {
        TurnSteer(self.steer.clone())
    }

    /// Redirect the turn in flight. Applied at the next step boundary — the
    /// point at which one assistant row closes and the next opens, which is
    /// also the point at which the invariant "one prepared turn per assistant
    /// row" is maintained.
    pub fn steer(&self, message: impl Into<String>) {
        let _ = self.steer.send(message.into());
    }
}

/// Steers a turn that someone else is driving. Sending to a finished turn is
/// a no-op, not an error: the turn ending first is a race, not a mistake.
#[derive(Clone)]
pub struct TurnSteer(mpsc::UnboundedSender<String>);

impl TurnSteer {
    pub fn send(&self, message: impl Into<String>) {
        let _ = self.0.send(message.into());
    }
}

impl Stream for TurnStream {
    type Item = TurnEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<TurnEvent>> {
        let this = self.get_mut();
        loop {
            match this.events.poll_recv(cx) {
                Poll::Ready(Some(event)) => return Poll::Ready(Some(event)),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => {}
            }
            let Some(run) = this.run.as_mut() else {
                return Poll::Pending;
            };
            if run.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            // Dropping the finished future closes the event channel, so the
            // next `poll_recv` drains the tail and then ends the stream.
            this.run = None;
        }
    }
}

#[cfg(test)]
mod tests;
