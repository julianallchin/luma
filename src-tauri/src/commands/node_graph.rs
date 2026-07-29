use tauri::{AppHandle, State};

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::eval::compile::compile_pattern;
use crate::eval::context::build_resident_context;
use crate::eval::graph_run::{evaluate_graph, merge_arg_values, EvaluateOptions};
use crate::eval::{Arena, CompiledAnnotation, Scene};
use crate::models::node_graph::{BeatGrid, Graph, GraphContext, NodeTypeDef, RunResult};
use crate::models::universe::UniverseState;
use crate::render_engine::RenderEngine;

#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    crate::node_graph::nodes::get_node_types()
}

/// Compile a graph against the eval engine, install it as the active scene for
/// live visualization, and return the editor's `RunResult`.
///
/// The evaluation itself lives in [`evaluate_graph`], which produces strictly more
/// than `RunResult` can carry (the time grid, primitive ids/positions, channel
/// names, hashes); this command projects it down to the wire shape the graph
/// editor has always consumed.
#[tauri::command]
pub async fn run_graph(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    _stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    graph: Graph,
    context: GraphContext,
    // False for param-only edits (live slider drags): mel specs depend only on
    // audio wiring + span, so their FFT + heavy payload can be skipped.
    include_mel_specs: Option<bool>,
) -> Result<RunResult, String> {
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    let evaluation = evaluate_graph(
        &db.0,
        &resource_root,
        &fft_service,
        &graph,
        &context,
        EvaluateOptions {
            include_mel: include_mel_specs.unwrap_or(true),
        },
    )
    .await?;

    // Drive live visualization from the run. An empty graph clears the scene
    // instead of installing a plan that outputs nothing.
    render_engine.set_active_scene((!graph.nodes.is_empty()).then(|| {
        Scene::new(vec![CompiledAnnotation {
            plan: evaluation.plan.clone(),
            span: evaluation.span,
            z_index: 0,
            blend_mode: crate::models::node_graph::BlendMode::Replace,
        }])
    }));

    Ok(evaluation.into_run_result())
}

/// Precompute a looping pattern preview as a sequence of UniverseState frames.
/// Used by the hover-card preview to play back smoothly without per-frame IPC.
#[tauri::command]
pub async fn preview_pattern(
    app: AppHandle,
    db: State<'_, Db>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, FftService>,
    pattern_id: String,
    track_id: String,
    venue_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
    fps: f32,
) -> Result<Vec<UniverseState>, String> {
    use crate::compositor::fetch_pattern_graph;

    let duration = end_time - start_time;
    if duration <= 0.0 {
        return Err("Preview duration must be positive".into());
    }

    let fps = fps.clamp(10.0, 30.0);
    let frame_count = ((duration * fps).ceil() as usize).clamp(1, 256);

    // 1. Fetch pattern graph
    let graph_json = fetch_pattern_graph(&db.0, &pattern_id).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;
    if graph.nodes.is_empty() {
        return Err("Pattern produced no output".into());
    }

    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    // 2. Args from the pattern's defaults, with Selection forced to "all" — the
    //    hover card has no venue selection context.
    let args = merge_arg_values(&graph, None, true);

    let span = (start_time, end_time);
    let (ctx, primitive_ids) = build_resident_context(
        &db.0,
        &db.0,
        &resource_root,
        &track_id,
        &venue_id,
        &graph.nodes,
        &graph.edges,
        &args,
        span,
        beat_grid,
    )
    .await;

    let plan = compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids)
        .map_err(|e| format!("Failed to compile pattern: {:?}", e))?;

    // 3. Evaluate over the [start, end] grid at `fps`.
    let dt = duration / frame_count as f32;
    let times: Vec<f32> = (0..frame_count)
        .map(|i| start_time + i as f32 * dt)
        .collect();
    let mut arena = Arena::default();
    Ok(crate::eval::eval(&plan, &times, &mut arena))
}
