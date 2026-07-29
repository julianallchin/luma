use tauri::{AppHandle, State};

use std::collections::HashMap;

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::eval::context::build_resident_context;
use crate::eval::{compile::compile_pattern, Arena, CompiledAnnotation, Scene};
use crate::models::node_graph::{BeatGrid, Graph, GraphContext, NodeTypeDef, RunResult};
use crate::models::universe::UniverseState;
use crate::render_engine::RenderEngine;
use crate::storage::StorageRoot;

#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    crate::node_graph::nodes::get_node_types()
}

/// Compile a graph against the new eval engine, install it as the active scene
/// for live visualization, and return a `RunResult` whose `universe_state` is the
/// graph evaluated at `context.start_time`.
///
/// `views` carries the `view_signal` / `view_uv` / `view_events` preview taps
/// (the plan evaluated over a dense grid of the span) and `mel_specs` the
/// `mel_spec_viewer` spectrograms, so the graph editor's viewer nodes render.
/// `color_views` stays empty (the editor ignores it).
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

    let mut args: HashMap<String, serde_json::Value> =
        context.arg_values.clone().unwrap_or_default();
    // Fill any args the editor didn't send from the pattern's defaults.
    for ad in &graph.args {
        args.entry(ad.id.clone())
            .or_insert_with(|| ad.default_value.clone());
    }

    let span = (context.start_time, context.end_time);
    let (ctx, primitive_ids) = build_resident_context(
        &db.0,
        &db.0,
        &StorageRoot::from_app(&app)?,
        &resource_root,
        &context.track_id,
        &context.venue_id,
        &graph.nodes,
        &graph.edges,
        &args,
        span,
        context.beat_grid.clone(),
    )
    .await;

    if primitive_ids.is_empty() {
        eprintln!(
            "[run_graph] WARNING: selection resolved to 0 fixtures \
             (venue_id={:?}) — output will be empty",
            context.venue_id
        );
    }

    let plan = compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids)
        .map_err(|e| format!("Failed to compile graph: {:?}", e))?;

    // Evaluate one frame at the span start for the immediate visualizer snapshot.
    let mut arena = Arena::default();
    let universe_state = crate::eval::eval(&plan, &[context.start_time], &mut arena).pop();

    // View-node previews: evaluate over a dense grid of the span and extract
    // the taps. The floor matches the view canvas at retina density (720 logical
    // px × 2 dpr) so short spans don't render chunky; long spans follow 44 Hz.
    // Wide taps (many primitives/channels) get a coarser grid so the per-run
    // payload stays bounded — these runs stream at ~20 Hz during param drags,
    // and serialize/parse cost scales with n*t*c.
    let views = if plan.views.is_empty() {
        HashMap::new()
    } else {
        const VIEW_FLOAT_BUDGET: usize = 12_288; // per view, ≈100 KB JSON
        let max_cols = plan
            .views
            .iter()
            .map(|(_, tap)| match tap {
                crate::eval::ViewTap::Slot(slot) => {
                    let spec = &plan.slots[*slot as usize];
                    (spec.n.max(1) * spec.c.max(1)) as usize
                }
                crate::eval::ViewTap::Events(_) => 1,
            })
            .max()
            .unwrap_or(1);
        let duration = (context.end_time - context.start_time).max(1e-3);
        let steps = ((duration * 44.0).ceil() as usize)
            .clamp(2048, 4096)
            .min((VIEW_FLOAT_BUDGET / max_cols).max(1024));
        let times: Vec<f32> = (0..steps)
            .map(|i| context.start_time + duration * (i as f32 / (steps - 1) as f32))
            .collect();
        crate::eval::eval_views(&plan, &times, &mut arena)
    };

    let mel_specs = if include_mel_specs.unwrap_or(true) {
        compute_mel_specs(&graph, &plan.ctx, &fft_service)
    } else {
        HashMap::new()
    };

    // Install as the active scene so the render loop drives live visualization.
    let scene = Scene::new(vec![CompiledAnnotation {
        plan: std::sync::Arc::new(plan),
        span,
        z_index: 0,
        blend_mode: crate::models::node_graph::BlendMode::Replace,
    }]);
    render_engine.set_active_scene(Some(scene));

    Ok(RunResult {
        views,
        mel_specs,
        color_views: HashMap::new(),
        universe_state,
    })
}

/// Spectrograms for the graph's `mel_spec_viewer` nodes, computed from the
/// resident audio cropped to the span. The viewer's `in` port is traced upstream
/// through pass-through audio nodes (filters) to its source: a `stem_splitter`
/// port selects that stem, anything else uses the full mix. (Filter nodes do not
/// transform the preview audio — the spectrogram shows the unfiltered source.)
fn compute_mel_specs(
    graph: &Graph,
    ctx: &crate::eval::ResidentContext,
    fft_service: &FftService,
) -> HashMap<String, crate::models::tracks::MelSpec> {
    use crate::audio::{generate_melspec, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH};

    let mut out = HashMap::new();
    let by_id: HashMap<&str, &crate::models::node_graph::NodeInstance> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let edge_to = |node: &str, port: &str| {
        graph
            .edges
            .iter()
            .find(|e| e.to_node == node && e.to_port == port)
    };

    for node in graph
        .nodes
        .iter()
        .filter(|n| n.type_id == "mel_spec_viewer")
    {
        // Trace `in` upstream through filter pass-throughs to the audio source.
        let mut stem: Option<String> = None;
        let mut edge = edge_to(&node.id, "in");
        while let Some(e) = edge {
            let Some(src) = by_id.get(e.from_node.as_str()) else {
                break;
            };
            match src.type_id.as_str() {
                "lowpass_filter" | "highpass_filter" => edge = edge_to(&src.id, "audio_in"),
                "stem_splitter" => {
                    stem = e.from_port.strip_suffix("_out").map(str::to_string);
                    break;
                }
                _ => break,
            }
        }

        let audio = match &stem {
            Some(name) => ctx.stems.get(name),
            None => ctx.audio.as_ref(),
        };
        let Some(audio) = audio else { continue };
        if audio.samples.is_empty() || audio.sample_rate == 0 {
            continue;
        }

        // Crop the resident full-track audio to the annotation span.
        let (s0, s1) = ctx.span;
        let sr = audio.sample_rate as f32;
        let start = ((s0.max(0.0) * sr) as usize).min(audio.samples.len());
        let end = ((s1.max(0.0) * sr).ceil() as usize).clamp(start, audio.samples.len());
        if start >= end {
            continue;
        }

        let data = generate_melspec(
            fft_service,
            &audio.samples[start..end],
            audio.sample_rate,
            MEL_SPEC_WIDTH,
            MEL_SPEC_HEIGHT,
        );

        // Beat grid shifted relative to the crop, when the viewer has one wired.
        let beat_grid = edge_to(&node.id, "grid")
            .and_then(|_| ctx.beat_grid.as_ref())
            .map(|g| beat_grid_relative_to_span(g, s0, s1));

        out.insert(
            node.id.clone(),
            crate::models::tracks::MelSpec {
                width: MEL_SPEC_WIDTH,
                height: MEL_SPEC_HEIGHT,
                data,
                beat_grid,
            },
        );
    }
    out
}

/// Shift a beat grid into span-relative time (keep beats inside `[start, end]`,
/// re-zeroed at `start`) — the frame of the span-cropped preview audio.
fn beat_grid_relative_to_span(grid: &BeatGrid, start: f32, end: f32) -> BeatGrid {
    let keep = |ts: &[f32]| -> Vec<f32> {
        ts.iter()
            .copied()
            .filter(|t| *t >= start && *t <= end)
            .map(|t| t - start)
            .collect()
    };
    BeatGrid {
        beats: keep(&grid.beats),
        downbeats: keep(&grid.downbeats),
        bpm: grid.bpm,
        downbeat_offset: grid.downbeat_offset - start,
        beats_per_bar: grid.beats_per_bar,
    }
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
        &StorageRoot::from_app(&app)?,
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
