use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::audio::{calculate_frequency_amplitude, generate_melspec, highpass_filter, load_or_decode_audio, lowpass_filter, StemCache, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH};
use crate::fixtures::layout::compute_head_offsets;
use crate::fixtures::parser::parse_definition;
use crate::models::node_graph::*;
use crate::node_graph::executor::{adsr_durations, calc_envelope, shape_curve};
use crate::node_graph::state::ExecutionState;
use crate::node_graph::NodeExecutionContext;
use crate::services::tracks::TARGET_SAMPLE_RATE;
use serde_json;

const CHROMA_DIM: usize = 12;
const PREVIEW_LENGTH: usize = 256;
const SIMULATION_RATE: f32 = 60.0;

mod analysis;
mod apply;
mod audio;
mod color;
mod selection;
mod signals;

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<(), String> {
    if selection::run_node(node, ctx, state).await? {
        return Ok(());
    }
    if audio::run_node(node, ctx, state).await? {
        return Ok(());
    }
    if signals::run_node(node, ctx, state).await? {
        return Ok(());
    }
    if color::run_node(node, ctx, state).await? {
        return Ok(());
    }
    if apply::run_node(node, ctx, state).await? {
        return Ok(());
    }
    if analysis::run_node(node, ctx, state).await? {
        return Ok(());
    }
    println!("Encountered unknown node type '{}'", node.type_id);
    Ok(())
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    let mut types = Vec::new();
    types.extend(selection::get_node_types());
    types.extend(audio::get_node_types());
    types.extend(signals::get_node_types());
    types.extend(color::get_node_types());
    types.extend(apply::get_node_types());
    types.extend(analysis::get_node_types());
    types
}
