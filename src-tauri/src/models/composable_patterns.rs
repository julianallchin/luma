//! The composable-pattern preview boundary. Graph/input schemas come directly
//! from luma-patterns; the native and JSON hosts use the same definitions.
use super::{selection::Selection, universe::UniverseState};
use luma_patterns::{Cell, Library, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComposablePreviewRequest {
    pub venue_id: String,
    pub track_id: String,
    pub definition: String,
    #[serde(default)]
    pub library: Option<Library>,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
    /// Each selection is one mapping group. Overlaps are rejected rather than
    /// silently assigning a cell to an arbitrary coordinate frame.
    pub targets: Vec<Selection>,
    pub times: Vec<f64>,
    pub clip_start: f64,
    /// Clip end in track seconds, used by clip-relative gradient/time nodes.
    pub clip_end: f64,
    #[serde(default)]
    pub seed: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposablePreview {
    pub cells: Vec<Cell>,
    pub beats: Vec<f64>,
    pub frames: Vec<UniverseState>,
}
