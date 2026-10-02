//! The wire shape of one Python cell: what the host sends and what it gets
//! back (design §15, contract C4).
//!
//! Deliberately notebook-native. The result carries what a notebook cell shows —
//! stdout, stderr, the last-expression repr, a traceback, figures — plus the two
//! things only the host can know: how long it took and which *exceptional* state
//! changes the model must be told about (`notices`). The rest of the host's
//! bookkeeping (execution id, binding revision, kernel generation) stays in Rust.

use serde::{Deserialize, Serialize};

/// One executed cell.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PythonCellResult {
    /// `"ok"` | `"error"` | `"interrupted"` | `"failed"`.
    ///
    /// `error` is a Python-level exception (the kernel is fine); `failed` is an
    /// infrastructure failure — the worker died, timed out past the interrupt
    /// ladder, or never started. The reason is in `notices`.
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
    pub duration_ms: u64,
}

/// A plot or headless scene frame the cell produced. `artifact_rel` locates
/// its workspace file; `base64_png` is what the model is shown.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PythonCellFigure {
    pub artifact_rel: String,
    pub width: u32,
    pub height: u32,
    pub base64_png: String,
}

/// What the transcript keeps for one executed cell — the `python` tool's stored
/// output, and therefore the shape every reader of a persisted turn decodes:
/// the chat panel, the detail view, the model-facing projection.
///
/// It is [`PythonCellResult`] minus what only the run knew (`artifact_rel`),
/// with each figure's bytes replaced by where they are kept. Older rows carry
/// fields this shape no longer has; serde ignores them, and so must every
/// other decoder.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PythonToolOutput {
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    pub repr: Option<String>,
    pub traceback: Option<String>,
    pub notices: Vec<String>,
    pub figures: Vec<PythonStoredFigure>,
    pub duration_ms: u64,
}

/// A figure as the transcript keeps it: its size, and the PNG's
/// `agent-figures/<uid>/<sha>.png` path (see [`crate::agent::figures`]).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PythonStoredFigure {
    pub width: u32,
    pub height: u32,
    pub path: String,
}

/// What the agent is looking at, as the host can describe it.
///
/// Mirrors `agent_execution::bindings::providers::BindingScope` minus
/// `agent_kind` — that is a property of the *thread*, read from the database, not
/// something a caller may assert.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PythonScopeInput {
    pub track_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    /// `[start_s, end_s]` in absolute track seconds.
    pub window: Option<(f64, f64)>,
}
