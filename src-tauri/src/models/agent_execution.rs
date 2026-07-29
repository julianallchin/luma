//! The wire shape of one Python cell: what the frontend sends and what it gets
//! back (design §15, contract C4).
//!
//! Deliberately notebook-native. The result carries what a notebook cell shows —
//! stdout, stderr, the last-expression repr, a traceback, figures — plus the two
//! things only the host can know: how long it took and which *exceptional* state
//! changes the model must be told about (`notices`). The rest of the host's
//! bookkeeping (execution id, binding revision, kernel generation) stays in Rust.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// One executed cell.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PythonCellResult {
    /// `"ok"` | `"error"` | `"interrupted"` | `"failed"`.
    ///
    /// `error` is a Python-level exception (the kernel is fine); `failed` is an
    /// infrastructure failure — the worker died, timed out past the interrupt
    /// ladder, or never started. The reason is in `notices`.
    #[ts(type = r#""ok" | "error" | "interrupted" | "failed""#)]
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    /// The last expression's bounded representation, when the cell ended in one.
    pub repr: Option<String>,
    pub traceback: Option<String>,
    pub figures: Vec<PythonCellFigure>,
    /// Concise prose the agent must see: kernel restarts, dropped figures,
    /// worker warnings. Never a status dump.
    pub notices: Vec<String>,
    #[ts(type = "number")]
    pub duration_ms: u64,
}

/// A Matplotlib figure the cell produced. `artifact_rel` is the durable
/// workspace-relative path the transcript keeps; `base64_png` is the transient
/// copy the model provider needs for an image part (design §14.7 / D10).
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PythonCellFigure {
    pub artifact_rel: String,
    pub width: u32,
    pub height: u32,
    pub base64_png: String,
}

/// What the agent is looking at, as the frontend can describe it.
///
/// Mirrors `agent_execution::bindings::providers::BindingScope` minus
/// `agent_kind` — that is a property of the *thread*, read from the database, not
/// something a caller may assert.
#[derive(TS, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PythonScopeInput {
    pub track_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    pub pattern_id: Option<String>,
    /// `[start_s, end_s]` in absolute track seconds.
    #[ts(type = "[number, number] | null")]
    pub window: Option<(f64, f64)>,
    /// The editor's live (possibly unsaved) graph — the one piece of scope only
    /// the frontend knows.
    #[ts(type = "unknown | null")]
    pub graph_definition: Option<Value>,
}
