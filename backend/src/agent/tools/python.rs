//! The `python` tool: one persistent kernel per agent thread.
//!
//! The model supplies a verb, a purpose and the code. Workspace, thread, binding
//! revision, scope ids and the graph snapshot come from the host-selected turn
//! context, never from model tool arguments.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{clamp_for_model, Tool, ToolContext, ToolOutcome};
use crate::agent::figures;
use crate::agent::model::ContentBlock;
use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::models::agent_execution::{PythonCellResult, PythonStoredFigure, PythonToolOutput};
use crate::services::agent_execution::{
    cancel_python_cell_inner, resolve_execution_id, run_python_cell_inner,
};
use crate::storage::StorageRoot;

/// The description is a cached prompt prefix: it must stay byte-stable for a
/// thread's lifetime, so it lives in a file rather than in a format string.
///
/// Public because it is the contract for *any* host that exposes this kernel —
/// `luma-mcp` hands the same text to an out-of-process coding agent, and a
/// second wording would be a second tool.
pub const PYTHON_TOOL_DESCRIPTION: &str = include_str!("../prompts/python-tool.md");

/// Above this total, remaining figures are not sent to the model.
const MAX_MODEL_FIGURE_BYTES: usize = 6_000_000;

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PythonArgs {
    /// The verb for the cell's work, ending in -ing, for example "Measuring".
    ///
    /// Never read on this side, like the two fields after it: the chat titles
    /// a chip from the *persisted* input, so their whole job is to put
    /// themselves in the schema.
    #[allow(dead_code)]
    verb: String,
    /// The past tense of `verb`, for example "Measured".
    #[allow(dead_code)]
    verb_past: String,
    /// A short phrase that follows the verb, for example "beat energy in bars
    /// 7–13".
    #[allow(dead_code)]
    purpose: String,
    /// Python cell source.
    code: String,
}

/// Holds the storage root because a stored figure is a path: replaying a cell
/// to the model reads the figure's bytes back out of the local cache.
pub struct PythonTool {
    storage: StorageRoot,
}

impl PythonTool {
    #[must_use]
    pub fn new(storage: StorageRoot) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl Tool for PythonTool {
    fn name(&self) -> &'static str {
        "python"
    }

    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(PYTHON_TOOL_DESCRIPTION)
    }

    fn schema(&self) -> Value {
        serde_json::to_value(schemars::schema_for!(PythonArgs)).unwrap_or_else(|_| {
            serde_json::json!({
                "type": "object",
                "required": ["verb", "verbPast", "purpose", "code"],
            })
        })
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
            ctx.draft_id,
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
            ctx.thread_id.to_string(),
            args.code,
            ctx.scope.clone(),
            Some(ctx.turn_message_id.to_string()),
            principal.clone(),
            ctx.execution_id.map(str::to_string),
            ctx.draft_id.map(str::to_string),
        )
        .await;
        drop(guard.disarm());

        let mut result = result?;
        let mut stored = Vec::with_capacity(result.figures.len());
        for figure in std::mem::take(&mut result.figures) {
            let png = base64::engine::general_purpose::STANDARD
                .decode(figure.base64_png.as_bytes())
                .map_err(|error| format!("unreadable figure {}: {error}", figure.artifact_rel))?;
            stored.push(PythonStoredFigure {
                width: figure.width,
                height: figure.height,
                path: figures::store(
                    &ctx.services().db.0,
                    &ctx.services().storage,
                    principal.as_deref(),
                    &png,
                )
                .await?,
            });
        }
        serde_json::to_value(to_stored_output(result, stored)).map_err(|error| error.to_string())
    }

    fn stored_output(&self, stored: &Value) -> ToolOutcome {
        match serde_json::from_value::<PythonToolOutput>(stored.clone()) {
            Ok(output) => {
                // A figure not on this machine (made on another device, never
                // opened here) is left out, as an oversized one is.
                let images = output
                    .figures
                    .iter()
                    .map(|figure| {
                        std::fs::read(figures::cached(&self.storage, &figure.path))
                            .ok()
                            .map(|png| base64::engine::general_purpose::STANDARD.encode(png))
                    })
                    .collect();
                ToolOutcome::Content(model_output(&output, images))
            }
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

fn to_stored_output(
    result: PythonCellResult,
    figures: Vec<PythonStoredFigure>,
) -> PythonToolOutput {
    PythonToolOutput {
        status: result.status,
        stdout: result.stdout,
        stderr: result.stderr,
        repr: result.repr,
        traceback: result.traceback,
        notices: result.notices,
        figures,
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
pub fn cell_content_blocks(mut result: PythonCellResult) -> Vec<ContentBlock> {
    let images = std::mem::take(&mut result.figures)
        .into_iter()
        .map(|figure| Some(figure.base64_png))
        .collect();
    model_output(&to_stored_output(result, Vec::new()), images)
}

/// Notebook-native model output: one text block assembling notices, stdout,
/// stderr, traceback and repr, then one image per figure. `images` holds each
/// figure's base64 PNG, `None` where the bytes are not available.
fn model_output(output: &PythonToolOutput, images: Vec<Option<String>>) -> Vec<ContentBlock> {
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
    } else if sections.is_empty() && images.is_empty() {
        sections.push("(no output)".into());
    }

    let mut blocks = Vec::new();
    let text = sections.join("\n\n");
    if !text.is_empty() {
        blocks.push(ContentBlock::Text(text));
    }
    let mut budget = MAX_MODEL_FIGURE_BYTES;
    let mut omitted = 0usize;
    for image in images {
        match image {
            Some(data) if data.len() <= budget => {
                budget -= data.len();
                blocks.push(ContentBlock::Image {
                    media_type: "image/png".into(),
                    data,
                });
            }
            _ => omitted += 1,
        }
    }
    if omitted > 0 {
        blocks.push(ContentBlock::Text(format!(
            "note: {omitted} further figure(s) were too large or not available to include. Plot fewer or smaller figures per cell."
        )));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(dir: &tempfile::TempDir) -> PythonTool {
        PythonTool::new(StorageRoot::from_path(dir.path().to_path_buf()))
    }

    #[test]
    fn a_traceback_reaches_the_model_as_one_text_block() {
        let output = PythonToolOutput {
            status: "error".into(),
            stdout: "loading\n".into(),
            traceback: Some("Traceback…\nValueError: no".into()),
            ..PythonToolOutput::default()
        };
        let blocks = model_output(&output, Vec::new());
        let ContentBlock::Text(text) = &blocks[0] else {
            panic!("expected text");
        };
        assert!(text.contains("stdout:\nloading"));
        assert!(text.contains("ValueError: no"));
    }

    #[test]
    fn an_empty_cell_still_says_something() {
        let blocks = model_output(
            &PythonToolOutput {
                status: "ok".into(),
                ..PythonToolOutput::default()
            },
            Vec::new(),
        );
        assert_eq!(blocks, vec![ContentBlock::Text("(no output)".into())]);
    }

    /// Rows persisted before a field was removed must still read: the
    /// transcript is append-only and never migrated.
    #[test]
    fn a_stored_row_with_a_retired_field_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let stored = serde_json::json!({
            "status": "ok", "stdout": "", "stderr": "", "repr": null,
            "traceback": null, "notices": [], "figures": [], "durationMs": 1,
            "figureCount": 0,
        });
        assert!(matches!(
            tool(&dir).stored_output(&stored),
            ToolOutcome::Content(_)
        ));
    }

    #[test]
    fn the_schema_names_every_argument() {
        let dir = tempfile::tempdir().unwrap();
        let schema = tool(&dir).schema();
        assert!(schema["properties"]["verb"].is_object());
        assert!(schema["properties"]["verbPast"].is_object());
        assert!(schema["properties"]["purpose"].is_object());
        assert!(schema["properties"]["code"].is_object());
    }

    /// A stored figure is a path. Replaying the cell shows the model the
    /// cached picture, and leaves out one this machine does not have.
    #[test]
    fn a_stored_figure_reaches_the_model_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageRoot::from_path(dir.path().to_path_buf());
        std::fs::create_dir_all(storage.agent_figure_path("a").parent().unwrap()).unwrap();
        std::fs::write(storage.agent_figure_path("a"), b"png").unwrap();
        let stored = serde_json::json!({
            "status": "ok", "stdout": "", "stderr": "", "repr": null,
            "traceback": null, "notices": [], "durationMs": 1,
            "figures": [
                {"width": 4, "height": 3, "path": "agent-figures/u1/a.png"},
                {"width": 4, "height": 3, "path": "agent-figures/u1/b.png"}
            ],
        });
        let ToolOutcome::Content(blocks) = tool(&dir).stored_output(&stored) else {
            panic!("expected content");
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"png");
        assert!(matches!(&blocks[0], ContentBlock::Image { data, .. } if *data == encoded));
        assert!(
            matches!(&blocks[1], ContentBlock::Text(note) if note.contains("1 further figure"))
        );
    }

    #[test]
    fn live_delivery_keeps_the_figure_budget() {
        let capture: PythonCellResult = serde_json::from_value(serde_json::json!({
            "status":"ok", "stdout":"", "stderr":"", "repr":null,
            "traceback":null, "notices":[], "durationMs":1,
            "figures":[
                {"artifactRel":"outputs/large.png", "width":1280, "height":720,
                 "base64Png":"a".repeat(2_000_004)},
                {"artifactRel":"outputs/other.png", "width":2000, "height":2000,
                 "base64Png":"b".repeat(MAX_MODEL_FIGURE_BYTES)}
            ]
        }))
        .unwrap();
        let blocks = cell_content_blocks(capture);
        assert!(matches!(&blocks[0], ContentBlock::Image { data, .. } if data.len() == 2_000_004));
        assert!(
            matches!(&blocks[1], ContentBlock::Text(note) if note.contains("1 further figure"))
        );
    }
}
