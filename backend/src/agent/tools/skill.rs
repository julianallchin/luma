//! The `skill` tool: load one playbook into the turn.
//!
//! The *listing* of what exists is not here — it is in the system prompt, where
//! every host can carry it whether or not it has this tool (`agent::skills`).
//! This tool is only the fetch, so its description stays short and static and
//! the cached prompt prefix does not move when a playbook is edited.
//!
//! Why a tool at all, when the listing already names an absolute path: this
//! loop's only other tool is Python in a seatbelt sandbox that cannot read the
//! resource directory, and a named call is a harder affordance than "please
//! read this file" — the reference harness admits its models often skip the
//! read. If the in-app agent ever gains a file reader, delete this tool and let
//! `<location>` do the work; the envelope is chosen so nothing else changes.

use std::borrow::Cow;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Tool, ToolContext, ToolOutcome};
use crate::agent::skills;

/// Description of the fetch itself. The menu lives in the system prompt.
///
/// Public, and a file rather than a literal, for the reason
/// `PYTHON_TOOL_DESCRIPTION` is: the TypeScript loop imports this same file
/// with `?raw`, and a second wording would be a second tool.
pub const SKILL_TOOL_DESCRIPTION: &str = include_str!("../prompts/skill-tool.md");

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(SKILL_TOOL_DESCRIPTION.trim_end())
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name from <available_skills>.",
                },
            },
            "required": ["name"],
        })
    }

    async fn call(&self, _ctx: &ToolContext<'_>, args: Value) -> Result<Value, String> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let registry = skills::bundled();
        let skill = registry.get(name).ok_or_else(|| {
            format!(
                "unknown skill '{name}'. Available: {}",
                registry.names().join(", ")
            )
        })?;
        Ok(json!({ "name": skill.name, "body": skill.envelope() }))
    }

    fn stored_output(&self, stored: &Value) -> ToolOutcome {
        match stored.get("body").and_then(Value::as_str) {
            Some(body) => ToolOutcome::Text(body.to_string()),
            None => ToolOutcome::Error("the skill result was not readable".into()),
        }
    }
}
