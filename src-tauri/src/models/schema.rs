use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum PortType {
    Intensity,
    Audio,
    BeatGrid,
    Series,
    Color,
    Selection,
    Signal,
    Gradient,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum ParamType {
    Number,
    Text,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum PatternArgType {
    Color,
    Scalar,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternArgDef {
    pub id: String,
    pub name: String,
    pub arg_type: PatternArgType,
    #[ts(type = "Record<string, unknown>")]
    pub default_value: Value,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PortDef {
    pub id: String,
    pub name: String,
    pub port_type: PortType,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ParamDef {
    pub id: String,
    pub name: String,
    pub param_type: ParamType,
    pub default_number: Option<f32>,
    pub default_text: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct NodeTypeDef {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub inputs: Vec<PortDef>,
    pub outputs: Vec<PortDef>,
    pub params: Vec<ParamDef>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct NodeInstance {
    pub id: String,
    pub type_id: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: HashMap<String, Value>,
    pub position_x: Option<f64>,
    pub position_y: Option<f64>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Graph {
    pub nodes: Vec<NodeInstance>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub args: Vec<PatternArgDef>,
}

/// Context provided by the host for graph execution.
/// The host is responsible for loading audio and computing beat grids.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct GraphContext {
    #[ts(type = "number")]
    pub track_id: i64,
    pub start_time: f32,
    pub end_time: f32,
    pub beat_grid: Option<BeatGrid>,
    #[ts(type = "Record<string, unknown> | undefined")]
    pub arg_values: Option<HashMap<String, Value>>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct BeatGrid {
    pub beats: Vec<f32>,
    pub downbeats: Vec<f32>,
    pub bpm: f32,
    pub downbeat_offset: f32,
    pub beats_per_bar: i32,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct SeriesSample {
    pub time: f32,
    pub values: Vec<f32>,
    pub label: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Series {
    pub dim: usize,
    pub labels: Option<Vec<String>>,
    pub samples: Vec<SeriesSample>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct SelectableItem {
    pub id: String, // Unique primitive ID (e.g., "fixture-1:0")
    pub fixture_id: String,
    pub head_index: usize,
    pub pos: (f32, f32, f32), // Global position (x, y, z)
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct Selection {
    pub items: Vec<SelectableItem>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct Signal {
    pub n: usize,       // Spatial dimension (Selection size)
    pub t: usize,       // Temporal dimension (Time samples)
    pub c: usize,       // Channel dimension (Data components)
    pub data: Vec<f32>, // Flat buffer: [n * (t * c) + t * c + c]
}

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct AudioCrop {
    pub start_seconds: f32,
    pub end_seconds: f32,
}

#[allow(dead_code)]
#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum BlendMode {
    Replace,
    Add,
    Multiply,
    Screen,
    Max,
    Min,
    Lighten,
    Value, // New "Value" blend mode
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct PrimitiveTimeSeries {
    pub primitive_id: String,
    // Using Series for each capability
    pub color: Option<Series>,    // dim=3 (RGB) or 4 (RGBW)
    pub dimmer: Option<Series>,   // dim=1
    pub position: Option<Series>, // dim=2 (Pan, Tilt)
    pub strobe: Option<Series>,   // dim=2 (Enabled, Rate)
    pub speed: Option<Series>,    // dim=1 (0 = frozen, 1 = fast)
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct LayerTimeSeries {
    pub primitives: Vec<PrimitiveTimeSeries>,
}

#[derive(TS, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct RunResult {
    pub views: HashMap<String, Signal>,
    pub mel_specs: HashMap<String, crate::models::tracks::MelSpec>,
    pub color_views: HashMap<String, String>,
    pub universe_state: Option<crate::models::universe::UniverseState>,
}
