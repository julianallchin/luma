use std::collections::HashMap;
use std::path::PathBuf;

use serde_json;
use sqlx::SqlitePool;

use crate::audio::StemCache;
use crate::models::node_graph::{BeatGrid, Edge, GraphContext, NodeInstance, PatternArgDef};
use crate::node_graph::GraphExecutionConfig;

/// Shared, read-only inputs that every node execution might need.
pub struct NodeExecutionContext<'a> {
    pub incoming_edges: &'a HashMap<&'a str, Vec<&'a Edge>>,
    pub nodes_by_id: &'a HashMap<&'a str, &'a NodeInstance>,
    pub pool: &'a SqlitePool,
    pub project_pool: Option<&'a SqlitePool>,
    pub resource_path_root: Option<&'a PathBuf>,
    pub fft_service: &'a crate::audio::FftService,
    pub stem_cache: &'a StemCache,
    pub graph_context: &'a GraphContext,
    pub arg_defs: &'a [PatternArgDef],
    pub arg_values: &'a HashMap<String, serde_json::Value>,
    pub config: &'a GraphExecutionConfig,
    pub context_audio_buffer: Option<&'a crate::node_graph::context::AudioBuffer>,
    pub context_beat_grid: Option<&'a BeatGrid>,
    pub compute_visualizations: bool,
}
