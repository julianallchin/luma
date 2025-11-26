use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct PlaybackStateSnapshot {
    pub active_node_id: Option<String>,
    pub is_playing: bool,
    pub current_time: f32,
    pub duration_seconds: f32,
}
