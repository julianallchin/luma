use tauri::{AppHandle, State};

use std::collections::HashMap;

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::eval::context::build_resident_context;
use crate::eval::{compile::compile_pattern, Arena, CompiledAnnotation, Scene};
use crate::models::node_graph::{BeatGrid, Graph, GraphContext, NodeTypeDef, RunResult};
use crate::models::universe::UniverseState;
use crate::render_engine::RenderEngine;

#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    crate::node_graph::nodes::get_node_types()
}

/// Compile a graph against the new eval engine, install it as the active scene
/// for live visualization, and return a `RunResult` whose `universe_state` is the
/// graph evaluated at `context.start_time`.
///
/// Views / mel_specs / color_views are returned EMPTY — intermediate view-node
/// exposure is a separate follow-up.
// TODO(eval-views): expose view-node slot data once the eval engine surfaces
// intermediate signals.
#[tauri::command]
pub async fn run_graph(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, FftService>,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    if graph.nodes.is_empty() {
        render_engine.set_active_scene(None);
        return Ok(RunResult {
            views: HashMap::new(),
            mel_specs: HashMap::new(),
            color_views: HashMap::new(),
            universe_state: None,
        });
    }

    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    let span = (context.start_time, context.end_time);
    let (ctx, primitive_ids) = build_resident_context(
        &db.0,
        &db.0,
        &resource_root,
        &context.track_id,
        &context.venue_id,
        &graph.nodes,
        &graph.edges,
        span,
        context.beat_grid.clone(),
    )
    .await;

    let mut args: HashMap<String, serde_json::Value> =
        context.arg_values.clone().unwrap_or_default();
    // Fill any args the editor didn't send from the pattern's defaults.
    for ad in &graph.args {
        args.entry(ad.id.clone())
            .or_insert_with(|| ad.default_value.clone());
    }

    let plan = compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids)
        .map_err(|e| format!("Failed to compile graph: {:?}", e))?;

    // Evaluate one frame at the span start for the immediate visualizer snapshot.
    let mut arena = Arena::default();
    let universe_state = crate::eval::eval(&plan, &[context.start_time], &mut arena).pop();

    // Install as the active scene so the render loop drives live visualization.
    let scene = Scene::new(vec![CompiledAnnotation {
        plan: std::sync::Arc::new(plan),
        span,
        z_index: 0,
        blend_mode: crate::models::node_graph::BlendMode::Replace,
    }]);
    render_engine.set_active_scene(Some(scene));

    Ok(RunResult {
        views: HashMap::new(),
        mel_specs: HashMap::new(),
        color_views: HashMap::new(),
        universe_state,
    })
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
    let frame_count = ((duration * fps).ceil() as usize).min(256).max(1);

    // 1. Fetch pattern graph
    let graph_json = fetch_pattern_graph(&db.0, &pattern_id).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;
    if graph.nodes.is_empty() {
        return Err("Pattern produced no output".into());
    }

    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    // 2. Build arg_values: force all Selection args to "all".
    let args: HashMap<String, serde_json::Value> = graph
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

    let span = (start_time, end_time);
    let (ctx, primitive_ids) = build_resident_context(
        &db.0,
        &db.0,
        &resource_root,
        &track_id,
        &venue_id,
        &graph.nodes,
        &graph.edges,
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
