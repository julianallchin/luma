//! The `anthropic-messages` transport, and the two services that speak it.
//!
//! The Vercel AI Gateway exposes the same `/v1/messages` wire protocol as the
//! first-party API — same request body, same SSE frames — over a different
//! host, a bearer token, and `creator/model` ids. That makes it a
//! *configuration* of this transport, not a second one; the fields that differ
//! all hang off [`super::Provider`].

use futures_util::stream::BoxStream;
use serde_json::{json, Map, Value};

use super::sse::{stream_sse, SseParser};
use super::{
    ContentBlock, ModelClient, ModelError, ModelEvent, ModelMessage, ModelRequest, ModelRole,
    Provider, ReasoningLevel, StopReason, Usage,
};

const API_VERSION: &str = "2023-06-01";

/// A `/v1/messages` client, aimed at whichever service issued its key.
pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    /// Selects the id column to route through, the auth header, and the label
    /// on any error — the three things the two services disagree about.
    provider: Provider,
}

impl AnthropicClient {
    /// The Vercel AI Gateway, which is how Luma reaches Anthropic models by
    /// default: one key, and the gateway's own routing behind it.
    #[must_use]
    pub fn gateway(api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: "https://ai-gateway.vercel.sh".into(),
            provider: Provider::VercelAiGateway,
        }
    }

    /// Direct first-party API access, for a user who has an Anthropic key and
    /// has explicitly asked for it (`agent_provider = "anthropic"`).
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: "https://api.anthropic.com".into(),
            provider: Provider::Anthropic,
        }
    }

    /// Point the client at another host — a local recorder or a test double.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

impl ModelClient for AnthropicClient {
    fn stream(&self, request: ModelRequest) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let provider = self.provider;
        let wire_id = match request.model.wire_id(provider) {
            Ok(id) => id,
            Err(error) => return once_err(error),
        };
        let http = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body(wire_id, &request));
        // The gateway rejects `x-api-key` cross-origin and authenticates the
        // bearer form; the first-party API takes only `x-api-key`.
        let http = match provider {
            Provider::Anthropic => http.header("x-api-key", &self.api_key),
            _ => http.bearer_auth(&self.api_key),
        };
        stream_sse(provider.as_str(), http, MessagesParser::new(provider))
    }
}

fn once_err(error: ModelError) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
    Box::pin(futures_util::stream::once(async move { Err(error) }))
}

fn body(wire_id: &str, request: &ModelRequest) -> Value {
    // Recomputed from scratch every request, never rotated: three markers on a
    // ceiling of four. Render order is tools → system → messages, so the system
    // marker covers the tool definitions too and the message marker advances
    // with the conversation. The fourth breakpoint stays unspent — the argument
    // for a second, trailing message anchor is a miss rate we have not measured
    // yet (`Transcript::missed_cache_tokens` is the instrument).
    let control = request.cache_retention.control(request.model);
    let mut body = json!({
        "model": wire_id,
        "max_tokens": request.max_tokens,
        "stream": true,
        "system": request
            .system
            .iter()
            .map(|text| stamp(json!({ "type": "text", "text": text }), control.as_ref()))
            .collect::<Vec<_>>(),
        "messages": messages(request, control.as_ref()),
    });
    let object = body.as_object_mut().expect("object literal");
    if !request.tools.is_empty() {
        let last = request.tools.len() - 1;
        object.insert(
            "tools".into(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .enumerate()
                    .map(|(index, tool)| {
                        let tool = json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.schema,
                        });
                        // Only the last: a marker there caches every definition
                        // before it, and stays put when the list grows.
                        stamp(tool, control.as_ref().filter(|_| index == last))
                    })
                    .collect(),
            ),
        );
    }
    // Thinking is configured, not budgeted: the current models take an effort
    // level and decide depth themselves, and a token budget is rejected.
    if request.reasoning == ReasoningLevel::Off {
        object.insert("thinking".into(), json!({ "type": "disabled" }));
    } else {
        object.insert(
            "thinking".into(),
            json!({ "type": "adaptive", "display": "summarized" }),
        );
        object.insert(
            "output_config".into(),
            json!({ "effort": request.reasoning.as_str() }),
        );
    }
    body
}

/// Add `cache_control` to an already-lowered block, when there is one to add.
fn stamp(mut block: Value, control: Option<&Value>) -> Value {
    if let Some(control) = control {
        block
            .as_object_mut()
            .expect("a lowered block is an object")
            .insert("cache_control".into(), control.clone());
    }
    block
}

/// The conversation, with the tail marker on the last content block of the last
/// message — and only when that message is a user turn.
///
/// An assistant turn gets nothing: the provider collapses tool results into a
/// synthetic user message, so during a tool loop the tail *is* the newest
/// `tool_result`, and a trailing assistant row means the step reads only as far
/// as system+tools. That uncovered case is measured rather than anchored.
fn messages(request: &ModelRequest, control: Option<&Value>) -> Vec<Value> {
    let mut out: Vec<Value> = request.messages.iter().map(message).collect();
    let Some(control) = control else { return out };
    let Some(last) = request.messages.last() else {
        return out;
    };
    if last.role != ModelRole::User || !cacheable_tail(last) {
        return out;
    }
    if let Some(tail) = out
        .last_mut()
        .and_then(|message| message.get_mut("content"))
        .and_then(Value::as_array_mut)
        .and_then(|content| content.last_mut())
    {
        *tail = stamp(tail.take(), Some(control));
    }
    out
}

/// Whether a message ends in a block a breakpoint may attach to. A `tool_use`
/// cannot carry one, and a message with no content has no block at all.
fn cacheable_tail(message: &ModelMessage) -> bool {
    matches!(
        message.content.last(),
        Some(ContentBlock::Text(_) | ContentBlock::ToolResult { .. } | ContentBlock::Image { .. })
    )
}

fn message(message: &ModelMessage) -> Value {
    json!({
        "role": match message.role {
            ModelRole::User => "user",
            ModelRole::Assistant => "assistant",
        },
        "content": message.content.iter().map(block).collect::<Vec<_>>(),
    })
}

fn block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text(text) => json!({ "type": "text", "text": text }),
        ContentBlock::ToolUse { id, name, input } => {
            json!({ "type": "tool_use", "id": id, "name": name, "input": input })
        }
        ContentBlock::ToolResult {
            id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": id,
            "is_error": is_error,
            "content": content.iter().map(self::block).collect::<Vec<_>>(),
        }),
        ContentBlock::Image { media_type, data } => json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        }),
    }
}

/// Folds `message_start` / `content_block_*` / `message_delta` frames into the
/// provider-independent delta vocabulary.
struct MessagesParser {
    /// Content-block index → tool call id, for the `input_json_delta` frames
    /// that carry only the index.
    open_tools: Vec<(u64, String)>,
    usage: Usage,
    stop_reason: StopReason,
    /// Names the service in any error — a gateway fault must not be reported
    /// as an Anthropic one.
    provider: &'static str,
    ended: bool,
}

impl MessagesParser {
    fn new(provider: Provider) -> Self {
        Self {
            open_tools: Vec::new(),
            usage: Usage::default(),
            stop_reason: StopReason::default(),
            provider: provider.as_str(),
            ended: false,
        }
    }

    fn tool_id(&self, index: u64) -> Option<&str> {
        self.open_tools
            .iter()
            .find(|(at, _)| *at == index)
            .map(|(_, id)| id.as_str())
    }
}

impl SseParser for MessagesParser {
    fn event(&mut self, data: &str) -> Result<Vec<ModelEvent>, ModelError> {
        let frame: Value = serde_json::from_str(data).map_err(|error| ModelError::Protocol {
            provider: self.provider,
            detail: error.to_string(),
        })?;
        let kind = frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let index = frame.get("index").and_then(Value::as_u64).unwrap_or(0);

        Ok(match kind {
            "message_start" => {
                if let Some(usage) = frame.pointer("/message/usage").and_then(Value::as_object) {
                    merge_usage(&mut self.usage, usage);
                }
                Vec::new()
            }
            "content_block_start" => {
                let start = frame.get("content_block").unwrap_or(&Value::Null);
                match start.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let id = start
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let name = start
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.open_tools.push((index, id.clone()));
                        vec![ModelEvent::ToolCallStarted { id, name }]
                    }
                    _ => Vec::new(),
                }
            }
            "content_block_delta" => {
                let delta = frame.get("delta").unwrap_or(&Value::Null);
                let text = |key: &str| {
                    delta
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => vec![ModelEvent::TextDelta(text("text"))],
                    Some("thinking_delta") => vec![ModelEvent::ReasoningDelta(text("thinking"))],
                    Some("input_json_delta") => match self.tool_id(index) {
                        Some(id) => vec![ModelEvent::ToolCallArgsDelta {
                            id: id.to_string(),
                            json: text("partial_json"),
                        }],
                        None => Vec::new(),
                    },
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => match self.tool_id(index) {
                Some(id) => {
                    let ended = ModelEvent::ToolCallEnded { id: id.to_string() };
                    self.open_tools.retain(|(at, _)| *at != index);
                    vec![ended]
                }
                None => Vec::new(),
            },
            "message_delta" => {
                if let Some(usage) = frame.get("usage").and_then(Value::as_object) {
                    merge_usage(&mut self.usage, usage);
                }
                if let Some(reason) = frame.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = stop_reason(reason);
                }
                Vec::new()
            }
            "message_stop" => {
                self.ended = true;
                vec![ModelEvent::StepEnded {
                    stop_reason: self.stop_reason,
                    usage: self.usage,
                }]
            }
            "error" => {
                return Err(ModelError::Status {
                    provider: self.provider,
                    status: 200,
                    body: frame.get("error").unwrap_or(&frame).to_string(),
                })
            }
            _ => Vec::new(),
        })
    }

    /// A body that ends without `message_stop` still spent tokens: the counts
    /// from the frames that did arrive close the step, rather than the caller
    /// inventing a zeroed one.
    fn finish(&mut self) -> Vec<ModelEvent> {
        if self.ended {
            return Vec::new();
        }
        self.ended = true;
        vec![ModelEvent::StepEnded {
            stop_reason: self.stop_reason,
            usage: self.usage,
        }]
    }
}

fn stop_reason(value: &str) -> StopReason {
    match value {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

fn merge_usage(usage: &mut Usage, frame: &Map<String, Value>) {
    let field = |key: &str| frame.get(key).and_then(Value::as_u64);
    if let Some(value) = field("input_tokens") {
        usage.input_tokens = value;
    }
    if let Some(value) = field("output_tokens") {
        usage.output_tokens = value;
    }
    if let Some(value) = field("cache_read_input_tokens") {
        usage.cache_read_input_tokens = value;
    }
    if let Some(value) = field("cache_creation_input_tokens") {
        usage.cache_creation_input_tokens = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model::{CacheRetention, ModelId, ToolSpec};

    fn parse(parser: &mut MessagesParser, data: &str) -> Vec<ModelEvent> {
        parser.event(data).expect("frame")
    }

    /// Anthropic splits one prompt across three fields and reports each in a
    /// different frame — `input_tokens` and the cache pair at `message_start`,
    /// `output_tokens` only at `message_delta`. They must accumulate onto one
    /// [`Usage`] rather than the later frame clearing the earlier one.
    #[test]
    fn a_cached_usage_accumulates_across_the_frames_that_carry_it() {
        let mut parser = MessagesParser::new(Provider::Anthropic);
        assert!(parse(
            &mut parser,
            r#"{"type":"message_start","message":{"usage":{
                "input_tokens":4000,"cache_read_input_tokens":176000,
                "cache_creation_input_tokens":2048}}}"#
        )
        .is_empty());
        assert!(parse(
            &mut parser,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":64}}"#
        )
        .is_empty());
        assert_eq!(
            parse(&mut parser, r#"{"type":"message_stop"}"#),
            vec![ModelEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 4_000,
                    output_tokens: 64,
                    cache_read_input_tokens: 176_000,
                    cache_creation_input_tokens: 2_048,
                },
            }]
        );
    }

    #[test]
    fn a_tool_call_arrives_as_start_args_end() {
        let mut parser = MessagesParser::new(Provider::Anthropic);
        assert!(parse(
            &mut parser,
            r#"{"type":"message_start","message":{"usage":{"input_tokens":7}}}"#
        )
        .is_empty());
        assert_eq!(
            parse(
                &mut parser,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"python"}}"#
            ),
            vec![ModelEvent::ToolCallStarted {
                id: "t1".into(),
                name: "python".into()
            }]
        );
        assert_eq!(
            parse(
                &mut parser,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#
            ),
            vec![ModelEvent::ToolCallArgsDelta {
                id: "t1".into(),
                json: "{\"a\":".into()
            }]
        );
        assert_eq!(
            parse(&mut parser, r#"{"type":"content_block_stop","index":1}"#),
            vec![ModelEvent::ToolCallEnded { id: "t1".into() }]
        );
        parse(
            &mut parser,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#,
        );
        assert_eq!(
            parse(&mut parser, r#"{"type":"message_stop"}"#),
            vec![ModelEvent::StepEnded {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                }
            }]
        );
    }

    #[test]
    fn the_request_body_carries_tools_and_an_effort_level() {
        let request = ModelRequest {
            model: ModelId::parse("claude-opus-5").expect("known model"),
            system: vec!["sys".into()],
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: vec![ContentBlock::Text("hi".into())],
            }],
            tools: vec![ToolSpec {
                name: "python".into(),
                description: "run".into(),
                schema: json!({ "type": "object" }),
            }],
            reasoning: ReasoningLevel::High,
            max_tokens: 1024,
            cache_retention: CacheRetention::Short,
        };
        let body = body("claude-opus-5", &request);
        assert_eq!(body["tools"][0]["name"], "python");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
    }

    /// Count every `cache_control` anywhere in a body — the ceiling is per
    /// *request*, so a test that counted only the sites it expected would miss
    /// a fourth someone added elsewhere.
    fn markers(body: &Value) -> usize {
        match body {
            Value::Object(fields) => fields
                .iter()
                .map(|(key, value)| usize::from(key == "cache_control") + markers(value))
                .sum(),
            Value::Array(items) => items.iter().map(markers).sum(),
            _ => 0,
        }
    }

    fn request(messages: Vec<ModelMessage>, retention: CacheRetention) -> ModelRequest {
        ModelRequest {
            model: ModelId::parse("claude-opus-5").expect("known model"),
            system: vec!["sys".into()],
            messages,
            tools: vec![
                ToolSpec {
                    name: "first".into(),
                    description: "a".into(),
                    schema: json!({ "type": "object" }),
                },
                ToolSpec {
                    name: "python".into(),
                    description: "run".into(),
                    schema: json!({ "type": "object" }),
                },
            ],
            reasoning: ReasoningLevel::High,
            max_tokens: 1024,
            cache_retention: retention,
        }
    }

    fn user(block: ContentBlock) -> ModelMessage {
        ModelMessage {
            role: ModelRole::User,
            content: vec![ContentBlock::Text("earlier".into()), block],
        }
    }

    /// The three sites, and the fourth breakpoint left unspent.
    #[test]
    fn three_markers_land_on_the_system_the_last_tool_and_the_conversation_tail() {
        let body = body(
            "claude-opus-5",
            &request(
                vec![user(ContentBlock::Text("hi".into()))],
                CacheRetention::Short,
            ),
        );
        assert_eq!(markers(&body), 3, "{body:#}");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["tools"][0]["cache_control"].is_null());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
        assert!(body["messages"][0]["content"][0]["cache_control"].is_null());
        assert_eq!(
            body["messages"][0]["content"][1]["cache_control"]["type"],
            "ephemeral"
        );
    }

    /// A tool loop's tail: the newest `tool_result`, inside the synthetic user
    /// message the transcript already collapses results into.
    #[test]
    fn the_tail_marker_follows_a_tool_result() {
        let body = body(
            "claude-opus-5",
            &request(
                vec![user(ContentBlock::ToolResult {
                    id: "c1".into(),
                    content: vec![ContentBlock::Text("42".into())],
                    is_error: false,
                })],
                CacheRetention::Short,
            ),
        );
        assert_eq!(markers(&body), 3, "{body:#}");
        assert_eq!(body["messages"][0]["content"][1]["type"], "tool_result");
        assert_eq!(
            body["messages"][0]["content"][1]["cache_control"]["type"],
            "ephemeral"
        );
    }

    /// Pi's accepted trade-off, ported: a step that resumes after an assistant
    /// turn places no message marker at all rather than anchoring one somewhere
    /// the next step would have to move it from.
    #[test]
    fn an_assistant_tail_gets_no_message_marker() {
        let body = body(
            "claude-opus-5",
            &request(
                vec![ModelMessage {
                    role: ModelRole::Assistant,
                    content: vec![ContentBlock::Text("thinking".into())],
                }],
                CacheRetention::Short,
            ),
        );
        assert_eq!(markers(&body), 2, "{body:#}");
        assert!(body["messages"][0]["content"][0]["cache_control"].is_null());
    }

    #[test]
    fn retention_none_writes_no_markers_and_long_carries_a_ttl() {
        let none = body(
            "claude-opus-5",
            &request(
                vec![user(ContentBlock::Text("hi".into()))],
                CacheRetention::None,
            ),
        );
        assert_eq!(markers(&none), 0, "{none:#}");

        let long = body(
            "claude-opus-5",
            &request(
                vec![user(ContentBlock::Text("hi".into()))],
                CacheRetention::Long,
            ),
        );
        assert_eq!(markers(&long), 3);
        assert_eq!(long["system"][0]["cache_control"]["ttl"], "1h");
    }
}
