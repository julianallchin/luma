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
    let mut body = json!({
        "model": wire_id,
        "max_tokens": request.max_tokens,
        "stream": true,
        "system": request.system,
        "messages": request.messages.iter().map(message).collect::<Vec<_>>(),
    });
    let object = body.as_object_mut().expect("object literal");
    if !request.tools.is_empty() {
        object.insert(
            "tools".into(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.schema,
                        })
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
}

impl MessagesParser {
    fn new(provider: Provider) -> Self {
        Self {
            open_tools: Vec::new(),
            usage: Usage::default(),
            stop_reason: StopReason::default(),
            provider: provider.as_str(),
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
            "message_stop" => vec![ModelEvent::StepEnded {
                stop_reason: self.stop_reason,
                usage: self.usage,
            }],
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
    use crate::agent::model::{ModelId, ToolSpec};

    fn parse(parser: &mut MessagesParser, data: &str) -> Vec<ModelEvent> {
        parser.event(data).expect("frame")
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
            system: "sys".into(),
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
        };
        let body = body("claude-opus-5", &request);
        assert_eq!(body["tools"][0]["name"], "python");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
    }
}
