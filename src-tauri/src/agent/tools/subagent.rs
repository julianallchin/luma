//! The `subagent` tool: delegate a turn to a thread of its own.
//!
//! Schema and plumbing only. Everything that makes a delegation different from
//! an ordinary turn — the limits, the progress stream, the publish — is in
//! [`crate::agent::subagent`], so this file stays what every other tool is: an
//! argument struct and a call.

use std::borrow::Cow;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{Tool, ToolContext, ToolOutcome};
use crate::agent::subagent::{delegate, report_for_model, Delegation, SubagentReport};

/// The description is a cached prompt prefix: it must stay byte-stable for a
/// thread's lifetime, so it lives in a file rather than in a format string.
pub const SUBAGENT_TOOL_DESCRIPTION: &str = include_str!("../prompts/subagent-tool.md");

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SubagentArgs {
    /// Three to five words, present tense, naming the work — "Fitting the
    /// chorus ramp". This is the only text a reader sees at a glance.
    description: String,
    /// The complete brief. The subagent sees nothing else of this
    /// conversation.
    task: String,
}

pub struct SubagentTool;

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &'static str {
        "subagent"
    }

    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(SUBAGENT_TOOL_DESCRIPTION.trim_end())
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schemars::schema_for!(SubagentArgs)).unwrap_or_else(
            |_| serde_json::json!({ "type": "object", "required": ["description", "task"] }),
        )
    }

    /// A refused delegation is an `Err` — nothing was created, so there is
    /// nothing for the transcript to point at. A delegation that ran and then
    /// failed is an `Ok` report whose outcome says so: the child's thread is
    /// the record, and losing the id would lose the record.
    async fn call(&self, ctx: &ToolContext<'_>, args: Value) -> Result<Value, String> {
        let args: SubagentArgs =
            serde_json::from_value(args).map_err(|error| format!("invalid arguments: {error}"))?;
        let report = delegate(
            ctx,
            Delegation {
                description: args.description,
                task: args.task,
            },
        )
        .await?;
        serde_json::to_value(report).map_err(|error| error.to_string())
    }

    fn stored_output(&self, stored: &Value) -> ToolOutcome {
        match serde_json::from_value::<SubagentReport>(stored.clone()) {
            Ok(report) => match report_for_model(&report) {
                Ok(text) => ToolOutcome::Text(text),
                Err(message) => ToolOutcome::Error(message),
            },
            Err(error) => ToolOutcome::Error(format!("unreadable subagent result: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_names_both_arguments() {
        let schema = SubagentTool.schema();
        assert!(schema["properties"]["description"].is_object());
        assert!(schema["properties"]["task"].is_object());
    }

    #[test]
    fn a_failed_subagent_reaches_the_model_as_an_error_that_names_its_thread() {
        let stored = serde_json::json!({
            "childThreadId": "child-1",
            "text": "",
            "outcome": { "status": "failed", "message": "no kernel" },
        });
        let ToolOutcome::Error(message) = SubagentTool.stored_output(&stored) else {
            panic!("a failed subagent must read as an error");
        };
        assert!(message.contains("no kernel"), "{message}");
        assert!(message.contains("child-1"), "{message}");
    }

    #[test]
    fn a_merged_subagent_reaches_the_model_as_its_answer_plus_the_revision() {
        let stored = serde_json::json!({
            "childThreadId": "child-1",
            "text": "Raised the ramp.",
            "outcome": { "status": "merged", "revisionId": "rev-9" },
        });
        let ToolOutcome::Text(text) = SubagentTool.stored_output(&stored) else {
            panic!("a merged subagent must read as text");
        };
        assert!(text.starts_with("Raised the ramp."), "{text}");
        assert!(text.contains(r#"<authored_merge status="merged" revision_id="rev-9"/>"#));
    }
}
