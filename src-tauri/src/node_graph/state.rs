use crate::models::node_graph::{BeatGrid, LayerTimeSeries, Selection, Signal};
use crate::models::tracks::MelSpec;
use std::collections::HashMap;

use super::context::AudioBuffer;

#[derive(Clone)]
pub struct RootCache {
    pub sections: Vec<crate::root_worker::ChordSection>,
    pub logits_path: Option<String>,
}

#[derive(Clone)]
pub struct NodeTiming {
    pub id: String,
    pub type_id: String,
    pub ms: f64,
}

pub struct ExecutionState {
    pub audio_buffers: HashMap<(String, String), AudioBuffer>,
    pub beat_grids: HashMap<(String, String), BeatGrid>,
    pub selections: HashMap<(String, String), Vec<Selection>>,
    pub signal_outputs: HashMap<(String, String), Signal>,
    /// Sorted timestamps in absolute track time (seconds). Produced by event
    /// sources (drum_events, beat_pulses) and consumed by ADSR-style nodes.
    pub event_outputs: HashMap<(String, String), Vec<f32>>,
    pub apply_outputs: Vec<LayerTimeSeries>,
    pub color_outputs: HashMap<(String, String), String>,
    pub root_caches: HashMap<String, RootCache>,
    pub view_results: HashMap<String, Signal>,
    pub mel_specs: HashMap<String, MelSpec>,
    pub color_views: HashMap<String, String>,
    pub node_timings: Vec<NodeTiming>,
}

impl ExecutionState {
    pub fn new() -> Self {
        Self {
            audio_buffers: HashMap::new(),
            beat_grids: HashMap::new(),
            selections: HashMap::new(),
            signal_outputs: HashMap::new(),
            event_outputs: HashMap::new(),
            apply_outputs: Vec::new(),
            color_outputs: HashMap::new(),
            root_caches: HashMap::new(),
            view_results: HashMap::new(),
            mel_specs: HashMap::new(),
            color_views: HashMap::new(),
            node_timings: Vec::new(),
        }
    }

    pub fn record_timing(&mut self, id: String, type_id: String, ms: f64) {
        self.node_timings.push(NodeTiming { id, type_id, ms });
    }
}
