//! Session engines. API inference remains in `model`; CLI engines own their
//! tool loop and return calls to the same Luma tool registry.

pub(crate) mod claim;
mod claude;
mod codex;
mod process;
pub(crate) mod state;

use super::{
    model::{ContentBlock, ToolSpec, Usage},
    tools::ToolOutcome,
    AgentError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    #[default]
    Api,
    Codex,
    Claude,
}

impl Engine {
    pub const CHOICES: &'static [(&'static str, &'static str)] = &[
        ("api", "API"),
        ("codex", "Codex subscription"),
        ("claude", "Claude Code subscription"),
    ];

    pub fn parse(value: &str) -> Result<Self, AgentError> {
        match value {
            "api" => Ok(Self::Api),
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            _ => Err(AgentError::Invalid(format!(
                "unknown agent engine '{value}'"
            ))),
        }
    }

    pub fn configured(settings: &HashMap<String, String>) -> Result<Self, AgentError> {
        Self::parse(settings.get("agent_engine").map_or("api", String::as_str))
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone)]
pub(super) struct Request {
    pub engine: Engine,
    pub model: Option<String>,
    pub system: String,
    pub prompt: String,
    pub tools: Vec<ToolSpec>,
    pub cwd: std::path::PathBuf,
    pub resume: Option<state::NativeSession>,
}

pub(super) enum Event {
    Session {
        id: String,
        model: Option<String>,
    },
    Text(String),
    Reasoning(String),
    Tool {
        id: String,
        name: String,
        input: Value,
        reply: Value,
    },
    Usage(Usage),
    Done,
}

pub(super) enum Session {
    Codex(codex::Session),
    Claude(claude::Session),
}

impl Session {
    pub async fn start(request: Request) -> Result<Self, AgentError> {
        match request.engine {
            Engine::Codex => codex::Session::start(request).await.map(Self::Codex),
            Engine::Claude => claude::Session::start(request).await.map(Self::Claude),
            Engine::Api => Err(AgentError::Invalid("API engine has no subprocess".into())),
        }
    }

    pub async fn next(&mut self) -> Result<Event, AgentError> {
        match self {
            Self::Codex(s) => s.next().await,
            Self::Claude(s) => s.next().await,
        }
    }

    pub fn usage_total(&self) -> Usage {
        match self {
            Self::Codex(s) => s.usage_total(),
            Self::Claude(_) => Usage::default(),
        }
    }

    pub async fn reply(&mut self, id: Value, outcome: ToolOutcome) -> Result<(), AgentError> {
        match self {
            Self::Codex(s) => s.reply(id, outcome).await,
            Self::Claude(s) => s.reply(id, outcome).await,
        }
    }
}

fn tool_definitions(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name":tool.name,
                "description":tool.description,
                "inputSchema":tool.schema,
            })
        })
        .collect()
}

pub(super) fn context_fingerprint(system: &str, tools: &[ToolSpec]) -> String {
    use sha2::{Digest, Sha256};
    let context = serde_json::to_vec(&(system, tool_definitions(tools)))
        .expect("serializable tool configuration");
    format!("{:x}", Sha256::digest(context))
}

fn content(outcome: ToolOutcome) -> (Vec<ContentBlock>, bool) {
    match outcome {
        ToolOutcome::Text(s) => (vec![ContentBlock::Text(s)], false),
        ToolOutcome::Error(s) => (vec![ContentBlock::Text(s)], true),
        ToolOutcome::Content(blocks) => (blocks, false),
    }
}

fn protocol(message: impl Into<String>) -> AgentError {
    AgentError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn request(engine: Engine, cwd: &std::path::Path) -> Request {
        Request {
            engine,
            model: None,
            system: "Use the echo tool.".into(),
            prompt: "Echo hi.".into(),
            tools: vec![ToolSpec {
                name: "echo".into(),
                description: "Echo input".into(),
                schema: serde_json::json!({"type":"object"}),
            }],
            cwd: cwd.into(),
            resume: None,
        }
    }
    #[tokio::test]
    #[ignore = "uses the selected locally authenticated CLI and subscription quota"]
    async fn live_cli_tool_round_trip() {
        let engine =
            Engine::parse(&std::env::var("LUMA_LIVE_ENGINE").unwrap_or_else(|_| "codex".into()))
                .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut request = request(engine, directory.path());
        request.prompt = "Call echo with value hi, then reply DONE. Do nothing else.".into();
        request.tools[0].schema = serde_json::json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false});
        let work = async {
            let mut resume = None;
            for _ in 0..2 {
                request.resume = resume.take();
                let mut session = Session::start(request.clone()).await.unwrap();
                let mut called = false;
                let mut text = String::new();
                loop {
                    match session.next().await.unwrap() {
                        Event::Tool {
                            name, input, reply, ..
                        } => {
                            assert_eq!(name, "echo");
                            assert_eq!(input["value"], "hi");
                            called = true;
                            session
                                .reply(reply, ToolOutcome::Text("hi".into()))
                                .await
                                .unwrap();
                        }
                        Event::Session { id, .. } => {
                            resume = Some(state::NativeSession {
                                id,
                                usage: Usage::default(),
                            })
                        }
                        Event::Text(delta) => text.push_str(&delta),
                        Event::Done => {
                            resume.as_mut().expect("native session").usage = session.usage_total();
                            break;
                        }
                        _ => {}
                    }
                }
                assert!(called, "CLI did not call the scoped tool");
                assert!(text.contains("DONE"), "{text}");
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(90), work)
            .await
            .expect("CLI smoke timed out");
    }
}
