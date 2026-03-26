use tauri::{AppHandle, State};

use std::collections::HashMap;

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::models::node_graph::{BeatGrid, Graph, GraphContext, NodeTypeDef, RunResult};
use crate::models::universe::UniverseState;
use crate::render_engine::RenderEngine;

#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    crate::node_graph::nodes::get_node_types()
}

#[tauri::command]
pub async fn run_graph(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    crate::node_graph::run_graph(
        app,
        db,
        render_engine,
        stem_cache,
        fft_service,
        graph,
        context,
    )
    .await
}

/// Precompute a looping pattern preview as a sequence of UniverseState frames.
/// Used by the hover-card preview to play back smoothly without per-frame IPC.
#[tauri::command]
pub async fn preview_pattern(
    app: AppHandle,
    db: State<'_, Db>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    pattern_id: String,
    track_id: String,
    venue_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
    fps: f32,
) -> Result<Vec<UniverseState>, String> {
    use crate::compositor::{
        fetch_pattern_graph, fetch_track_path_and_hash, get_or_load_shared_audio,
    };
    use crate::engine::render_frame;
    use crate::node_graph::{run_graph_internal, GraphExecutionConfig};

    let duration = end_time - start_time;
    if duration <= 0.0 {
        return Err("Preview duration must be positive".into());
    }

    let fps = fps.clamp(10.0, 30.0);
    let frame_count = (duration * fps).ceil() as usize;
    let frame_count = frame_count.min(256); // cap to prevent huge allocations

    // 1. Fetch pattern graph
    let graph_json = fetch_pattern_graph(&db.0, &pattern_id).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;

    // 2. Load shared audio context
    let (track_path, track_hash) = fetch_track_path_and_hash(&db.0, &track_id).await?;
    let shared_audio = get_or_load_shared_audio(&track_id, &track_path, &track_hash).await?;

    // 3. Resolve fixture path
    let final_path = crate::services::fixtures::resolve_fixtures_root(&app).ok();

    // 4. Build arg_values: force all Selection args to "all"
    let arg_values: HashMap<String, serde_json::Value> = graph
        .args
        .iter()
        .map(|arg| {
            let value = match arg.arg_type {
                crate::models::node_graph::PatternArgType::Selection => {
                    serde_json::json!({ "expression": "all", "spatialReference": "global" })
                }
                _ => arg.default_value.clone(),
            };
            (arg.id.clone(), value)
        })
        .collect();

    let context = GraphContext {
        track_id,
        venue_id,
        start_time,
        end_time,
        beat_grid,
        arg_values: Some(arg_values),
        instance_seed: Some(42), // deterministic for preview
    };

    let (_result, layer) = run_graph_internal(
        &db.0,
        Some(&db.0),
        &stem_cache,
        &fft_service,
        final_path,
        graph,
        context,
        GraphExecutionConfig {
            compute_visualizations: false,
            log_summary: false,
            log_primitives: false,
            shared_audio: Some(shared_audio),
        },
    )
    .await?;

    // 5. Sample frames
    let layer = layer.ok_or("Pattern produced no output")?;
    let dt = duration / frame_count as f32;
    let frames: Vec<UniverseState> = (0..frame_count)
        .map(|i| render_frame(&layer, start_time + i as f32 * dt))
        .collect();

    Ok(frames)
}
