//! A scripted agent: the model's turns and the tools it calls, stated as data.
//!
//! The model is the only thing a chat test cannot have for real — it is a
//! network — so it is replayed from the fixture. Below it the turn is real:
//! real threads, real rows, and the shipped tools a test names. A tool a test
//! scripts answers a fixed result after a delay, because every scripted event
//! is ready at once and without a slow tool there is no moment at which a turn
//! is observably half-finished.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use luma_lib::agent::model::scripted::ScriptedModel;
use luma_lib::agent::model::{ModelEvent, StopReason, Usage};
use luma_lib::agent::tools::{Tool, ToolContext, ToolRegistry};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// One model step as JSON: an array of events, each one of
/// `{ text }`, `{ reasoning }`, `{ call, id, args }` or `{ end, usage }`.
///
/// A step with a call and no `end` ends in `tool_use`; any other step without
/// one ends in `end_turn` (the scripted model appends it).
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Event {
    Text {
        text: String,
    },
    Reasoning {
        reasoning: String,
    },
    Call {
        call: String,
        id: String,
        args: Value,
    },
    End {
        end: StopReason,
        #[serde(default)]
        usage: Usage,
    },
}

pub(super) fn steps<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<Option<Vec<Vec<ModelEvent>>>, D::Error> {
    let Some(steps) = Option::<Vec<Vec<Event>>>::deserialize(de)? else {
        return Ok(None);
    };
    Ok(Some(
        steps
            .into_iter()
            .map(|step| {
                let mut out = Vec::new();
                let (mut called, mut ended) = (false, false);
                for event in step {
                    match event {
                        Event::Text { text } => out.push(ModelEvent::TextDelta(text)),
                        Event::Reasoning { reasoning } => {
                            out.push(ModelEvent::ReasoningDelta(reasoning));
                        }
                        Event::Call { call, id, args } => {
                            called = true;
                            out.push(ModelEvent::ToolCallStarted {
                                id: id.clone(),
                                name: call,
                            });
                            out.push(ModelEvent::ToolCallArgsDelta {
                                id: id.clone(),
                                json: args.to_string(),
                            });
                            out.push(ModelEvent::ToolCallEnded { id });
                        }
                        Event::End { end, usage } => {
                            ended = true;
                            out.push(ModelEvent::StepEnded {
                                stop_reason: end,
                                usage,
                            });
                        }
                    }
                }
                if called && !ended {
                    out.push(ModelEvent::StepEnded {
                        stop_reason: StopReason::ToolUse,
                        usage: Usage::default(),
                    });
                }
                out
            })
            .collect(),
    ))
}

pub(super) fn model(steps: &[Vec<ModelEvent>], cadence: Option<Duration>) -> ScriptedModel {
    ScriptedModel::new(steps.to_vec()).with_cadence(cadence.unwrap_or_default())
}

/// A `tools` entry: a shipped tool by name (`"subagent"`), or a scripted one.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum ToolEntry {
    Shipped(String),
    Scripted(ScriptedTool),
}

/// `{ name, result, latency_ms?, error?, description?, schema? }`.
///
/// Named like the tool it stands in for, because a tool's name is how the
/// chat narrates it. `error` answers as a failed call instead of `result`.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScriptedTool {
    name: Name,
    #[serde(default)]
    description: String,
    #[serde(default = "any_object")]
    schema: Value,
    #[serde(default)]
    latency_ms: u64,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<String>,
}

/// `Tool::name` is `&'static str`. One short string per scripted tool per
/// test is the whole leak.
#[derive(Clone, Copy)]
struct Name(&'static str);

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(Self(Box::leak(String::deserialize(de)?.into_boxed_str())))
    }
}

fn any_object() -> Value {
    serde_json::json!({ "type": "object" })
}

#[async_trait::async_trait]
impl Tool for ScriptedTool {
    fn name(&self) -> &'static str {
        self.name.0
    }

    fn description(&self) -> Cow<'static, str> {
        Cow::Owned(self.description.clone())
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    async fn call(&self, _ctx: &ToolContext<'_>, _args: Value) -> Result<Value, String> {
        tokio::time::sleep(Duration::from_millis(self.latency_ms)).await;
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(self.result.clone()),
        }
    }
}

/// The registry `entries` name, in their order. A shipped name that is not a
/// shipped tool is an error, so a typo cannot quietly drop a tool.
pub(super) fn registry(entries: &[ToolEntry]) -> Result<ToolRegistry, String> {
    use luma_lib::agent::tools::{python::PythonTool, skill::SkillTool, subagent::SubagentTool};
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for entry in entries {
        match entry {
            ToolEntry::Scripted(tool) => tools.push(Arc::new(tool.clone())),
            ToolEntry::Shipped(name) => {
                let tool: Arc<dyn Tool> = match name.as_str() {
                    "python" => Arc::new(PythonTool),
                    "skill" => Arc::new(SkillTool),
                    "subagent" => Arc::new(SubagentTool),
                    _ => return Err(format!("no shipped tool {name:?}")),
                };
                tools.push(tool);
            }
        }
    }
    Ok(ToolRegistry::new(tools))
}
