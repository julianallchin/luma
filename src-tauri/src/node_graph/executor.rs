use crate::audio::StemCache;
use crate::database::Db;
pub use crate::models::node_graph::*;
use crate::node_graph::state::ExecutionState;
use crate::node_graph::{nodes, NodeExecutionContext};
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde_json;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, State};

static RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

// Graph execution returns preview data (channels, mel specs, series, colors).
pub async fn run_graph(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, crate::render_engine::RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, crate::audio::FftService>,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    let project_pool = Some(&db.0);

    // Resolve resource path for fixtures
    let final_path = crate::services::fixtures::resolve_fixtures_root(&app).ok();

    let (result, layer) = run_graph_internal(
        &db.0,
        project_pool,
        &stem_cache,
        &fft_service,
        final_path,
        graph,
        context,
        GraphExecutionConfig {
            compute_visualizations: true,
            log_summary: true,
            log_primitives: false,
            shared_audio: None,
        },
    )
    .await?;

    // Push the calculated layer to the render engine for real-time visualization
    render_engine.set_active_layer(layer);

    Ok(result)
}

#[derive(Clone)]
pub struct SharedAudioContext {
    pub track_id: i64,
    pub track_hash: String,
    pub samples: Arc<Vec<f32>>,
    pub sample_rate: u32,
}

#[derive(Clone)]
pub struct GraphExecutionConfig {
    pub compute_visualizations: bool,
    pub log_summary: bool,
    pub log_primitives: bool,
    pub shared_audio: Option<SharedAudioContext>,
}

impl Default for GraphExecutionConfig {
    fn default() -> Self {
        Self {
            compute_visualizations: true,
            log_summary: true,
            log_primitives: false,
            shared_audio: None,
        }
    }
}

pub async fn run_graph_internal(
    pool: &SqlitePool,
    project_pool: Option<&SqlitePool>,
    stem_cache: &StemCache,
    fft_service: &crate::audio::FftService,
    resource_path_root: Option<PathBuf>,
    graph: Graph,
    context: GraphContext,
    config: GraphExecutionConfig,
) -> Result<(RunResult, Option<LayerTimeSeries>), String> {
    let run_id = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_start = Instant::now();

    if config.log_summary {
        println!("[run_graph #{run_id}] start nodes={}", graph.nodes.len());
    }

    if graph.nodes.is_empty() {
        return Ok((
            RunResult {
                views: HashMap::new(),
                mel_specs: HashMap::new(),
                color_views: HashMap::new(),
                universe_state: None,
            },
            None,
        ));
    }

    let arg_values: HashMap<String, serde_json::Value> =
        context.arg_values.clone().unwrap_or_default();

    let nodes_by_id: HashMap<&str, &NodeInstance> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut dependency_graph: DiGraph<&str, ()> = DiGraph::new();
    let mut node_indices = HashMap::new();

    for node in &graph.nodes {
        let idx = dependency_graph.add_node(node.id.as_str());
        node_indices.insert(node.id.as_str(), idx);
    }

    for edge in &graph.edges {
        let Some(&from_idx) = node_indices.get(edge.from_node.as_str()) else {
            return Err(format!("Unknown from_node '{}' in edge", edge.from_node));
        };
        let Some(&to_idx) = node_indices.get(edge.to_node.as_str()) else {
            return Err(format!("Unknown to_node '{}' in edge", edge.to_node));
        };
        dependency_graph.add_edge(from_idx, to_idx, ());
    }

    let sorted = toposort(&dependency_graph, None)
        .map_err(|_| "Graph has a cycle. Execution aborted.".to_string())?;

    let mut incoming_edges: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        incoming_edges
            .entry(edge.to_node.as_str())
            .or_default()
            .push(edge);
    }

    let mut state = ExecutionState::new();

    let loaded_context = crate::node_graph::context::load_context(
        pool,
        &context,
        config.shared_audio.as_ref(),
        &graph.nodes,
    )
    .await?;
    let context_load_ms = loaded_context.load_ms;
    let context_beat_grid = loaded_context.beat_grid.clone();
    let node_context = NodeExecutionContext {
        incoming_edges: &incoming_edges,
        nodes_by_id: &nodes_by_id,
        pool,
        project_pool,
        resource_path_root: resource_path_root.as_ref(),
        fft_service,
        stem_cache,
        graph_context: &context,
        arg_defs: &graph.args,
        arg_values: &arg_values,
        config: &config,
        context_audio_buffer: loaded_context.audio_buffer.as_ref(),
        context_beat_grid: context_beat_grid.as_ref(),
        compute_visualizations: config.compute_visualizations,
    };

    let nodes_exec_start = Instant::now();
    for node_idx in sorted {
        let node_id = dependency_graph[node_idx];
        let node = nodes_by_id
            .get(node_id)
            .copied()
            .ok_or_else(|| format!("Node '{}' not found during execution", node_id))?;

        let node_start = Instant::now();

        nodes::run_node(node, &node_context, &mut state).await?;

        let node_ms = node_start.elapsed().as_secs_f64() * 1000.0;
        state.record_timing(node.id.clone(), node.type_id.clone(), node_ms);
    }
    let nodes_exec_ms = nodes_exec_start.elapsed().as_secs_f64() * 1000.0;

    // Merge all Apply outputs into a single LayerTimeSeries
    let merge_start = Instant::now();
    let merged_layer = if !state.apply_outputs.is_empty() {
        let mut merged_primitives: HashMap<String, PrimitiveTimeSeries> = HashMap::new();

        for layer in state.apply_outputs.drain(..) {
            for prim in layer.primitives {
                let entry = merged_primitives
                    .entry(prim.primitive_id.clone())
                    .or_insert_with(|| PrimitiveTimeSeries {
                        primitive_id: prim.primitive_id.clone(),
                        color: None,
                        dimmer: None,
                        position: None,
                        strobe: None,
                        speed: None,
                    });

                // Simple merge (last write wins or union) - TODO: Conflict detection
                if prim.color.is_some() {
                    entry.color = prim.color;
                }
                if prim.dimmer.is_some() {
                    entry.dimmer = prim.dimmer;
                }
                if prim.position.is_some() {
                    entry.position = prim.position;
                }
                if prim.strobe.is_some() {
                    entry.strobe = prim.strobe;
                }
                if prim.speed.is_some() {
                    entry.speed = prim.speed;
                }
            }
        }

        Some(LayerTimeSeries {
            primitives: merged_primitives.into_values().collect(),
        })
    } else {
        None
    };
    let merge_ms = merge_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = run_start.elapsed().as_secs_f64() * 1000.0;

    if let Some(l) = &merged_layer {
        if config.log_summary {
            println!(
                "[run_graph #{run_id}] done primitives={} context_ms={:.2} node_exec_ms={:.2} merge_ms={:.2} total_ms={:.2}",
                l.primitives.len(),
                context_load_ms,
                nodes_exec_ms,
                merge_ms,
                total_ms
            );
            let mut top_nodes = state.node_timings.clone();
            top_nodes.sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap_or(std::cmp::Ordering::Equal));
            let top_nodes: Vec<String> = top_nodes
                .into_iter()
                .take(5)
                .map(|n| format!("{} ({}) {:.2}ms", n.id, n.type_id, n.ms))
                .collect();
            if !top_nodes.is_empty() {
                println!(
                    "[run_graph #{run_id}] slowest_nodes: {}",
                    top_nodes.join(", ")
                );
            }
        }
        if config.log_primitives {
            for p in &l.primitives {
                println!("  - Primitive: {}", p.primitive_id);
            }
        }
    } else if config.log_summary {
        println!(
            "[run_graph #{run_id}] No layer generated (empty apply outputs) context_ms={:.2} node_exec_ms={:.2} merge_ms={:.2} total_ms={:.2}",
            context_load_ms, nodes_exec_ms, merge_ms, total_ms
        );
    }

    // Render one frame at start_time for preview (or 0.0 if start is negative/unset)
    // In a real app, the "Engine" loop would call render_frame(layer, t) continuously.
    // Here, we just snapshot the start state so the frontend visualizer sees something immediately.
    let universe_state = if let Some(layer) = &merged_layer {
        Some(crate::engine::render_frame(layer, context.start_time))
    } else {
        None
    };

    Ok((
        RunResult {
            views: state.view_results.clone(),
            mel_specs: state.mel_specs.clone(),
            color_views: state.color_views.clone(),
            universe_state,
        },
        merged_layer,
    ))
}

pub(crate) fn adsr_durations(
    span_sec: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
) -> (f32, f32, f32, f32) {
    let a_w = attack.clamp(0.0, 1.0);
    let d_w = decay.clamp(0.0, 1.0);
    let s_w = sustain.clamp(0.0, 1.0);
    let r_w = release.clamp(0.0, 1.0);
    let weight_sum = a_w + d_w + s_w + r_w;

    if weight_sum < 1e-6 {
        return (0.0, 0.0, 0.0, 0.0);
    }

    let scale = span_sec / weight_sum;
    (a_w * scale, d_w * scale, s_w * scale, r_w * scale)
}

pub(crate) fn calc_envelope(
    t: f32,
    peak: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    sustain_level: f32,
    a_curve: f32,
    d_curve: f32,
) -> f32 {
    if t < peak - attack {
        return 0.0;
    }

    // Attack: ramp 0 -> 1
    if t <= peak {
        if attack <= 0.0 {
            return 1.0;
        }
        let x = (t - (peak - attack)) / attack;
        return shape_curve(x, a_curve);
    }

    let decay_end = peak + decay;
    // Decay: 1 -> sustain_level
    if t <= decay_end {
        if decay <= 0.0 {
            return sustain_level;
        }
        let x = (t - peak) / decay;
        let shaped = shape_curve(1.0 - x, d_curve);
        return sustain_level + (1.0 - sustain_level) * shaped;
    }

    let sustain_end = decay_end + sustain;
    // Sustain: hold sustain_level
    if t <= sustain_end {
        return sustain_level;
    }

    let release_end = sustain_end + release;
    // Release: sustain_level -> 0
    if t <= release_end {
        if release <= 0.0 {
            return 0.0;
        }
        let x = (t - sustain_end) / release;
        let shaped = shape_curve(1.0 - x, d_curve);
        return sustain_level * shaped;
    }

    0.0
}

pub(crate) fn shape_curve(x: f32, curve: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if curve.abs() < 0.001 {
        x // Linear
    } else if curve > 0.0 {
        // Convex / Snappy (Power > 1)
        // Map 0..1 to Power 1..6
        let p = 1.0 + curve * 5.0;
        x.powf(p)
    } else {
        // Concave / Swell (Inverse Power)
        // y = 1 - (1-x)^p
        let p = 1.0 + (-curve) * 5.0;
        1.0 - (1.0 - x).powf(p)
    }
}
