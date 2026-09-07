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
        let wire_id = match request.model.wire_id(super::Provider::OpenRouter) {
            Ok(id) => id,
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

/// The one auto-detection: OpenRouter forwards per-block `cache_control` to
/// Anthropic verbatim, and every other upstream either caches implicitly on a
/// stable prefix or rejects the field. The wire id is the whole test.
fn cache_control(wire_id: &str, request: &ModelRequest) -> Option<Value> {
    wire_id
        .starts_with("anthropic/")
        .then(|| request.cache_retention.control(request.model))
        .flatten()
}

fn body(wire_id: &str, request: &ModelRequest) -> Value {
    let control = cache_control(wire_id, request);
    let mut messages = vec![system(request, control.as_ref())];
    for message in &request.messages {
        messages.extend(lower(message));
    }
    // Same three sites as the native transport, over a shape that disagrees
    // about where the last cacheable block is: one system message, the last
    // tool, and the tail of the conversation.
    if control.is_some() && request.messages.last().is_some_and(cacheable_tail) {
        if let Some(tail) = messages.last_mut() {
            *tail = stamp_content(tail.take(), control.as_ref());
        }
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
        let last = request.tools.len() - 1;
        object.insert(
            "tools".into(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .enumerate()
                    .map(|(index, tool)| {
                        let mut spec = json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.schema,
                            }
                        });
                        if index == last {
                            if let Some(control) = &control {
                                spec.as_object_mut()
                                    .expect("object literal")
                                    .insert("cache_control".into(), control.clone());
                            }
                        }
                        spec
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

/// The system prompt as one completions message.
///
/// Plain text when nothing is being cached — that is what every upstream
/// accepts. A marker forces the content-part array form, which only providers
/// that speak the Anthropic block shape need to understand, and only those get
/// a marker in the first place.
fn system(request: &ModelRequest, control: Option<&Value>) -> Value {
    match control {
        None => json!({ "role": "system", "content": request.system.join("\n\n") }),
        Some(control) => json!({
            "role": "system",
            "content": request
                .system
                .iter()
                .map(|text| json!({ "type": "text", "text": text, "cache_control": control }))
                .collect::<Vec<_>>(),
        }),
    }
}

/// Move a completions message's text content into the part array so the tail
/// marker has a block to sit on. A message with nothing to say is left alone —
/// an empty part carries no prefix and would only spend a breakpoint.
fn stamp_content(mut message: Value, control: Option<&Value>) -> Value {
    let (Some(control), Some(object)) = (control, message.as_object_mut()) else {
        return message;
    };
    let Some(text) = object.get("content").and_then(Value::as_str) else {
        return message;
    };
    if text.is_empty() {
        return message;
    }
    let parts = json!([{ "type": "text", "text": text, "cache_control": control }]);
    object.insert("content".into(), parts);
    message
}

/// Whether a message ends in a block a breakpoint may attach to — the same rule
/// the native transport applies, so the two wires disagree only about *where*
/// that block ended up.
fn cacheable_tail(message: &ModelMessage) -> bool {
    message.role == ModelRole::User
        && matches!(
            message.content.last(),
            Some(
                ContentBlock::Text(_)
                    | ContentBlock::ToolResult { .. }
                    | ContentBlock::Image { .. }
            )
        )
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
            let detail = |path: &str| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|details| details.get(path))
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
            };
            // `prompt_tokens` counts the whole prompt — the cached prefix it
            // read *and* the prefix it wrote; [`Usage`] counts neither in
            // `input_tokens`. So **both** come off, not just the read: a step
            // that writes a cache would otherwise report its prefix twice, and
            // every reader that sums the three fields (the context gauge, the
            // miss detector) doubles it.
            //
            // The two counters are themselves disjoint — a token is read or
            // written, never both — so subtracting each is not subtracting the
            // same token twice. Saturating, because a provider whose numbers
            // disagree must not wrap a fresh-token count around to u64::MAX.
            let cached = detail("cached_tokens");
            // OpenRouter names the write counter `cache_write_tokens`; the
            // OpenAI spelling is accepted too, since which one arrives depends
            // on the upstream the router picked.
            let written = detail("cache_write_tokens").max(detail("cache_creation_tokens"));
            self.usage.input_tokens = field("prompt_tokens")
                .saturating_sub(cached)
                .saturating_sub(written);
            self.usage.cache_read_input_tokens = cached;
            self.usage.cache_creation_input_tokens = written;
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

        Ok(events)
    }

    /// The completions wire has no end frame: the finish reason arrives on the
    /// last content chunk and the token counts on a *later*, choice-less one.
    /// So the step can only be closed once the body is over — closing at the
    /// finish reason reports the usage that had not arrived yet, which is
    /// zero.
    fn finish(&mut self) -> Vec<ModelEvent> {
        if self.ended {
            return Vec::new();
        }
        self.ended = true;
        let mut events: Vec<_> = std::mem::take(&mut self.open_tools)
            .into_iter()
            .map(|(_, id)| ModelEvent::ToolCallEnded { id })
            .collect();
        events.push(ModelEvent::StepEnded {
            stop_reason: self.stop_reason.unwrap_or_default(),
            usage: self.usage,
        });
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame order OpenRouter actually streams, captured from a live
    /// `anthropic/claude-opus-5` request: the finish reason rides an *empty*
    /// content delta, and the token counts arrive one frame later. A parser
    /// that closes the step when the finish reason lands has not seen a single
    /// token count yet, and reports zeros.
    #[test]
    fn the_step_closes_after_the_trailing_usage_frame_not_at_the_finish_reason() {
        let mut parser = CompletionsParser::default();
        assert!(parser
            .event(r#"{"choices":[{"index":0,"delta":{"content":"Hi","role":"assistant"},"finish_reason":null}]}"#)
            .expect("frame")
            .contains(&ModelEvent::TextDelta("Hi".into())));
        assert!(parser
            .event(r#"{"choices":[{"index":0,"delta":{"content":"","role":"assistant"},"finish_reason":"stop","native_finish_reason":"end_turn"}]}"#)
            .expect("frame")
            .is_empty());
        assert!(parser
            .event(
                r#"{"choices":[{"index":0,"delta":{"content":"","role":"assistant"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":13,"completion_tokens":5,"total_tokens":18,
                             "prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":0}}}"#,
            )
            .expect("frame")
            .is_empty());
        assert_eq!(
            parser.finish(),
            vec![ModelEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 13,
                    output_tokens: 5,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            }]
        );
        assert!(parser.finish().is_empty(), "the step closes exactly once");
    }

    /// The OpenAI convention, converted: `prompt_tokens` counts the prefix the
    /// step read *and* the prefix it wrote, and [`Usage`] counts neither as
    /// fresh input, so the fresh count is what is left after both come off.
    /// Without the subtraction a warm thread's prompt is counted twice, and a
    /// context gauge reads double. `cache_write_tokens` is OpenRouter's name
    /// for the counter OpenAI spells `cache_creation_tokens`.
    #[test]
    fn a_cached_openai_shaped_usage_lands_in_the_anthropic_convention() {
        let mut parser = CompletionsParser::default();
        assert!(parser
            .event(
                r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":180000,"completion_tokens":64,
                             "prompt_tokens_details":{"cached_tokens":176000,
                                                      "cache_write_tokens":2048}}}"#,
            )
            .expect("frame")
            .contains(&ModelEvent::TextDelta("hi".into())));
        assert_eq!(
            parser.finish(),
            vec![ModelEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 1_952,
                    output_tokens: 64,
                    cache_read_input_tokens: 176_000,
                    cache_creation_input_tokens: 2_048,
                },
            }]
        );
    }

    /// A provider that reports no cache detail at all: every prompt token is
    /// fresh, and nothing is subtracted.
    #[test]
    fn an_uncached_usage_is_all_fresh_input() {
        let mut parser = CompletionsParser::default();
        assert!(parser
            .event(
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":900,"completion_tokens":12}}"#,
            )
            .expect("frame")
            .is_empty());
        assert_eq!(
            parser.finish(),
            vec![ModelEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 900,
                    output_tokens: 12,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            }]
        );
    }

    #[test]
    fn a_streamed_tool_call_closes_when_the_body_ends() {
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
            .is_empty());
        let closed = parser.finish();
        assert_eq!(closed[0], ModelEvent::ToolCallEnded { id: "c1".into() });
        assert!(matches!(
            closed[1],
            ModelEvent::StepEnded {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    fn markers(body: &Value) -> usize {
        match body {
            Value::Array(items) => items.iter().map(markers).sum(),
            Value::Object(fields) => fields
                .iter()
                .map(|(key, value)| usize::from(key == "cache_control") + markers(value))
                .sum(),
            _ => 0,
        }
    }

    fn request(key: &str, messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            model: super::super::ModelId::parse(key).expect("known model"),
            system: vec!["sys".into()],
            messages,
            tools: vec![
                super::super::ToolSpec {
                    name: "first".into(),
                    description: "a".into(),
                    schema: json!({ "type": "object" }),
                },
                super::super::ToolSpec {
                    name: "python".into(),
                    description: "run".into(),
                    schema: json!({ "type": "object" }),
                },
            ],
            reasoning: ReasoningLevel::Off,
            max_tokens: 1024,
            cache_retention: super::super::CacheRetention::Short,
        }
    }

    fn user(block: ContentBlock) -> Vec<ModelMessage> {
        vec![ModelMessage {
            role: ModelRole::User,
            content: vec![block],
        }]
    }

    /// The same three sites as the native transport, over the shape that
    /// disagrees about where the last cacheable block is: the system message's
    /// content parts, the last tool, and the last *lowered* message — which for
    /// a tool result is a `role: "tool"` entry, not a user turn.
    #[test]
    fn an_anthropic_wire_id_gets_three_markers() {
        let body = body(
            "anthropic/claude-opus-5",
            &request("claude-opus-5", user(ContentBlock::Text("hi".into()))),
        );
        assert_eq!(markers(&body), 3, "{body:#}");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(body["tools"][0]["cache_control"].is_null());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["messages"][1]["content"][0]["text"], "hi");
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn the_tail_marker_follows_a_tool_result_onto_its_own_message() {
        let body = body(
            "anthropic/claude-opus-5",
            &request(
                "claude-opus-5",
                user(ContentBlock::ToolResult {
                    id: "c1".into(),
                    content: vec![ContentBlock::Text("42".into())],
                    is_error: false,
                }),
            ),
        );
        assert_eq!(markers(&body), 3, "{body:#}");
        let tail = &body["messages"][1];
        assert_eq!(tail["role"], "tool");
        assert_eq!(tail["content"][0]["text"], "42");
        assert_eq!(tail["content"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn an_assistant_tail_gets_no_message_marker() {
        let body = body(
            "anthropic/claude-opus-5",
            &request(
                "claude-opus-5",
                vec![ModelMessage {
                    role: ModelRole::Assistant,
                    content: vec![ContentBlock::Text("thinking".into())],
                }],
            ),
        );
        assert_eq!(markers(&body), 2, "{body:#}");
    }

    /// Everything that is not an Anthropic model here caches implicitly on a
    /// stable prefix. A marker is at best ignored and at worst a 400, and the
    /// system message stays the plain-string form those upstreams expect.
    #[test]
    fn a_non_anthropic_wire_id_gets_no_markers_at_all() {
        let body = body(
            "moonshotai/kimi-k3-fast",
            &request("kimi-k3-fast", user(ContentBlock::Text("hi".into()))),
        );
        assert_eq!(markers(&body), 0, "{body:#}");
        assert_eq!(body["messages"][0]["content"], "sys");
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
