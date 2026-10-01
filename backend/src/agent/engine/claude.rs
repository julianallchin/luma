//! Claude's stream-json control protocol, including SDK MCP requests. The
//! unmodified CLI owns authentication; Luma owns every advertised tool.

use super::{
    content,
    process::{command, Process},
    protocol, AgentError, ContentBlock, Event, Request, ToolOutcome, Usage,
};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(in crate::agent) struct Session {
    process: Process,
    request: Request,
    completed: bool,
    final_answer: Option<String>,
    pub(super) last_usage: Option<Usage>,
    pub(super) context_window: Option<u64>,
    model: Option<String>,
    /// Steers written to stdin whose replay has not come back, oldest first.
    steers: Arc<Mutex<VecDeque<String>>>,
    /// A turn end held back while steers wait for their replay.
    held: Option<Held>,
    /// What the request in flight read, as its `message_start` said.
    request_usage: Option<Usage>,
}

struct Held {
    final_answer: Option<String>,
    deadline: tokio::time::Instant,
}

// Adapted from Comet, MIT, (c) 2026 Wing: the held turn end.
/// How long a turn end waits for the replay of a steer written before it. A
/// `now` steer interrupts the turn it lands in, and the CLI (2.1.283, checked
/// by hand) ends that turn with a `result` — an error one when it cut a step
/// short — and then replays the steer and answers it. If nothing follows,
/// the steers were absorbed into the turn that ended.
const HELD_RESULT_SETTLE: Duration = Duration::from_secs(5);

impl Session {
    pub async fn start(request: Request) -> Result<Self, AgentError> {
        let mut auth = claude_command(&request.cwd);
        auth.args(["auth", "status", "--json"]).kill_on_drop(true);
        let status = tokio::time::timeout(std::time::Duration::from_secs(15), auth.output())
            .await
            .map_err(|_| protocol("Claude authentication check timed out"))?
            .map_err(|e| protocol(format!("could not check Claude authentication: {e}")))?;
        let status: Value = serde_json::from_slice(&status.stdout)
            .map_err(|e| protocol(format!("invalid Claude authentication status: {e}")))?;
        if status["loggedIn"] != true || status["authMethod"] != "claude.ai" {
            return Err(protocol("A new Claude CLI process reports no Claude.ai login. An already-open terminal session may still be signed in. Run `claude auth login` in a fresh terminal, then retry."));
        }
        let mut cmd = stream_command(&request.cwd);
        cmd.arg("--system-prompt").arg(&request.system);
        // Without this, the CLI freezes the system prompt at a conversation's
        // first turn and reuses that snapshot verbatim on every later resume
        // — including a hydrated one — no matter what `--system-prompt` this
        // call passed. Luma's system prompt is the same for every turn of a
        // thread, but a newer build's prompt should reach an older thread.
        cmd.arg("--system-prompt-snapshot").arg("off");
        cmd.arg("--mcp-config")
            .arg(json!({"mcpServers":{"luma":{"type":"sdk","name":"luma"}}}).to_string());
        if let Some(effort) = &request.effort {
            cmd.arg("--effort").arg(effort);
        }
        if let Some(model) = &request.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(resume) = &request.resume {
            cmd.arg("--resume").arg(&resume.id);
        }
        Self::connect(request, Process::start(cmd)?).await
    }

    async fn connect(request: Request, mut process: Process) -> Result<Self, AgentError> {
        initialize(&mut process).await?;
        Ok(Self {
            process,
            request,
            completed: false,
            final_answer: None,
            last_usage: None,
            context_window: None,
            model: None,
            steers: Arc::default(),
            held: None,
            request_usage: None,
        })
    }

    pub(super) fn steerer(&self) -> Steerer {
        Steerer {
            input: self.process.input(),
            steers: Arc::clone(&self.steers),
        }
    }

    pub async fn next(&mut self) -> Result<Event, AgentError> {
        loop {
            if let Some(text) = self.final_answer.take() {
                return Ok(Event::FinalAnswer(text));
            }
            if self.completed {
                return Ok(Event::Done);
            }
            let frame = match self.held.as_ref().map(|held| held.deadline) {
                None => self.process.read().await?,
                Some(deadline) => {
                    match tokio::time::timeout_at(deadline, self.process.read()).await {
                        Ok(frame) => {
                            // Still producing: not the quiet end the hold waits for.
                            if let Some(held) = &mut self.held {
                                held.deadline = tokio::time::Instant::now() + HELD_RESULT_SETTLE;
                            }
                            frame?
                        }
                        Err(_) => {
                            let held = self.held.take().expect("held above");
                            self.completed = true;
                            self.final_answer = held.final_answer;
                            let absorbed: Vec<String> =
                                self.steers.lock().expect("steers").drain(..).collect();
                            if absorbed.is_empty() {
                                continue;
                            }
                            return Ok(Event::Steered(absorbed));
                        }
                    }
                }
            };
            match frame["type"].as_str().unwrap_or("") {
                "control_response" if frame["response"]["request_id"] == "initialize" => {
                    if frame["response"]["subtype"] != "success" {
                        // Naming the installed CLI version turns a rejected
                        // `--resume` (a hydrated session's on-disk shape is
                        // reverse-engineered, not documented — see
                        // `claude_session`) into something a human can act
                        // on immediately, without a separate version check.
                        let version = cli_version(&self.request.cwd).await;
                        return Err(protocol(format!(
                            "Claude CLI {version} rejected session initialization: {}",
                            frame["response"]
                        )));
                    }
                    self.process.send(json!({"type":"user","message":{"role":"user","content":text_blocks(&self.request.prompt)}})).await?;
                }
                "control_request" => {
                    let request = &frame["request"];
                    let id = frame["request_id"].clone();
                    match request["subtype"].as_str().unwrap_or("") {
                        "mcp_message" if request["server_name"] == "luma" => {
                            let message = &request["message"];
                            let reply = json!({"control":id,"mcp":message["id"]});
                            match message["method"].as_str().unwrap_or("") {
                                "tools/call" => return Ok(Event::Tool {
                                    id: format!("claude-{}",id.as_str().unwrap_or("tool")),
                                    name: message["params"]["name"].as_str().ok_or_else(|| protocol("Claude tool has no name"))?.into(),
                                    input: message["params"]["arguments"].clone(), reply,
                                }),
                                "initialize" => self.mcp_reply(reply,json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"luma","version":env!("CARGO_PKG_VERSION")}})).await?,
                                "tools/list" => {
                                    self.mcp_reply(reply,json!({"tools":super::tool_definitions(&self.request.tools)})).await?;
                                }
                                "notifications/initialized" | "ping" => self.mcp_reply(reply,json!({})).await?,
                                method => return Err(protocol(format!("unsupported Claude MCP method: {method}"))),
                            }
                        }
                        "can_use_tool" => {
                            let allowed = request["tool_name"].as_str().is_some_and(|name| {
                                self.request
                                    .tools
                                    .iter()
                                    .any(|tool| name == format!("mcp__luma__{}", tool.name))
                            });
                            let response = if allowed {
                                json!({"behavior":"allow","updatedInput":request["input"]})
                            } else {
                                json!({"behavior":"deny","message":"Only Luma tools are available in this session"})
                            };
                            self.control_reply(id, response).await?;
                        }
                        subtype => {
                            return Err(protocol(format!(
                                "unsupported Claude control request: {subtype}"
                            )))
                        }
                    }
                }
                "system" if frame["subtype"] == "init" => {
                    return Ok(Event::Session {
                        id: frame["session_id"]
                            .as_str()
                            .ok_or_else(|| protocol("Claude returned no session id"))?
                            .into(),
                        model: frame["model"].as_str().map(str::to_string),
                    });
                }
                "user" if frame["parent_tool_use_id"].is_null() => {
                    // Only the CLI's replay says a steer joined the
                    // conversation. A replay confirms its steer and every
                    // earlier one: a steer superseded by a later `now` is
                    // never replayed itself.
                    let uuid = frame["uuid"].as_str().unwrap_or_default();
                    let mut steers = self.steers.lock().expect("steers");
                    if let Some(at) = steers.iter().position(|id| id == uuid) {
                        return Ok(Event::Steered(steers.drain(..=at).collect()));
                    }
                }
                "stream_event" => {
                    let event = &frame["event"];
                    if frame["parent_tool_use_id"].is_null() {
                        match event["type"].as_str().unwrap_or("") {
                            "message_start" => {
                                self.request_usage = Some(usage_of(&event["message"]["usage"]));
                            }
                            // The request's end: its final usage, all of it.
                            "message_delta" => {
                                let end = usage_of(&event["usage"]);
                                let start = self.request_usage.take().unwrap_or_default();
                                let usage = Usage {
                                    input_tokens: start.input_tokens.max(end.input_tokens),
                                    output_tokens: start.output_tokens.max(end.output_tokens),
                                    cache_creation_input_tokens: start
                                        .cache_creation_input_tokens
                                        .max(end.cache_creation_input_tokens),
                                    cache_read_input_tokens: start
                                        .cache_read_input_tokens
                                        .max(end.cache_read_input_tokens),
                                };
                                self.last_usage = Some(usage);
                                return Ok(Event::Step(usage));
                            }
                            _ => {}
                        }
                    }
                    match event["delta"]["type"].as_str().unwrap_or("") {
                        "text_delta" => {
                            return Ok(Event::Text(
                                event["delta"]["text"].as_str().unwrap_or("").into(),
                            ))
                        }
                        "thinking_delta" => {
                            return Ok(Event::Reasoning(
                                event["delta"]["thinking"].as_str().unwrap_or("").into(),
                            ))
                        }
                        _ => {}
                    }
                }
                "assistant" => {
                    let message = &frame["message"];
                    if let Some(model) = message["model"].as_str() {
                        self.model = Some(model.to_owned());
                        // The CLI reports the window only in the turn's
                        // closing `result`, after every step has been
                        // recorded; until then each step uses the picker's.
                        if self.context_window.is_none() {
                            self.context_window = window(model).map(u64::from);
                        }
                    }
                    if let Some(usage) = message.get("usage") {
                        self.last_usage = Some(usage_of(usage));
                    }
                    if frame.get("error").is_some() {
                        return Err(claude_error(&frame));
                    }
                }

                "result" => {
                    let steering = !self.steers.lock().expect("steers").is_empty();
                    if frame["is_error"] == true && !steering {
                        return Err(claude_error(&frame));
                    }
                    let final_answer = frame["result"].as_str().map(str::to_owned);
                    if steering {
                        // A steer boundary, not the end of the run.
                        self.held = Some(Held {
                            final_answer,
                            deadline: tokio::time::Instant::now() + HELD_RESULT_SETTLE,
                        });
                    } else {
                        self.held = None;
                        self.completed = true;
                        self.final_answer = final_answer;
                    }
                    let models = frame["modelUsage"].as_object();
                    let model_usage = models.and_then(|models| {
                        self.model
                            .as_ref()
                            .and_then(|model| {
                                models.get(model).or_else(|| {
                                    models.values().find(|entry| {
                                        entry["canonicalModel"].as_str() == Some(model)
                                    })
                                })
                            })
                            .or_else(|| {
                                (models.len() == 1)
                                    .then(|| models.values().next())
                                    .flatten()
                            })
                    });
                    if let Some(window) = model_usage
                        .and_then(|usage| usage["contextWindow"].as_u64())
                        .filter(|window| *window > 0)
                    {
                        self.context_window = Some(window);
                    }
                    return Ok(Event::Usage(usage_of(&frame["usage"])));
                }
                _ => {}
            }
        }
    }

    async fn control_reply(&mut self, id: Value, response: Value) -> Result<(), AgentError> {
        self.process.send(json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}})).await
    }

    async fn mcp_reply(&mut self, id: Value, result: Value) -> Result<(), AgentError> {
        self.control_reply(
            id["control"].clone(),
            json!({"mcp_response":{"jsonrpc":"2.0","id":id["mcp"],"result":result}}),
        )
        .await
    }

    pub(super) fn input(&self) -> super::process::Input {
        self.process.input()
    }

    #[cfg(test)]
    pub async fn reply(&mut self, id: Value, outcome: ToolOutcome) -> Result<(), AgentError> {
        self.process.send(reply_frame(id, outcome)).await
    }
}
/// Writes steers into a running session's stdin.
pub(in crate::agent) struct Steerer {
    input: super::process::Input,
    steers: Arc<Mutex<VecDeque<String>>>,
}

impl Steerer {
    pub async fn steer(
        &self,
        id: &str,
        blocks: &[String],
        tools_open: bool,
    ) -> Result<(), AgentError> {
        // Recorded before the write, so a replay can never outrun it.
        self.steers.lock().expect("steers").push_back(id.to_owned());
        self.input.send(steer_frame(blocks, id, !tools_open)).await
    }
}

/// A user message's content: one text block each.
fn text_blocks(blocks: &[String]) -> Value {
    blocks
        .iter()
        .map(|text| json!({"type":"text","text":text}))
        .collect()
}

// Adapted from Comet, MIT, (c) 2026 Wing.
/// A steer line. `priority: "now"` stops streaming text or thinking at once
/// and the steer is answered next. But `now` also aborts an in-flight MCP tool
/// call, so while any tool is open the steer goes as `next`: the tool
/// finishes and the steer lands right after its result, in the same turn.
fn steer_frame(blocks: &[String], id: &str, immediate: bool) -> Value {
    json!({"type":"user", "uuid":id, "priority": if immediate { "now" } else { "next" },
        "message":{"role":"user","content":text_blocks(blocks)}, "parent_tool_use_id":null})
}

fn stream_command(cwd: &std::path::Path) -> tokio::process::Command {
    let mut cmd = claude_command(cwd);
    cmd.args([
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // Echoes each stdin user message when the CLI takes it: the one
        // signal that says where a steer joined the conversation.
        "--replay-user-messages",
        "--tools",
        "",
        "--strict-mcp-config",
        "--permission-prompt-tool",
        "stdio",
        "--setting-sources",
        "",
        "--max-turns",
        "64",
    ]);
    cmd
}

/// The Claude models Luma names itself, ported from zeron's
/// `crates/harness/src/claude/catalog.rs`: id, name, effort levels. The CLI's
/// own list only adds ids missing here, because its names and descriptions
/// change between releases.
const CURATED: &[(&str, &str, &[&str])] = &[
    ("claude-fable-5-1", "Fable 5.1", FULL_EFFORT),
    ("claude-fable-5", "Fable 5", FULL_EFFORT),
    ("claude-opus-5-5", "Opus 5.5", FULL_EFFORT),
    ("claude-opus-4-8", "Opus 4.8", FULL_EFFORT),
    ("claude-opus-4-7", "Opus 4.7", FULL_EFFORT),
    ("claude-sonnet-5-5", "Sonnet 5.5", FULL_EFFORT),
    ("claude-haiku-4-5", "Haiku 4.5", &[]),
];

const FULL_EFFORT: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Prompt tokens each Claude model accepts. The CLI's model list does not
/// say; it reports a window only in a turn's `modelUsage`. Each value is
/// from the model's page under platform.claude.com/docs/en/models/, read
/// 2026-10-01, and the windows Claude Code 2.1.286 reported in turns agree
/// (Fable 5.1, Opus 5.5, Sonnet 5: 1M; Haiku 4.5: 200K).
const WINDOWS: &[(&str, u32)] = &[
    ("claude-fable-5-1", 1_000_000),
    ("claude-fable-5", 1_000_000),
    ("claude-opus-5-5", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-5-5", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-haiku-4-5", 200_000),
];

/// `id`'s window, also under a dated id (`claude-haiku-4-5-20251001`) or the
/// CLI's `[1m]` spelling. `None` for a model the table does not know: no
/// number is better than a guessed one.
pub(super) fn window(id: &str) -> Option<u32> {
    let id = id.strip_suffix("[1m]").unwrap_or(id);
    let undated = match id.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => id,
    };
    WINDOWS
        .iter()
        .find(|(known, _)| *known == undated)
        .map(|(_, window)| *window)
}

/// CLI aliases that are not model ids. A saved selection must name a model.
const ALIASES: &[&str] = &["default", "opus", "sonnet", "haiku", "fable"];

pub(super) async fn models(
    cwd: &std::path::Path,
) -> Result<Vec<super::catalog::ModelChoice>, AgentError> {
    // A CLI that cannot start or is logged out still gets the curated list.
    let live = match discover(cwd).await {
        Ok(live) => live,
        Err(error) => {
            log::warn!("[agent] Claude model discovery failed, using the built-in list: {error}");
            Vec::new()
        }
    };
    Ok(catalog(&live))
}

async fn discover(cwd: &std::path::Path) -> Result<Vec<Value>, AgentError> {
    let mut process = Process::start(stream_command(cwd))?;
    initialize(&mut process).await?;
    loop {
        let frame = process.read().await?;
        if frame["type"] == "control_response" && frame["response"]["request_id"] == "initialize" {
            return frame
                .pointer("/response/response/models")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| protocol("Claude did not return its model catalog"));
        }
    }
}

/// The curated rows, then each live model id they miss, with the CLI's
/// default model first behind a "Default" row.
fn catalog(live: &[Value]) -> Vec<super::catalog::ModelChoice> {
    let choice = |id: &str, label: &str, efforts: Vec<String>| super::catalog::ModelChoice {
        id: Some(id.into()),
        label: label.into(),
        resolved_model: Some(id.into()),
        effort_levels: efforts,
        context_window: window(id),
        price: None,
    };
    let mut models: Vec<_> = CURATED
        .iter()
        .map(|(id, label, efforts)| {
            choice(id, label, efforts.iter().map(|e| e.to_string()).collect())
        })
        .collect();
    let mut default = None;
    for entry in live {
        let text = |key: &str| {
            entry[key]
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
        };
        let Some(id) = text("resolvedModel").or_else(|| text("value")) else {
            continue;
        };
        if ALIASES.contains(&id.strip_suffix("[1m]").unwrap_or(id)) {
            continue;
        }
        if text("value") == Some("default") {
            default = Some(id.to_string());
        }
        let label = text("displayName").unwrap_or(id);
        // A dated id such as `claude-haiku-4-5-20251001` is the curated row
        // under another name.
        if models.iter().any(|m| m.id.as_deref() == Some(id) || m.label == label) {
            continue;
        }
        let efforts = entry["supportedEffortLevels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        models.push(choice(id, label, efforts));
    }
    let default_label = default.as_deref().and_then(|default| {
        let index = models
            .iter()
            .position(|m| m.id.as_deref() == Some(default))?;
        let model = models.remove(index);
        let label = model.label.clone();
        models.insert(0, model);
        Some(label)
    });
    models.insert(
        0,
        super::catalog::ModelChoice {
            id: None,
            label: match default_label {
                Some(label) => format!("Default · {label}"),
                None => "Default".into(),
            },
            context_window: default.as_deref().and_then(window),
            resolved_model: default,
            effort_levels: FULL_EFFORT.iter().map(|e| e.to_string()).collect(),
            price: None,
        },
    );
    models
}

async fn initialize(process: &mut Process) -> Result<(), AgentError> {
    process.send(json!({"type":"control_request","request_id":"initialize","request":{"subtype":"initialize","hooks":null}})).await
}

/// The installed CLI's own `--version` output, best-effort. Only called on
/// an initialization-failure path, so a slow or failing version check never
/// costs the happy path anything.
async fn cli_version(cwd: &std::path::Path) -> String {
    let mut cmd = claude_command(cwd);
    cmd.arg("--version").kill_on_drop(true);
    match tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await {
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        _ => "(version unknown)".into(),
    }
}

fn claude_command(cwd: &std::path::Path) -> tokio::process::Command {
    let mut cmd = command("claude", cwd);
    for variable in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        cmd.env_remove(variable);
    }
    cmd
}

fn claude_error(frame: &Value) -> AgentError {
    let messages: Vec<_> = frame["errors"]
        .as_array()
        .or_else(|| frame.pointer("/message/content").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().or_else(|| value["text"].as_str()))
        .filter(|text| !text.trim().is_empty())
        .collect();
    let message = if messages.is_empty() {
        ["result", "error", "subtype"]
            .into_iter()
            .filter_map(|key| frame[key].as_str())
            .find(|text| !text.trim().is_empty())
            .unwrap_or("unknown provider error")
            .to_owned()
    } else {
        messages.join("\n")
    };
    protocol(format!("Claude: {message}"))
}

fn count(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

fn usage_of(usage: &Value) -> Usage {
    Usage {
        input_tokens: count(usage, "input_tokens"),
        output_tokens: count(usage, "output_tokens"),
        cache_creation_input_tokens: count(usage, "cache_creation_input_tokens"),
        cache_read_input_tokens: count(usage, "cache_read_input_tokens"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_errors_preserve_the_explanation() {
        for frame in [
            json!({"error":"invalid_request","message":{"content":[{"type":"text","text":"Request rejected: try a new session."}]}}),
            json!({"is_error":true,"errors":[],"result":"Request rejected: try a new session."}),
            json!({"is_error":true,"errors":["Request rejected: try a new session."]}),
        ] {
            assert!(claude_error(&frame)
                .to_string()
                .contains("Request rejected: try a new session."));
        }
        assert!(claude_error(&json!({"error":"invalid_request"}))
            .to_string()
            .contains("invalid_request"));
    }

    /// Rows are named by model, not by the CLI's taglines. Aliases and dated
    /// ids of curated models add no rows; a model the list lacks is added.
    #[test]
    fn catalog_names_models_and_drops_aliases() {
        let live = [
            json!({"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)","description":"Opus 5.5 · Best for everyday, complex tasks"}),
            json!({"value":"opus","resolvedModel":"claude-opus-5-5","displayName":"Opus 5.5","description":"For complex work and everyday tasks"}),
            json!({"value":"sonnet","resolvedModel":"claude-sonnet-5-5","displayName":"Sonnet 5.5","description":"Most efficient for simpler tasks","supportedEffortLevels":["low","high"]}),
            json!({"value":"haiku","resolvedModel":"claude-haiku-4-5-20251001","displayName":"Haiku 4.5"}),
        ];
        let models = catalog(&live);
        let labels: Vec<_> = models.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Default · Opus 5.5",
                "Opus 5.5",
                "Fable 5.1",
                "Fable 5",
                "Opus 4.8",
                "Opus 4.7",
                "Sonnet 5.5",
                "Haiku 4.5",
            ]
        );
        assert_eq!(models[0].id, None);
        assert_eq!(catalog(&[])[0].label, "Default");
    }

    /// Every row names its window: curated rows, rows only the CLI lists,
    /// dated ids, and the Default row through the model it resolves to.
    #[test]
    fn every_claude_row_shows_its_window() {
        let live = [
            json!({"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)"}),
            json!({"value":"sonnet","resolvedModel":"claude-sonnet-5-5","displayName":"Sonnet 5.5"}),
            json!({"value":"haiku","resolvedModel":"claude-haiku-4-5-20251001","displayName":"Haiku 4.5"}),
            json!({"value":"claude-opus-4-6","resolvedModel":"claude-opus-4-6","displayName":"Opus 4.6"}),
        ];
        let models = catalog(&live);
        for model in &models {
            assert!(model.context_window.is_some(), "{} has no window", model.label);
        }
        let window_of = |label: &str| {
            models
                .iter()
                .find(|m| m.label == label)
                .and_then(|m| m.context_window)
        };
        assert_eq!(window_of("Default · Opus 5.5"), Some(1_000_000));
        assert_eq!(window_of("Opus 5.5"), Some(1_000_000));
        assert_eq!(window_of("Sonnet 5.5"), Some(1_000_000));
        assert_eq!(window_of("Haiku 4.5"), Some(200_000));
        assert_eq!(window("claude-haiku-4-5-20251001"), Some(200_000));
        assert_eq!(window("claude-opus-5-5[1m]"), Some(1_000_000));
        assert_eq!(window("claude-unknown-9"), None);
        assert_eq!(catalog(&[])[0].context_window, None);
    }

    #[tokio::test]
    async fn scoped_mcp_round_trip_and_denied_native_tool() {
        let temp = tempfile::tempdir().unwrap();
        let script = r#"
import json,sys
read=lambda: json.loads(sys.stdin.readline())
def send(x): print(json.dumps(x),flush=True)
def control(id,request): send({'type':'control_request','request_id':id,'request':request})
assert read()['request']['subtype']=='initialize'
control('list',{'subtype':'mcp_message','server_name':'luma','message':{'id':1,'method':'tools/list'}})
assert read()['response']['response']['mcp_response']['result']['tools'][0]['name']=='echo'
send({'type':'control_response','response':{'request_id':'initialize','subtype':'success','response':{}}})
assert read()['type']=='user'
send({'type':'system','subtype':'init','session_id':'native'})
control('deny',{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'touch forbidden'}})
assert read()['response']['response']['behavior']=='deny'
control('call',{'subtype':'mcp_message','server_name':'luma','message':{'id':2,'method':'tools/call','params':{'name':'echo','arguments':{'value':'hi'}}}})
reply=read()['response']['response']['mcp_response']
assert reply['id']==2
assert reply['result']['content'][0]['type']=='image'
send({'type':'stream_event','event':{'delta':{'type':'text_delta','text':'done'}}})
send({'type':'assistant','message':{'model':'claude-sonnet-5','usage':{'input_tokens':6,'output_tokens':1}}})
send({'type':'result','is_error':False,'usage':{'input_tokens':10,'output_tokens':2},'modelUsage':{'claude-sonnet-5':{'contextWindow':200000},'child':{'contextWindow':1000000}}})
"#;
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", script]);
        let request = super::super::tests::request(super::super::Engine::Claude, temp.path());
        let mut session = Session::connect(request, Process::start(command).unwrap())
            .await
            .unwrap();
        assert!(matches!(session.next().await.unwrap(),Event::Session {id,..} if id == "native"));
        let Event::Tool {
            name, input, reply, ..
        } = session.next().await.unwrap()
        else {
            panic!("tool")
        };
        assert_eq!(name, "echo");
        assert_eq!(input, json!({"value":"hi"}));
        session
            .reply(
                reply,
                ToolOutcome::Content(vec![ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                }]),
            )
            .await
            .unwrap();
        assert!(matches!(session.next().await.unwrap(),Event::Text(text) if text=="done"));
        assert!(
            matches!(session.next().await.unwrap(),Event::Usage(usage) if usage.input_tokens==10)
        );
        assert_eq!(session.context_window, Some(200_000));
        assert_eq!(session.last_usage.expect("last request").input_tokens, 6);
        assert!(matches!(session.next().await.unwrap(), Event::Done));
    }

    /// The Claude CLI's steering, as 2.1.283 was seen doing it by hand: a
    /// `now` steer ends the running turn with an error result, then replays
    /// the steer and answers it. The result is a steer boundary, the replay
    /// places the steer, and each request reports its usage as it ends.
    #[tokio::test]
    async fn a_steer_waits_for_its_replay_and_each_request_reports_usage() {
        let temp = tempfile::tempdir().unwrap();
        let script = r#"
import json,sys
read=lambda: json.loads(sys.stdin.readline())
def send(x): print(json.dumps(x),flush=True)
def request(start,end):
    send({'type':'stream_event','parent_tool_use_id':None,'event':{'type':'message_start','message':{'usage':start}}})
    send({'type':'stream_event','parent_tool_use_id':None,'event':{'type':'message_delta','delta':{'stop_reason':'end_turn'},'usage':end}})
assert read()['request']['subtype']=='initialize'
send({'type':'control_response','response':{'request_id':'initialize','subtype':'success','response':{}}})
first=read()
assert first['message']=={'role':'user','content':[{'type':'text','text':'Echo hi.'}]}, first
send({'type':'system','subtype':'init','session_id':'native'})
send({'type':'user','uuid':'cli-own','isReplay':True,'parent_tool_use_id':None,'message':first['message']})
request({'input_tokens':10,'cache_read_input_tokens':4000,'output_tokens':1},{'output_tokens':30})
steer=read()
assert steer=={'type':'user','uuid':'s1','priority':'now','message':{'role':'user','content':[{'type':'text','text':'darker'},{'type':'text','text':'<editor-context>'}]},'parent_tool_use_id':None}, steer
send({'type':'result','subtype':'error_during_execution','is_error':True,'errors':['[ede_diagnostic] interrupted']})
send({'type':'system','subtype':'init','session_id':'native'})
send({'type':'user','uuid':'s1','isReplay':True,'parent_tool_use_id':None,'message':steer['message']})
request({'input_tokens':10,'cache_read_input_tokens':5000,'output_tokens':1},{'input_tokens':10,'cache_read_input_tokens':5000,'output_tokens':7})
send({'type':'result','is_error':False,'result':'darker it is','usage':{'input_tokens':20,'output_tokens':37},'modelUsage':{'claude-sonnet-5':{'contextWindow':200000}}})
"#;
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", script]);
        let request = super::super::tests::request(super::super::Engine::Claude, temp.path());
        let mut session = Session::connect(request, Process::start(command).unwrap())
            .await
            .unwrap();
        let steerer = session.steerer();
        assert!(matches!(
            session.next().await.unwrap(),
            Event::Session { .. }
        ));
        let Event::Step(first) = session.next().await.unwrap() else {
            panic!("the first request's usage")
        };
        assert_eq!(
            (first.cache_read_input_tokens, first.output_tokens),
            (4000, 30)
        );
        // The typed text and the editor context go as separate blocks.
        steerer
            .steer("s1", &["darker".into(), "<editor-context>".into()], false)
            .await
            .unwrap();
        // The interrupted turn's end is not the run's end.
        assert!(matches!(session.next().await.unwrap(), Event::Usage(_)));
        assert!(matches!(
            session.next().await.unwrap(),
            Event::Session { .. }
        ));
        assert!(matches!(session.next().await.unwrap(), Event::Steered(ids) if ids == ["s1"]));
        let Event::Step(second) = session.next().await.unwrap() else {
            panic!("the second request's usage")
        };
        assert!(
            second.cache_read_input_tokens > first.cache_read_input_tokens,
            "each request reports its own prompt"
        );
        assert!(matches!(session.next().await.unwrap(), Event::Usage(_)));
        assert!(
            matches!(session.next().await.unwrap(), Event::FinalAnswer(text) if text == "darker it is")
        );
        assert!(matches!(session.next().await.unwrap(), Event::Done));
    }

    /// A steer written while a tool runs must not abort it.
    #[test]
    fn a_steer_waits_behind_an_open_tool() {
        let blocks = ["x".to_string()];
        assert_eq!(steer_frame(&blocks, "id", false)["priority"], "next");
        assert_eq!(steer_frame(&blocks, "id", true)["priority"], "now");
    }
}

pub(super) fn reply_frame(id: Value, outcome: ToolOutcome) -> Value {
    let (blocks, failed) = content(outcome);
    let blocks: Vec<_> = blocks
        .into_iter()
        .filter_map(|b| match b {
            ContentBlock::Text(text) => Some(json!({"type":"text","text":text})),
            ContentBlock::Image { media_type, data } => {
                Some(json!({"type":"image","mimeType":media_type,"data":data}))
            }
            _ => None,
        })
        .collect();
    json!({"type":"control_response","response":{"subtype":"success","request_id":id["control"],"response":{"mcp_response":{"jsonrpc":"2.0","id":id["mcp"],"result":{"content":blocks,"isError":failed}}}}})
}
