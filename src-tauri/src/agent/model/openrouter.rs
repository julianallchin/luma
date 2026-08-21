//! The `openai-completions` transport, as OpenRouter serves it.
//!
//! This is where Kimi, Grok and anything else non-Anthropic is reached. The
//! WKWebView CORS workaround the TypeScript stack carried (`gateway-fetch`)
//! has no analogue here and is deliberately not ported: an HTTP client makes
//! no preflight request.

use futures_util::stream::BoxStream;
use serde_json::{json, Value};

use super::sse::{stream_sse, SseParser};
use super::{
    ContentBlock, ModelClient, ModelError, ModelEvent, ModelMessage, ModelRequest, ModelRole,
    ReasoningLevel, StopReason, Usage,
};

const PROVIDER: &str = "openrouter";

pub struct OpenRouterClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenRouterClient {
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: "https://openrouter.ai/api".into(),
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

impl ModelClient for OpenRouterClient {
    fn stream(&self, request: ModelRequest) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let wire_id = match request.model.route(super::Provider::OpenRouter) {
            Ok((_, id)) => id,
            Err(error) => {
                return Box::pin(futures_util::stream::once(async move { Err(error) }));
            }
        };
        let http = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://luma.show")
            .header("X-Title", "Luma")
            .json(&body(wire_id, &request));
        stream_sse(PROVIDER, http, CompletionsParser::default())
    }
}

fn body(wire_id: &str, request: &ModelRequest) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": request.system })];
    for message in &request.messages {
        messages.extend(lower(message));
    }
    let mut body = json!({
        "model": wire_id,
        "max_tokens": request.max_tokens,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": messages,
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
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.schema,
                            }
                        })
                    })
                    .collect(),
            ),
        );
    }
    if request.reasoning != ReasoningLevel::Off {
        object.insert(
            "reasoning".into(),
            json!({ "effort": request.reasoning.as_str() }),
        );
    }
    body
}

/// One provider-independent message becomes one *or more* completions messages:
/// tool results are their own `role: "tool"` entries there, where the
/// Anthropic shape nests them inside a user turn.
fn lower(message: &ModelMessage) -> Vec<Value> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text(value) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(value);
            }
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": input.to_string() },
            })),
            ContentBlock::ToolResult {
                id,
                content,
                is_error,
            } => {
                let mut body = content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.clone()),
                        // Completions tool messages are text-only; an image
                        // result is announced rather than silently dropped.
                        ContentBlock::Image { media_type, .. } => {
                            Some(format!("[{media_type} image omitted]"))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if *is_error {
                    body = format!("error: {body}");
                }
                out.push(json!({ "role": "tool", "tool_call_id": id, "content": body }));
            }
            ContentBlock::Image { media_type, data } => tool_calls.push(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{media_type};base64,{data}") },
            })),
        }
    }
    let role = match message.role {
        ModelRole::User => "user",
        ModelRole::Assistant => "assistant",
    };
    if !text.is_empty() || !tool_calls.is_empty() {
        let mut turn = json!({ "role": role, "content": text });
        if !tool_calls.is_empty() {
            turn.as_object_mut()
                .expect("object literal")
                .insert("tool_calls".into(), Value::Array(tool_calls));
        }
        out.insert(0, turn);
    }
    out
}

#[derive(Default)]
struct CompletionsParser {
    /// `tool_calls[].index` → id, since only the first chunk carries the id.
    open_tools: Vec<(u64, String)>,
    usage: Usage,
    stop_reason: Option<StopReason>,
    ended: bool,
}

impl SseParser for CompletionsParser {
    fn event(&mut self, data: &str) -> Result<Vec<ModelEvent>, ModelError> {
        let frame: Value = serde_json::from_str(data).map_err(|error| ModelError::Protocol {
            provider: PROVIDER,
            detail: error.to_string(),
        })?;
        if let Some(error) = frame.get("error") {
            return Err(ModelError::Status {
                provider: PROVIDER,
                status: 200,
                body: error.to_string(),
            });
        }
        let mut events = Vec::new();

        if let Some(usage) = frame.get("usage").and_then(Value::as_object) {
            let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or_default();
            self.usage.input_tokens = field("prompt_tokens");
            self.usage.output_tokens = field("completion_tokens");
        }

        let choice = frame.pointer("/choices/0").cloned().unwrap_or(Value::Null);
        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(ModelEvent::TextDelta(text.to_string()));
                }
            }
            for key in ["reasoning", "reasoning_content"] {
                if let Some(text) = delta.get(key).and_then(Value::as_str) {
                    if !text.is_empty() {
                        events.push(ModelEvent::ReasoningDelta(text.to_string()));
                    }
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        if !self.open_tools.iter().any(|(at, _)| *at == index) {
                            self.open_tools.push((index, id.to_string()));
                            events.push(ModelEvent::ToolCallStarted {
                                id: id.to_string(),
                                name: call
                                    .pointer("/function/name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            });
                        }
                    }
                    let Some((_, id)) = self.open_tools.iter().find(|(at, _)| *at == index) else {
                        continue;
                    };
                    if let Some(json) = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .filter(|json| !json.is_empty())
                    {
                        events.push(ModelEvent::ToolCallArgsDelta {
                            id: id.clone(),
                            json: json.to_string(),
                        });
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(match reason {
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal,
                _ => StopReason::EndTurn,
            });
        }

        // The completions wire has no explicit end frame — the finish reason
        // arrives, then optionally a usage-only frame. Close the step once,
        // after a frame that carried no further content.
        if let Some(stop_reason) = self.stop_reason {
            if !self.ended && events.is_empty() {
                self.ended = true;
                for (_, id) in std::mem::take(&mut self.open_tools) {
                    events.push(ModelEvent::ToolCallEnded { id });
                }
                events.push(ModelEvent::StepEnded {
                    stop_reason,
                    usage: self.usage,
                });
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_streamed_tool_call_closes_at_the_finish_reason() {
        let mut parser = CompletionsParser::default();
        let events = parser
            .event(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"python","arguments":"{\"a\""}}]}}]}"#,
            )
            .expect("frame");
        assert_eq!(
            events,
            vec![
                ModelEvent::ToolCallStarted {
                    id: "c1".into(),
                    name: "python".into()
                },
                ModelEvent::ToolCallArgsDelta {
                    id: "c1".into(),
                    json: "{\"a\"".into()
                }
            ]
        );
        assert!(parser
            .event(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
            .expect("frame")
            .contains(&ModelEvent::ToolCallEnded { id: "c1".into() }));
    }

    #[test]
    fn tool_results_lower_to_their_own_messages() {
        let lowered = lower(&ModelMessage {
            role: ModelRole::User,
            content: vec![ContentBlock::ToolResult {
                id: "c1".into(),
                content: vec![ContentBlock::Text("ok".into())],
                is_error: false,
            }],
        });
        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered[0]["role"], "tool");
        assert_eq!(lowered[0]["tool_call_id"], "c1");
    }
}
