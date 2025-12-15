use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Clone, Debug, TS)]
#[ts(export, export_to = "../../src/bindings/universe.ts")]
#[serde(rename_all = "camelCase")]
pub struct PrimitiveState {
    pub dimmer: f32,        // 0.0 - 1.0
    pub color: [f32; 3],    // RGB [0.0 - 1.0]
    pub strobe: f32,        // 0.0 (off) - 1.0 (fastest)
    pub position: [f32; 2], // [PanDeg, TiltDeg]
    pub speed: f32,         // 0.0 (frozen) or 1.0 (fast) - binary
}

#[derive(Serialize, Deserialize, Clone, Debug, TS)]
#[ts(export, export_to = "../../src/bindings/universe.ts")]
#[serde(rename_all = "camelCase")]
pub struct UniverseState {
    // Key: "fixture-uuid" OR "fixture-uuid:head-index"
    pub primitives: HashMap<String, PrimitiveState>,
}
