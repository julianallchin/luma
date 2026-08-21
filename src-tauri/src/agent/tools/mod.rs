//! The tool surface: one trait, one registry, one schema per argument struct.
//!
//! A tool is bound to a *context*, never to a host. That is what makes a
//! subagent's tool set identical to its parent's by construction rather than by
//! assertion: the same [`registry`] call builds both, and only the
//! [`ToolContext`] differs (a child execution namespace, a detached authored
//! workspace). There is no second builder that could drift.

pub mod python;

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::model::{ContentBlock, ToolSpec};
use crate::dispatch::AppServices;
use crate::models::agent_execution::PythonScopeInput;

/// What a tool result becomes *for the model*: prose, a failure, or content
/// blocks (which is how a figure or a rendered preview reaches a vision model).
///
/// Distinct from what the tool *stores*: the transcript keeps the full capture,
/// the model sees a clamped, notebook-shaped rendering of it.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOutcome {
    Text(String),
    Error(String),
    Content(Vec<ContentBlock>),
}

/// Everything a tool call knows about where it is running.
///
/// Deliberately not a host handle: a tool reaches services, the durable thread,
/// and the scope the *thread* declares — never a window, a canvas, or an
/// editor. The two host-flavoured tools in the TypeScript stack are handled by
/// turn events instead (§1).
pub struct ToolContext<'a> {
    pub services: &'a AppServices,
    pub thread_id: &'a str,
    /// The durable assistant row this call is attributed to. A cell with edit
    /// authority must be traceable to a persisted turn, so this is not optional.
    pub turn_message_id: &'a str,
    /// A child execution namespace, for a subagent. `None` runs in the
    /// thread's own kernel.
    pub execution_id: Option<&'a str>,
    /// A detached authored document this call may write. `None` writes the
    /// thread's own.
    pub authored_workspace_id: Option<&'a str>,
    /// What the agent is looking at, resolved from the thread — never asserted
    /// by the model.
    pub scope: &'a PythonScopeInput,
}

/// One capability the model can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> Cow<'static, str>;

    /// JSON Schema for the arguments object, derived from the argument struct
    /// so the two cannot drift.
    fn schema(&self) -> Value;

    /// Run the call and return what the **transcript** should store.
    ///
    /// # Errors
    ///
    /// A human-readable failure, stored as the part's `errorText` and shown to
    /// the model as an error result. A tool failing is normal; it must not fail
    /// the turn.
    async fn call(&self, ctx: &ToolContext<'_>, args: Value) -> Result<Value, String>;

    /// Rebuild the model-facing result from what the transcript stored. Pure,
    /// so rehydrating an old thread reproduces exactly what the model saw.
    fn stored_output(&self, stored: &Value) -> ToolOutcome {
        ToolOutcome::Text(stored.to_string())
    }
}

/// The tools one agent may call, in a stable order (prompt caching keys on the
/// serialized tool list, so ordering is a correctness concern, not a style one).
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { tools }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(AsRef::as_ref)
    }

    /// The provider-facing declarations.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|tool| ToolSpec {
                name: tool.name().to_string(),
                description: tool.description().into_owned(),
                schema: tool.schema(),
            })
            .collect()
    }

    /// The tool names this registry exposes, for the subagent surface check.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }
}

/// The tool set for an agent kind. Both a parent turn and a subagent turn call
/// this; they differ only in the [`ToolContext`] they pass to the result.
#[must_use]
pub fn registry(kind: super::AgentKind) -> ToolRegistry {
    match kind {
        // The graph agent's own tools (graph edits, `ask_venue`, `preview`)
        // are not ported yet; it shares the notebook until they are.
        super::AgentKind::TrackCopilot | super::AgentKind::PatternGraph => {
            ToolRegistry::new(vec![Arc::new(python::PythonTool)])
        }
    }
}

/// Head+tail clamp for model-facing tool text.
///
/// The transcript keeps the full capture; this trims what enters the model's
/// context. Middle-out, because the head carries the shape of an output and the
/// tail carries the conclusion (or the error) — and the marker tells the model
/// to inspect smaller slices rather than print everything.
#[must_use]
pub fn clamp_for_model(text: &str, max_chars: usize, label: &str, tail_share: f64) -> String {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() <= max_chars {
        return text.to_string();
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let tail = (max_chars as f64 * tail_share) as usize;
    let head = max_chars - tail;
    let omitted = characters.len() - max_chars;
    let head_text: String = characters[..head].iter().collect();
    let tail_text: String = characters[characters.len() - tail..].iter().collect();
    format!(
        "{head_text}\n… [{omitted} chars of {label} omitted — inspect smaller slices instead of printing everything]\n{tail_text}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamping_keeps_both_ends_and_says_what_it_dropped() {
        let text = "a".repeat(100);
        let clamped = clamp_for_model(&text, 20, "stdout", 0.4);
        assert!(clamped.starts_with(&"a".repeat(12)));
        assert!(clamped.contains("80 chars of stdout omitted"));
        assert!(clamp_for_model("short", 20, "stdout", 0.4) == "short");
    }
}
