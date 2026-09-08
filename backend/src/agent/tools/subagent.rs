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

/// Keep the advertised schema a plain object: MCP and native tool adapters
/// require an object root. Conditional requirements belong to host validation.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubagentArgs {
    /// Delegate work (default), or inspect a completed child's proposal.
    #[serde(default)]
    action: SubagentAction,
    /// Required for delegate: short label naming the work.
    description: Option<String>,
    /// Required for delegate: standalone brief; the child sees no parent conversation.
    task: Option<String>,
    /// Required for inspect: the child thread returned by delegation.
    child_thread_id: Option<String>,
    /// Inspect only: omit to list proposal files and conflicts; provide a listed path to read source.
    path: Option<String>,
    /// Inspect only: zero-based character offset, default 0; at most 3000 characters per version.
    offset: Option<usize>,
}

#[derive(Default, serde::Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SubagentAction {
    #[default]
    Delegate,
    Inspect,
}

impl SubagentArgs {
    fn validate(&self) -> Result<(), String> {
        match self.action {
            SubagentAction::Delegate => {
                if self
                    .description
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
                    || self.task.as_deref().is_none_or(|s| s.trim().is_empty())
                {
                    return Err("delegate requires nonempty description and task".into());
                }
                if self.child_thread_id.is_some() || self.path.is_some() || self.offset.is_some() {
                    return Err(
                        "childThreadId, path and offset are only valid with action=inspect".into(),
                    );
                }
            }
            SubagentAction::Inspect => {
                if self
                    .child_thread_id
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
                {
                    return Err("inspect requires childThreadId from a completed delegation".into());
                }
                if self.description.is_some() || self.task.is_some() {
                    return Err("description and task are only valid with action=delegate".into());
                }
                if self.offset.is_some() && self.path.is_none() {
                    return Err("inspect offset requires a source path".into());
                }
            }
        }
        Ok(())
    }
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
        args.validate()?;
        match args.action {
            SubagentAction::Delegate => {
                let report = delegate(
                    ctx,
                    Delegation {
                        description: args.description.expect("validated description"),
                        task: args.task.expect("validated task"),
                    },
                )
                .await?;
                serde_json::to_value(report).map_err(|error| error.to_string())
            }
            SubagentAction::Inspect => {
                let child_thread_id = args.child_thread_id.expect("validated childThreadId");
                let services = ctx.services();
                if services.subagents.is_running(&child_thread_id) {
                    return Err(
                        "the child is still running; inspect its proposal after completion".into(),
                    );
                }
                let principal = services
                    .admitted_principal()
                    .await
                    .map_err(|error| error.to_string())?;
                let proposal = services
                    .authored()
                    .subagent_proposal(
                        &services.db().0,
                        principal.as_deref(),
                        ctx.thread_id,
                        &child_thread_id,
                        args.path.as_deref(),
                        args.offset.unwrap_or(0),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(serde_json::json!({"proposal":proposal}))
            }
        }
    }

    fn stored_output(&self, stored: &Value) -> ToolOutcome {
        if let Some(proposal) = stored.get("proposal") {
            return ToolOutcome::Text(proposal.to_string());
        }
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
    fn action_requirements_are_validated_before_execution() {
        for value in [
            serde_json::json!({"description":"Build the rig","task":"Place four lights"}),
            serde_json::json!({"action":"delegate","description":"Build the rig","task":"Place four lights"}),
            serde_json::json!({"action":"inspect","childThreadId":"child-1"}),
            serde_json::json!({"action":"inspect","childThreadId":"child-1","path":"score.json","offset":3000}),
        ] {
            serde_json::from_value::<SubagentArgs>(value)
                .unwrap()
                .validate()
                .unwrap();
        }
        for value in [
            serde_json::json!({}),
            serde_json::json!({"action":"inspect"}),
            serde_json::json!({"description":"Work","task":"Do it","childThreadId":"child-1"}),
            serde_json::json!({"action":"inspect","childThreadId":"child-1","task":"Do it"}),
            serde_json::json!({"action":"inspect","childThreadId":"child-1","offset":10}),
        ] {
            assert!(serde_json::from_value::<SubagentArgs>(value)
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(serde_json::from_value::<SubagentArgs>(
            serde_json::json!({"action":"inspcet","description":"Work","task":"Do it"})
        )
        .is_err());
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
        assert!(text.starts_with("<authored_merge"), "{text}");
        assert!(text.ends_with("Raised the ramp."), "{text}");
        assert!(text.contains(r#"<authored_merge status="merged" revision_id="rev-9"/>"#));
    }
}
