//! The durable transcript.
//!
//! `agent_thread_messages.parts` is a **shipped JSON schema**: every thread
//! written by the TypeScript stack must round-trip through the types here
//! byte-compatibly, so the (de)serializers are hand-written against the wire
//! shape rather than derived from a shape we would have chosen today. An
//! unrecognized part is preserved verbatim ([`AgentChatPart::Unknown`]) instead
//! of being dropped — a reader from a future build must not truncate a
//! transcript it merely does not understand.
//!
//! Two directions live here and nowhere else:
//!
//! * [`apply`] folds a [`TurnEvent`] into a [`Transcript`]. Both hosts call it;
//!   neither writes its own reducer.
//! * [`to_model_messages`] rebuilds a model request from a persisted thread,
//!   splitting one assistant row back into per-step messages at
//!   [`AgentChatPart::StepStart`] and rebuilding each tool result through the
//!   tool that produced it.

use std::ops::Range;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use super::model::{ContentBlock, ModelMessage, ModelRole};
use super::tools::{ToolOutcome, ToolRegistry};
use super::{ToolResult, TurnEvent};

/// Who authored a transcript row. The durable column stores the lowercase name.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    /// The turn a non-conversational client opened — an MCP connection, a
    /// script, anything that runs cells without speaking.
    ///
    /// It is a *turn*, not speech: cells attach to it exactly as they attach
    /// to a user turn, it anchors their operation namespace, and it is never
    /// sent to a provider. Who opened it is the thread's `actor`, not this —
    /// role says what kind of turn, actor says whose.
    Session,
}

impl Role {
    /// The string the `agent_thread_messages.role` column stores.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Session => "session",
        }
    }

    /// Read the durable column back. The one place the string vocabulary is
    /// spelled out: every other reader asks here rather than keeping its own
    /// copy of the match, which is how the two used to drift.
    #[must_use]
    pub fn parse(role: &str) -> Option<Self> {
        match role {
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            "session" => Some(Role::Session),
            _ => None,
        }
    }
}

/// Lifecycle of one tool call as the chat surface renders it. Values are the
/// durable wire strings; anything unrecognized is preserved as
/// [`ToolState::Other`] rather than rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolState {
    InputStreaming,
    InputAvailable,
    OutputAvailable,
    OutputError,
    Other(String),
}

impl ToolState {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            ToolState::InputStreaming => "input-streaming",
            ToolState::InputAvailable => "input-available",
            ToolState::OutputAvailable => "output-available",
            ToolState::OutputError => "output-error",
            ToolState::Other(other) => other,
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "input-streaming" => ToolState::InputStreaming,
            "input-available" => ToolState::InputAvailable,
            "output-available" => ToolState::OutputAvailable,
            "output-error" => ToolState::OutputError,
            other => ToolState::Other(other.to_string()),
        }
    }
}

/// One tool call inside an assistant row.
///
/// The wire discriminant carries the tool name (`tool-<name>`), except for the
/// dynamic form, which puts it in a sibling `toolName` field. `dynamic` records
/// which spelling the row used so a read/write cycle reproduces it exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolPart {
    pub name: Option<String>,
    pub dynamic: bool,
    pub call_id: String,
    pub state: ToolState,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub error_text: Option<String>,
}

impl ToolPart {
    /// The tool this call addresses, or `"tool"` for a dynamic call that never
    /// named one — matching the TypeScript renderer's fallback.
    #[must_use]
    pub fn tool_name(&self) -> &str {
        self.name.as_deref().unwrap_or("tool")
    }
}

/// One part of a transcript row, in the durable column's vocabulary.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AgentChatPart {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        started_at: Option<i64>,
        last_delta_at: Option<i64>,
    },
    Tool(ToolPart),
    /// `data-pi-message` on the wire. The name outlived the provider that
    /// coined it; renaming a discriminant in a durable column to match an
    /// implementation detail buys a migration and nothing else.
    ProviderMessage {
        data: Value,
    },
    StepStart,
    /// A part shape this build does not know, preserved verbatim.
    Unknown(Value),
}

impl AgentChatPart {
    fn text_mut(&mut self) -> Option<&mut String> {
        match self {
            AgentChatPart::Text { text } | AgentChatPart::Reasoning { text, .. } => Some(text),
            _ => None,
        }
    }
}

impl Serialize for AgentChatPart {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AgentChatPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(D::Error::custom)
    }
}

impl AgentChatPart {
    /// The exact JSON object the durable column stores.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        match self {
            AgentChatPart::Text { text } => {
                map.insert("type".into(), "text".into());
                map.insert("text".into(), text.clone().into());
            }
            AgentChatPart::Reasoning {
                text,
                started_at,
                last_delta_at,
            } => {
                map.insert("type".into(), "reasoning".into());
                map.insert("text".into(), text.clone().into());
                if let Some(value) = started_at {
                    map.insert("startedAt".into(), (*value).into());
                }
                if let Some(value) = last_delta_at {
                    map.insert("lastDeltaAt".into(), (*value).into());
                }
            }
            AgentChatPart::Tool(tool) => {
                if tool.dynamic {
                    map.insert("type".into(), "dynamic-tool".into());
                    if let Some(name) = &tool.name {
                        map.insert("toolName".into(), name.clone().into());
                    }
                } else {
                    map.insert(
                        "type".into(),
                        format!("tool-{}", tool.name.clone().unwrap_or_default()).into(),
                    );
                }
                map.insert("toolCallId".into(), tool.call_id.clone().into());
                map.insert("state".into(), tool.state.as_str().into());
                if let Some(input) = &tool.input {
                    map.insert("input".into(), input.clone());
                }
                if let Some(output) = &tool.output {
                    map.insert("output".into(), output.clone());
                }
                if let Some(error) = &tool.error_text {
                    map.insert("errorText".into(), error.clone().into());
                }
            }
            AgentChatPart::ProviderMessage { data } => {
                map.insert("type".into(), "data-pi-message".into());
                map.insert("data".into(), data.clone());
            }
            AgentChatPart::StepStart => {
                map.insert("type".into(), "step-start".into());
            }
            AgentChatPart::Unknown(value) => return value.clone(),
        }
        Value::Object(map)
    }

    /// Read one durable part.
    ///
    /// # Errors
    ///
    /// If the value is not an object, or its `type` is missing or not a string.
    /// A *known* `type` with a malformed body is an error; an *unknown* `type`
    /// is preserved, not rejected.
    pub fn from_value(value: Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "agent chat part must be an object".to_string())?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "agent chat part is missing a string `type`".to_string())?
            .to_string();
        let string = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_string);
        let number = |key: &str| object.get(key).and_then(Value::as_i64);

        Ok(match kind.as_str() {
            "text" => AgentChatPart::Text {
                text: string("text").unwrap_or_default(),
            },
            "reasoning" => AgentChatPart::Reasoning {
                text: string("text").unwrap_or_default(),
                started_at: number("startedAt"),
                last_delta_at: number("lastDeltaAt"),
            },
            "data-pi-message" => AgentChatPart::ProviderMessage {
                data: object.get("data").cloned().unwrap_or(Value::Null),
            },
            "step-start" => AgentChatPart::StepStart,
            "dynamic-tool" => AgentChatPart::Tool(ToolPart {
                name: string("toolName"),
                dynamic: true,
                call_id: string("toolCallId").unwrap_or_default(),
                state: ToolState::parse(string("state").as_deref().unwrap_or("input-available")),
                input: object.get("input").cloned(),
                output: object.get("output").cloned(),
                error_text: string("errorText"),
            }),
            other if other.starts_with("tool-") => AgentChatPart::Tool(ToolPart {
                name: Some(other["tool-".len()..].to_string()),
                dynamic: false,
                call_id: string("toolCallId").unwrap_or_default(),
                state: ToolState::parse(string("state").as_deref().unwrap_or("input-available")),
                input: object.get("input").cloned(),
                output: object.get("output").cloned(),
                error_text: string("errorText"),
            }),
            _ => AgentChatPart::Unknown(value),
        })
    }
}

/// One transcript row: a durable `agent_thread_messages` record's identity,
/// role and parts, without the storage columns the loop never reads.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AgentChatMessage {
    pub id: String,
    pub role: Role,
    pub parts: Vec<AgentChatPart>,
}

impl AgentChatMessage {
    #[must_use]
    pub fn user(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: Role::User,
            parts: vec![AgentChatPart::Text { text: text.into() }],
        }
    }

    /// The `parts` column value for this row.
    #[must_use]
    pub fn parts_json(&self) -> Value {
        Value::Array(self.parts.iter().map(AgentChatPart::to_value).collect())
    }

    /// What this row *says*: its text parts, in order, joined by newlines.
    /// Reasoning, tool calls and provider metadata are not speech.
    #[must_use]
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                AgentChatPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Read a row's `parts` column.
    ///
    /// # Errors
    ///
    /// If `parts` is not an array of well-formed parts.
    pub fn parse_parts(parts: &Value) -> Result<Vec<AgentChatPart>, String> {
        parts
            .as_array()
            .ok_or_else(|| "agent thread message parts must be an array".to_string())?
            .iter()
            .cloned()
            .map(AgentChatPart::from_value)
            .collect()
    }
}

/// One conversation's rows, oldest first.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    pub messages: Vec<AgentChatMessage>,
}

impl Transcript {
    /// Rebuild from durable rows.
    ///
    /// # Errors
    ///
    /// If a row's `parts` column cannot be read, or its role is outside
    /// [`Role`]'s vocabulary.
    pub fn from_rows(
        rows: &[crate::models::agent_threads::AgentThreadMessage],
    ) -> Result<Self, String> {
        let mut messages = Vec::with_capacity(rows.len());
        for row in rows {
            let role = Role::parse(&row.role)
                .ok_or_else(|| format!("unsupported transcript role '{}'", row.role))?;
            messages.push(AgentChatMessage {
                id: row.id.clone(),
                role,
                parts: AgentChatMessage::parse_parts(&row.parts)?,
            });
        }
        Ok(Self { messages })
    }

    /// The id of the last row, which is the compare-and-swap token every
    /// durable append is made against.
    #[must_use]
    pub fn head_message_id(&self) -> Option<String> {
        self.messages.last().map(|message| message.id.clone())
    }

    /// What the most recent model request cost, or [`None`] in a conversation
    /// that has not made one.
    ///
    /// Read back out of the transcript rather than remembered beside it, which
    /// is what makes a reloaded thread and a live one answer identically: a
    /// panel that cached the last [`TurnEvent::StepEnded`] would go blank on
    /// reopen and disagree with the row sitting on screen.
    #[must_use]
    pub fn last_request(&self) -> Option<RequestUsage> {
        self.requests().next_back()
    }

    /// Every model step this thread has made, oldest first.
    pub fn requests(&self) -> impl DoubleEndedIterator<Item = RequestUsage> + '_ {
        self.messages
            .iter()
            .flat_map(|message| message.parts.iter())
            .filter_map(|part| match part {
                AgentChatPart::ProviderMessage { data } => RequestUsage::from_value(data),
                _ => None,
            })
    }

    /// Prompt tokens this thread re-read at full price that a warm cache should
    /// have covered.
    ///
    /// The answer to "is caching still working" that survives the one-time
    /// acceptance check: it is derived from usage readback alone, so it notices
    /// a prefix someone destabilised without anyone having to re-run a live
    /// test. Zero is the healthy reading. A thread against a provider that never
    /// reports caching at all reads like a total miss, which is the honest
    /// answer — those tokens really were paid for twice.
    #[must_use]
    pub fn missed_cache_tokens(&self) -> u64 {
        let mut previous: Option<RequestUsage> = None;
        let mut missed = 0;
        for request in self.requests() {
            if let Some(previous) = &previous {
                missed += request.missed_tokens(previous);
            }
            previous = Some(request);
        }
        missed
    }
}

/// Per-step misses at or below this are breakpoint granularity, not a broken
/// prefix: the tail marker sits on the *previous* step's last block, so the
/// blocks written after it are legitimately fresh.
const CACHE_MISS_NOISE_FLOOR: u64 = 1024;

/// One model request's accounting, as the durable `data-pi-message` part
/// records it.
///
/// A *step*, not a turn: the last step is the one whose prompt was the whole
/// conversation so far, so it — and not a sum over the turn — is what says how
/// close the thread is to its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestUsage {
    pub usage: super::model::Usage,
    /// The model that served it, when the row names one this build still
    /// carries. Rows written before the field existed, and rows naming a
    /// retired model, leave it [`None`] — which is also the one case with no
    /// window to measure against.
    pub model: Option<super::model::ModelId>,
    pub duration: Option<std::time::Duration>,
}

impl RequestUsage {
    fn from_value(data: &Value) -> Option<Self> {
        let object = data.as_object()?;
        let usage = serde_json::from_value(object.get("usage")?.clone()).ok()?;
        Some(Self {
            usage,
            model: object
                .get("model")
                .and_then(Value::as_str)
                .and_then(super::model::ModelId::parse),
            duration: object
                .get("durationMs")
                .and_then(Value::as_u64)
                .map(std::time::Duration::from_millis),
        })
    }

    /// Everything the provider read to answer: fresh tokens plus whatever came
    /// out of, or went into, the prompt cache.
    ///
    /// **Not `input_tokens` alone.** Anthropic reports cached prefix tokens in
    /// their own fields and excludes them from `input_tokens`, so on a warm
    /// thread — which is every thread past its first turn — `input_tokens` is
    /// the last few hundred characters the person typed. A gauge fed from it
    /// would read near-empty right up to the context limit.
    #[must_use]
    pub fn prompt_tokens(&self) -> u64 {
        self.usage.input_tokens
            + self.usage.cache_read_input_tokens
            + self.usage.cache_creation_input_tokens
    }

    /// How much of the prefix `previous` established this step paid for again.
    ///
    /// `min` of the two prompts, because only the overlap *could* have been
    /// cached — a step whose prompt shrank (a rewind, a compaction) is not owed
    /// coverage it never sent. Below a 1024-token noise floor the answer is
    /// zero rather than a small number, so the reading stays a signal.
    #[must_use]
    pub fn missed_tokens(&self, previous: &Self) -> u64 {
        let cacheable = previous.prompt_tokens().min(self.prompt_tokens());
        let missed = cacheable.saturating_sub(self.usage.cache_read_input_tokens);
        if missed > CACHE_MISS_NOISE_FLOOR {
            missed
        } else {
            0
        }
    }

    /// How full the window is, 0..1, or [`None`] when the row names no model
    /// this build knows and there is therefore no window.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        let window = f64::from(self.model?.context_window());
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        Some(((self.prompt_tokens() as f64 / window) as f32).clamp(0.0, 1.0))
    }
}

/// What one [`apply`] changed, so a host can remeasure exactly that row and
/// fade exactly the characters that arrived.
///
/// `part` is not in the design sketch and is load-bearing: a fade span is
/// meaningless without knowing which part grew, since a row holds several
/// independently-growing text parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Index of the row that changed, or `None` when the event changed nothing
    /// durable (a subagent milestone, a document notification).
    pub row: Option<usize>,
    /// Index of the part within that row that grew.
    pub part: Option<usize>,
    /// Byte range appended to that part's text.
    pub appended: Option<Range<usize>>,
}

/// Fold one turn event into the transcript. The single canonical reducer;
/// both hosts call it and neither writes another.
pub fn apply(transcript: &mut Transcript, event: &TurnEvent) -> Applied {
    match event {
        TurnEvent::MessageStarted { id, role } => {
            transcript.messages.push(AgentChatMessage {
                id: id.clone(),
                role: *role,
                parts: Vec::new(),
            });
            Applied {
                row: Some(transcript.messages.len() - 1),
                ..Applied::default()
            }
        }
        TurnEvent::StepStarted => push_part(transcript, AgentChatPart::StepStart),
        TurnEvent::TextDelta { text } => append_text(transcript, text, false),
        TurnEvent::ReasoningDelta { text } => append_text(transcript, text, true),
        TurnEvent::ToolCallStarted {
            call_id,
            name,
            input,
        } => push_part(
            transcript,
            AgentChatPart::Tool(ToolPart {
                name: Some(name.clone()),
                dynamic: false,
                call_id: call_id.clone(),
                state: ToolState::InputAvailable,
                input: Some(input.clone()),
                output: None,
                error_text: None,
            }),
        ),
        TurnEvent::ToolCallEnded { call_id, output } => {
            let Some(row) = transcript.messages.len().checked_sub(1) else {
                return Applied::default();
            };
            let message = &mut transcript.messages[row];
            let found = message
                .parts
                .iter_mut()
                .enumerate()
                .rev()
                .find_map(|(index, part)| match part {
                    AgentChatPart::Tool(tool) if tool.call_id == *call_id => Some((index, tool)),
                    _ => None,
                });
            let Some((part, tool)) = found else {
                return Applied::default();
            };
            match output {
                ToolResult::Output { value } => {
                    tool.state = ToolState::OutputAvailable;
                    tool.output = Some(value.clone());
                }
                ToolResult::Failed { message } => {
                    tool.state = ToolState::OutputError;
                    tool.error_text = Some(message.clone());
                }
            }
            Applied {
                row: Some(row),
                part: Some(part),
                appended: None,
            }
        }
        TurnEvent::StepEnded {
            stop_reason,
            usage,
            model,
            duration_ms,
        } => push_part(
            transcript,
            AgentChatPart::ProviderMessage {
                data: serde_json::json!({
                    "stopReason": stop_reason.as_str(),
                    "usage": usage,
                    "model": model,
                    "durationMs": duration_ms,
                }),
            },
        ),
        // Live-only or host-only: a subagent milestone is not transcript
        // (§2.5), and the other two are notifications to the editor.
        TurnEvent::Subagent { .. }
        | TurnEvent::DocumentChanged { .. }
        | TurnEvent::PreviewSelection { .. }
        | TurnEvent::MessageEnded { .. }
        | TurnEvent::TurnEnded { .. } => Applied::default(),
    }
}

fn push_part(transcript: &mut Transcript, part: AgentChatPart) -> Applied {
    let Some(row) = transcript.messages.len().checked_sub(1) else {
        return Applied::default();
    };
    let message = &mut transcript.messages[row];
    message.parts.push(part);
    Applied {
        row: Some(row),
        part: Some(message.parts.len() - 1),
        appended: None,
    }
}

fn append_text(transcript: &mut Transcript, delta: &str, reasoning: bool) -> Applied {
    let Some(row) = transcript.messages.len().checked_sub(1) else {
        return Applied::default();
    };
    let message = &mut transcript.messages[row];
    let extendable = match message.parts.last() {
        Some(AgentChatPart::Text { .. }) if !reasoning => true,
        Some(AgentChatPart::Reasoning { .. }) if reasoning => true,
        _ => false,
    };
    if !extendable {
        message.parts.push(if reasoning {
            AgentChatPart::Reasoning {
                text: String::new(),
                started_at: Some(now_ms()),
                last_delta_at: Some(now_ms()),
            }
        } else {
            AgentChatPart::Text {
                text: String::new(),
            }
        });
    }
    let part = message.parts.len() - 1;
    let target = &mut message.parts[part];
    if let AgentChatPart::Reasoning { last_delta_at, .. } = target {
        *last_delta_at = Some(now_ms());
    }
    let text = target.text_mut().expect("just pushed a text-bearing part");
    let start = text.len();
    text.push_str(delta);
    Applied {
        row: Some(row),
        part: Some(part),
        appended: Some(start..text.len()),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// Rehydrate a model request from the durable transcript.
///
/// One assistant row is several model messages: a row accumulates every step of
/// a turn, and the provider wants each step's tool calls answered by a matching
/// result message before the next step's content. `registry` is what turns a
/// *stored* tool output back into the *model-facing* one — the two differ
/// wherever a tool persists more (or less) than the model should re-read.
#[must_use]
pub fn to_model_messages(transcript: &Transcript, registry: &ToolRegistry) -> Vec<ModelMessage> {
    let mut out = Vec::new();
    for message in &transcript.messages {
        match message.role {
            Role::User => {
                let text = message.text();
                if !text.is_empty() {
                    out.push(ModelMessage {
                        role: ModelRole::User,
                        content: vec![ContentBlock::Text(text)],
                    });
                }
            }
            Role::Assistant => push_assistant_steps(&mut out, message, registry),
            // A session marker is provenance, not conversation: it stays in
            // the transcript (it is a real row, and the CAS head may be it)
            // and never reaches a provider.
            Role::Session => {}
        }
    }
    out
}

fn push_assistant_steps(
    out: &mut Vec<ModelMessage>,
    message: &AgentChatMessage,
    registry: &ToolRegistry,
) {
    let mut step: Vec<ContentBlock> = Vec::new();
    let mut results: Vec<ContentBlock> = Vec::new();
    let mut flush = |step: &mut Vec<ContentBlock>, results: &mut Vec<ContentBlock>| {
        if !step.is_empty() {
            out.push(ModelMessage {
                role: ModelRole::Assistant,
                content: std::mem::take(step),
            });
        }
        if !results.is_empty() {
            out.push(ModelMessage {
                role: ModelRole::User,
                content: std::mem::take(results),
            });
        }
    };

    for part in &message.parts {
        match part {
            AgentChatPart::StepStart => flush(&mut step, &mut results),
            AgentChatPart::Text { text } if !text.is_empty() => {
                step.push(ContentBlock::Text(text.clone()));
            }
            AgentChatPart::Tool(tool) => {
                step.push(ContentBlock::ToolUse {
                    id: tool.call_id.clone(),
                    name: tool.tool_name().to_string(),
                    input: tool.input.clone().unwrap_or(Value::Null),
                });
                results.push(tool_result(tool, registry));
            }
            // Reasoning and provider metadata are display state; replaying them
            // as content would put the model's own scratchpad back in its mouth.
            _ => {}
        }
    }
    flush(&mut step, &mut results);
}

fn tool_result(tool: &ToolPart, registry: &ToolRegistry) -> ContentBlock {
    let outcome = match (&tool.error_text, &tool.output) {
        (Some(error), _) => ToolOutcome::Error(error.clone()),
        (None, Some(output)) => match registry.get(tool.tool_name()) {
            Some(implementation) => implementation.stored_output(output),
            None => ToolOutcome::Text(output.to_string()),
        },
        (None, None) => ToolOutcome::Error("tool call did not complete".into()),
    };
    let (content, is_error) = match outcome {
        ToolOutcome::Text(text) => (vec![ContentBlock::Text(text)], false),
        ToolOutcome::Error(text) => (vec![ContentBlock::Text(text)], true),
        ToolOutcome::Content(blocks) => (blocks, false),
    };
    ContentBlock::ToolResult {
        id: tool.call_id.clone(),
        content,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model::{StopReason, Usage};
    use serde_json::json;

    #[test]
    fn durable_parts_round_trip_byte_compatibly() {
        let wire = json!([
            { "type": "step-start" },
            { "type": "text", "text": "hello" },
            { "type": "reasoning", "text": "hmm", "startedAt": 1, "lastDeltaAt": 2 },
            {
                "type": "tool-python",
                "toolCallId": "call_1",
                "state": "output-available",
                "input": { "code": "1+1" },
                "output": { "stdout": "2" }
            },
            {
                "type": "dynamic-tool",
                "toolName": "ask_venue",
                "toolCallId": "call_2",
                "state": "output-error",
                "errorText": "boom"
            },
            { "type": "data-pi-message", "data": { "stopReason": "end_turn" } },
            { "type": "data-subagent", "data": { "id": "sub_1" } },
            { "type": "from-a-future-build", "shape": [1, 2, 3] }
        ]);
        let parts = AgentChatMessage::parse_parts(&wire).expect("parse");
        assert_eq!(parts.len(), 8);
        let message = AgentChatMessage {
            id: "m1".into(),
            role: Role::Assistant,
            parts,
        };
        assert_eq!(message.parts_json(), wire);
    }

    #[test]
    fn the_fold_appends_deltas_into_one_text_part() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            &TurnEvent::MessageStarted {
                id: "a1".into(),
                role: Role::Assistant,
            },
        );
        apply(&mut transcript, &TurnEvent::StepStarted);
        apply(&mut transcript, &TurnEvent::TextDelta { text: "he".into() });
        let applied = apply(
            &mut transcript,
            &TurnEvent::TextDelta { text: "llo".into() },
        );
        assert_eq!(applied.row, Some(0));
        assert_eq!(applied.appended, Some(2..5));
        assert_eq!(
            transcript.messages[0].parts[1],
            AgentChatPart::Text {
                text: "hello".into()
            }
        );
    }

    #[test]
    fn tool_results_close_the_part_they_opened() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            &TurnEvent::MessageStarted {
                id: "a1".into(),
                role: Role::Assistant,
            },
        );
        apply(
            &mut transcript,
            &TurnEvent::ToolCallStarted {
                call_id: "c1".into(),
                name: "python".into(),
                input: json!({ "code": "1" }),
            },
        );
        apply(
            &mut transcript,
            &TurnEvent::ToolCallEnded {
                call_id: "c1".into(),
                output: ToolResult::Output {
                    value: json!({ "stdout": "1" }),
                },
            },
        );
        let AgentChatPart::Tool(tool) = &transcript.messages[0].parts[0] else {
            panic!("expected a tool part");
        };
        assert_eq!(tool.state, ToolState::OutputAvailable);
        assert_eq!(tool.output, Some(json!({ "stdout": "1" })));
    }

    #[test]
    fn rehydration_splits_one_assistant_row_per_step() {
        let transcript = Transcript {
            messages: vec![
                AgentChatMessage::user("u1", "go"),
                AgentChatMessage {
                    id: "a1".into(),
                    role: Role::Assistant,
                    parts: vec![
                        AgentChatPart::StepStart,
                        AgentChatPart::Tool(ToolPart {
                            name: Some("probe".into()),
                            dynamic: false,
                            call_id: "c1".into(),
                            state: ToolState::OutputAvailable,
                            input: Some(json!({})),
                            output: Some(json!("ok")),
                            error_text: None,
                        }),
                        AgentChatPart::ProviderMessage { data: Value::Null },
                        AgentChatPart::StepStart,
                        AgentChatPart::Text {
                            text: "done".into(),
                        },
                    ],
                },
            ],
        };
        let messages = to_model_messages(&transcript, &ToolRegistry::default());
        let roles: Vec<_> = messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                ModelRole::User,
                ModelRole::Assistant,
                ModelRole::User,
                ModelRole::Assistant
            ]
        );
        assert!(matches!(
            messages[2].content[0],
            ContentBlock::ToolResult { .. }
        ));
    }

    #[test]
    fn step_metadata_lands_on_the_open_row() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            &TurnEvent::MessageStarted {
                id: "a1".into(),
                role: Role::Assistant,
            },
        );
        apply(
            &mut transcript,
            &TurnEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: "claude-opus-5".into(),
                duration_ms: 1_234,
            },
        );
        assert!(matches!(
            transcript.messages[0].parts[0],
            AgentChatPart::ProviderMessage { .. }
        ));
    }

    /// The whole point of writing the step's metadata into the durable part:
    /// a reloaded thread answers "how full is the window" from the same bytes
    /// the live panel read, with no state kept beside the transcript.
    #[test]
    fn the_last_request_survives_a_round_trip_through_the_parts_column() {
        let mut transcript = Transcript::default();
        apply(
            &mut transcript,
            &TurnEvent::MessageStarted {
                id: "a1".into(),
                role: Role::Assistant,
            },
        );
        // Two steps: the reader must answer with the *last*, not the first.
        for (input, duration) in [(10_u64, 100_u64), (240_000, 7_500)] {
            apply(
                &mut transcript,
                &TurnEvent::StepEnded {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: input,
                        output_tokens: 42,
                        cache_read_input_tokens: 500_000,
                        cache_creation_input_tokens: 10_000,
                    },
                    model: "claude-opus-5".into(),
                    duration_ms: duration,
                },
            );
        }
        // Through the column and back, exactly as a reopened thread reads it.
        let column = serde_json::json!([{
            "id": "a1",
            "role": "assistant",
            "parts": transcript.messages[0].parts_json(),
        }]);
        let parts = AgentChatMessage::parse_parts(&column[0]["parts"]).expect("round trip");
        let reloaded = Transcript {
            messages: vec![AgentChatMessage {
                id: "a1".into(),
                role: Role::Assistant,
                parts,
            }],
        };

        let last = reloaded.last_request().expect("a step was recorded");
        assert_eq!(last.usage.input_tokens, 240_000);
        assert_eq!(last.duration, Some(std::time::Duration::from_millis(7_500)));
        assert_eq!(
            last.model.map(super::super::model::ModelId::key),
            Some("claude-opus-5")
        );
        // Cached tokens are context too — the reason this is not `input_tokens`.
        assert_eq!(last.prompt_tokens(), 240_000 + 500_000 + 10_000);
        assert_eq!(last.fraction(), Some(0.75));
        assert_eq!(last, transcript.last_request().expect("live"));
    }

    /// A row written before the field existed, or naming a model this build
    /// dropped, still reports its tokens — it just has no window to measure
    /// them against. The gauge asks for a fraction and gets an honest `None`
    /// rather than a made-up denominator.
    #[test]
    fn a_step_with_no_known_model_has_tokens_but_no_fraction() {
        let mut transcript = Transcript::default();
        transcript.messages.push(AgentChatMessage {
            id: "a1".into(),
            role: Role::Assistant,
            parts: vec![AgentChatPart::ProviderMessage {
                data: serde_json::json!({
                    "stopReason": "end_turn",
                    "usage": { "inputTokens": 7, "outputTokens": 3,
                               "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0 },
                }),
            }],
        });
        let last = transcript.last_request().expect("a step was recorded");
        assert_eq!(last.prompt_tokens(), 7);
        assert_eq!(last.model, None);
        assert_eq!(last.fraction(), None);
        assert_eq!(last.duration, None);
    }

    /// A thread of steps, as the miss detector reads it back.
    fn stepped(steps: &[(u64, u64)]) -> Transcript {
        Transcript {
            messages: steps
                .iter()
                .enumerate()
                .map(|(index, (fresh, cache_read))| AgentChatMessage {
                    id: format!("a{index}"),
                    role: Role::Assistant,
                    parts: vec![AgentChatPart::ProviderMessage {
                        data: serde_json::json!({
                            "usage": { "inputTokens": fresh, "outputTokens": 1,
                                       "cacheReadInputTokens": cache_read,
                                       "cacheCreationInputTokens": 0 },
                        }),
                    }],
                })
                .collect(),
        }
    }

    /// A healthy thread reads zero: each step's cache read covers the prefix
    /// the step before it established, and the handful of tokens the person
    /// typed since are under the noise floor.
    #[test]
    fn a_warm_thread_misses_nothing() {
        assert_eq!(
            stepped(&[(20_000, 0), (60, 20_000), (40, 20_060)]).missed_cache_tokens(),
            0
        );
    }

    /// A provider that reports no cache read at all — a destabilised prefix, or
    /// a marker nobody placed. Every step past the first pays for the whole
    /// overlap again, and the number says so.
    #[test]
    fn an_uncached_thread_reports_the_prefix_it_paid_for_twice() {
        assert_eq!(
            stepped(&[(20_000, 0), (20_060, 0), (20_100, 0)]).missed_cache_tokens(),
            20_000 + 20_060
        );
    }

    /// Below the floor is granularity, not breakage: the tail marker sits on
    /// the previous step's last block, so the blocks after it are legitimately
    /// fresh and must not accumulate into a scary total.
    #[test]
    fn a_sub_floor_miss_is_noise_not_a_miss() {
        let step = stepped(&[(2_000, 0), (500, 1_500)]);
        let requests: Vec<_> = step.requests().collect();
        assert_eq!(requests[1].missed_tokens(&requests[0]), 0);
        assert_eq!(step.missed_cache_tokens(), 0);
        // One token over, and it counts.
        let over = stepped(&[(2_000, 0), (1_050, 975)]);
        assert_eq!(over.missed_cache_tokens(), 1_025);
    }

    /// A session turn is a row like any other — it loads, it can be the CAS
    /// head — and it reaches no provider.
    ///
    /// The regression this pins is the harsh one: `from_rows` used to reject
    /// every unknown role, so one session row made the whole thread
    /// unloadable in both hosts.
    #[test]
    fn a_session_turn_rides_the_transcript_but_never_the_wire() {
        let row =
            |id: &str, role: &str, text: &str| crate::models::agent_threads::AgentThreadMessage {
                id: id.into(),
                thread_id: "thread".into(),
                parent_message_id: None,
                seq: 0,
                role: role.into(),
                parts: json!([{ "type": "text", "text": text }]),
                created_at: "2026-01-01T00:00:00Z".into(),
            };
        let rows = [
            row("session-1", "session", "Session opened on Aurora."),
            row("user-1", "user", "hello"),
        ];
        let transcript = Transcript::from_rows(&rows).expect("a session row loads");
        assert_eq!(transcript.messages[0].role, Role::Session);
        assert_eq!(transcript.head_message_id().as_deref(), Some("user-1"));

        let model = to_model_messages(&transcript, &ToolRegistry::default());
        assert_eq!(
            model.len(),
            1,
            "only the user turn is conversation: {model:?}"
        );
        assert_eq!(model[0].role, ModelRole::User);

        assert_eq!(Role::parse("session"), Some(Role::Session));
        assert_eq!(Role::Session.as_str(), "session");
        assert_eq!(Role::parse("mcp"), None);
    }
}
