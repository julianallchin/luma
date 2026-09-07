//! The `python` tool: one persistent kernel per agent thread.
//!
//! The model supplies a purpose and the code. Workspace, thread, binding
//! revision, scope ids and the graph snapshot are resolved from the thread —
//! never from the model (design §7.1).

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{clamp_for_model, Tool, ToolContext, ToolOutcome};
use crate::agent::model::ContentBlock;
use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::models::agent_execution::{PythonCellResult, PythonStoredFigure, PythonToolOutput};
use crate::services::agent_execution::{
    cancel_python_cell_inner, resolve_execution_id, run_python_cell_inner,
};

/// The description is a cached prompt prefix: it must stay byte-stable for a
/// thread's lifetime, so it lives in a file rather than in a format string.
///
/// Public because it is the contract for *any* host that exposes this kernel —
/// `luma-mcp` hands the same text to an out-of-process coding agent, and a
/// second wording would be a second tool.
pub const PYTHON_TOOL_DESCRIPTION: &str = include_str!("../prompts/python-tool.md");

/// Above this, a single figure's base64 is not persisted in the transcript.
const MAX_PERSISTED_FIGURE_BYTES: usize = 2_000_000;
/// Above this total, remaining figures are not sent to the model.
const MAX_MODEL_FIGURE_BYTES: usize = 6_000_000;

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PythonArgs {
    /// A short noun phrase describing the intended outcome; it must read
    /// naturally after "Running".
    ///
    /// Never read on this side: the chat titles a chip from the *persisted*
    /// input, so this field's whole job is to put itself in the schema.
    #[allow(dead_code)]
    purpose: String,
    /// Python cell source.
    code: String,
}

pub struct PythonTool;

#[async_trait]
impl Tool for PythonTool {
    fn name(&self) -> &'static str {
        "python"
    }

    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(PYTHON_TOOL_DESCRIPTION)
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schemars::schema_for!(PythonArgs)).unwrap_or_else(
            |_| serde_json::json!({ "type": "object", "required": ["purpose", "code"] }),
        )
    }

    async fn call(&self, ctx: &ToolContext<'_>, args: Value) -> Result<Value, String> {
        let args: PythonArgs =
            serde_json::from_value(args).map_err(|error| format!("invalid arguments: {error}"))?;
        // Interrupting the kernel is the *only* thing cancellation has to do
        // here, and dropping this future is the only way a turn is cancelled —
        // so the interrupt hangs off `Drop` rather than a token some caller
        // could forget to pair with the call.
        let execution_id = resolve_execution_id(
            ctx.thread_id,
            ctx.execution_id.map(str::to_string),
            ctx.authored_workspace_id,
        )?;
        let guard = InterruptOnDrop {
            workspaces: Arc::clone(&ctx.services().workspaces),
            execution_id,
            armed: true,
        };

        let principal = ctx
            .services()
            .admitted_principal()
            .await
            .map_err(|error| error.to_string())?;
        let result = run_python_cell_inner(
            &ctx.services().db.0,
            &ctx.services().storage,
            &ctx.services().fixtures_root,
            &ctx.services().workspaces,
            &ctx.services().graph_runs,
            &ctx.services().authored,
            ctx.thread_id.to_string(),
            args.code,
            ctx.scope.clone(),
            Some(ctx.turn_message_id.to_string()),
            principal,
            ctx.execution_id.map(str::to_string),
            ctx.authored_workspace_id.map(str::to_string),
        )
        .await;
        drop(guard.disarm());

        let stored = to_stored_output(result?);
        serde_json::to_value(stored).map_err(|error| error.to_string())
    }

    fn stored_output(&self, stored: &Value) -> ToolOutcome {
        match serde_json::from_value::<PythonToolOutput>(stored.clone()) {
            Ok(output) => ToolOutcome::Content(model_output(&output)),
            Err(error) => ToolOutcome::Error(format!("unreadable python result: {error}")),
        }
    }
}

struct InterruptOnDrop {
    workspaces: Arc<PythonWorkspaceService>,
    execution_id: String,
    armed: bool,
}

impl InterruptOnDrop {
    fn disarm(mut self) -> Self {
        self.armed = false;
        self
    }
}

impl Drop for InterruptOnDrop {
    fn drop(&mut self) {
        if self.armed {
            cancel_python_cell_inner(&self.workspaces, &self.execution_id);
        }
    }
}

fn to_stored_output(result: PythonCellResult) -> PythonToolOutput {
    PythonToolOutput {
        status: result.status,
        stdout: result.stdout,
        stderr: result.stderr,
        repr: result.repr,
        traceback: result.traceback,
        notices: result.notices,
        figures: result
            .figures
            .into_iter()
            .map(|figure| PythonStoredFigure {
                width: figure.width,
                height: figure.height,
                base64_png: (figure.base64_png.len() <= MAX_PERSISTED_FIGURE_BYTES)
                    .then_some(figure.base64_png),
            })
            .collect(),
        duration_ms: result.duration_ms,
    }
}

/// The model-facing projection of one executed cell.
///
/// Every host that shows a cell to a model goes through here: the in-app tool
/// after it has persisted the transcript row, `luma-mcp` straight off the
/// service result. The clamping and the figure budget are part of the contract,
/// not of either transport.
#[must_use]
pub fn cell_content_blocks(result: PythonCellResult) -> Vec<ContentBlock> {
    model_output(&to_stored_output(result))
}

/// Notebook-native model output: one text block assembling notices, stdout,
/// stderr, traceback and repr, then one image per figure.
fn model_output(output: &PythonToolOutput) -> Vec<ContentBlock> {
    let mut sections: Vec<String> = Vec::new();
    for notice in &output.notices {
        sections.push(format!("note: {notice}"));
    }
    if !output.stdout.trim().is_empty() {
        sections.push(format!(
            "stdout:\n{}",
            clamp_for_model(output.stdout.trim_end_matches('\n'), 8_000, "stdout", 0.4)
        ));
    }
    if !output.stderr.trim().is_empty() {
        sections.push(format!(
            "stderr:\n{}",
            clamp_for_model(output.stderr.trim_end_matches('\n'), 4_000, "stderr", 0.4)
        ));
    }
    if let Some(traceback) = &output.traceback {
        // Tail-biased: the raising frame and the error line live at the bottom.
        sections.push(clamp_for_model(
            traceback.trim_end_matches('\n'),
            6_000,
            "traceback",
            0.75,
        ));
    }
    if let Some(repr) = &output.repr {
        sections.push(repr.clone());
    }
    if output.status == "interrupted" {
        sections.push("Cell interrupted before it finished.".into());
    } else if sections.is_empty() && output.figures.is_empty() {
        sections.push("(no output)".into());
    }

    let mut blocks = Vec::new();
    let text = sections.join("\n\n");
    if !text.is_empty() {
        blocks.push(ContentBlock::Text(text));
    }
    let mut budget = MAX_MODEL_FIGURE_BYTES;
    let mut omitted = 0usize;
    for figure in &output.figures {
        match &figure.base64_png {
            Some(data) if data.len() <= budget => {
                budget -= data.len();
                blocks.push(ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: data.clone(),
                });
            }
            _ => omitted += 1,
        }
    }
    if omitted > 0 {
        blocks.push(ContentBlock::Text(format!(
            "note: {omitted} further figure(s) were too large to include. Plot fewer or smaller figures per cell."
        )));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traceback_reaches_the_model_as_one_text_block() {
        let output = PythonToolOutput {
            status: "error".into(),
            stdout: "loading\n".into(),
            traceback: Some("Traceback…\nValueError: no".into()),
            ..PythonToolOutput::default()
        };
        let blocks = model_output(&output);
        let ContentBlock::Text(text) = &blocks[0] else {
            panic!("expected text");
        };
        assert!(text.contains("stdout:\nloading"));
        assert!(text.contains("ValueError: no"));
    }

    #[test]
    fn an_empty_cell_still_says_something() {
        let blocks = model_output(&PythonToolOutput {
            status: "ok".into(),
            ..PythonToolOutput::default()
        });
        assert_eq!(blocks, vec![ContentBlock::Text("(no output)".into())]);
    }

    /// Rows persisted before a field was removed must still read: the
    /// transcript is append-only and never migrated.
    #[test]
    fn a_stored_row_with_a_retired_field_still_reads() {
        let stored = serde_json::json!({
            "status": "ok", "stdout": "", "stderr": "", "repr": null,
            "traceback": null, "notices": [], "figures": [], "durationMs": 1,
            "figureCount": 0,
        });
        assert!(matches!(
            PythonTool.stored_output(&stored),
            ToolOutcome::Content(_)
        ));
    }

    #[test]
    fn the_schema_names_both_arguments() {
        let schema = PythonTool.schema();
        assert!(schema["properties"]["purpose"].is_object());
        assert!(schema["properties"]["code"].is_object());
    }
}
