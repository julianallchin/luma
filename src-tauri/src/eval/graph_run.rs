//! One graph run, fully described.
//!
//! [`evaluate_graph`] is the single place a `(graph, context)` pair becomes a
//! compiled [`Plan`] plus everything the run produced: the dense preview taps and
//! **the time grid they were sampled on**, the resolved primitive ids and their
//! world positions, the span, the mel spectrograms, and stable fingerprints of
//! the graph / args / selection.
//!
//! Historically this all lived inline in `run_graph`, which then dropped most of
//! it: the `times` grid was computed and thrown away (the frontend re-derived it
//! from the span), and `primitive_ids` / `positions` were moved into the `Scene`
//! with the `Plan` and became unreachable. Both are load-bearing for anything that
//! wants to *reason* about a run rather than draw it — the agent's code executor
//! above all — so the evaluation now owns an `Arc<Plan>` and the command projects
//! from it instead of consuming it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::storage::StorageRoot;

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::audio::{mel_center_frequencies, FftService};
use crate::canonical_json::to_string as canonical_json;
use crate::eval::compile::compile_pattern;
use crate::eval::context::build_resident_context;
use crate::eval::{Arena, Plan, ResidentContext, ViewTap};
use crate::models::node_graph::{BeatGrid, Graph, GraphContext, RunResult, Signal};
use crate::models::tracks::MelSpec;
use crate::models::universe::UniverseState;

/// Per-view float budget for the dense grid (≈100 KB of JSON per view once
/// serialized). Runs stream at ~20 Hz during param drags and cost scales with
/// `n*t*c`, so wide taps get a coarser time axis.
const VIEW_FLOAT_BUDGET: usize = 12_288;

/// A view tap plus what its channel axis means. `Signal` stays exactly the wire
/// type the editor renders; the labels ride alongside so a consumer that isn't a
/// plotter (the Python executor) can name `c` instead of guessing from its width.
#[derive(Clone, Debug)]
pub struct SemanticSignal {
    pub signal: Signal,
    /// One name per channel; `channels.len() == signal.c`.
    pub channels: Vec<String>,
}

/// A mel spectrogram plus the coordinate vectors `MelSpec` throws away.
#[derive(Clone, Debug)]
pub struct SemanticMel {
    pub mel: MelSpec,
    /// Center frequency of each mel row, ascending; `len == mel.height`.
    pub frequencies_hz: Vec<f32>,
    /// Absolute track seconds at each column center; `len == mel.width`.
    pub times_s: Vec<f32>,
}

/// Everything one `(graph, context)` evaluation produced.
#[derive(Clone, Debug)]
pub struct GraphEvaluation {
    /// The compiled program, shared with the live scene rather than moved into it.
    /// Also the only handle on the resident context (audio, beats, stems).
    pub plan: Arc<Plan>,
    /// `view_*` node id -> tap, sampled on [`GraphEvaluation::times_s`].
    pub views: HashMap<String, SemanticSignal>,
    /// `mel_spec_viewer` node id -> spectrogram. `None` means mel specs were
    /// **not computed** (the param-drag path skips them); `Some(empty)` means the
    /// graph has no viewer nodes. A consumer must be able to tell "unavailable"
    /// from "nothing to show".
    pub mel_views: Option<HashMap<String, SemanticMel>>,
    /// The absolute-second grid `views` were sampled on. Computed unconditionally,
    /// even when the graph has no view nodes, so a caller can evaluate the plan
    /// itself on the same axis.
    pub times_s: Vec<f32>,
    pub primitive_ids: Vec<String>,
    /// World position per primitive, index-aligned with `primitive_ids`.
    pub positions: Vec<[f32; 3]>,
    pub span: (f32, f32),
    /// Fingerprints — see [`graph_hash`], [`arg_hash`], [`selection_hash`].
    pub graph_hash: String,
    pub arg_hash: String,
    pub selection_hash: String,
    pub track_id: String,
    pub venue_id: String,
    /// The graph at `span.0` **only** — a single frame, not a slice of the
    /// `times_s` axis. The editor's visualizer wants the span's first frame and
    /// evaluating one time sample is far cheaper than assembling a full
    /// `UniverseState` per dense-grid step. Do not read it as `times_s[0]`'s
    /// output unless you have checked they coincide (they do today, since the grid
    /// starts at `span.0`, but nothing enforces it).
    pub universe_state: Option<UniverseState>,
}

/// What to spend time on. Mel specs are the only optional piece — they are pure
/// FFT over audio the plan already holds, and depend on nothing but the audio
/// wiring and the span, so a param-only re-run can skip them entirely.
#[derive(Clone, Copy, Debug)]
pub struct EvaluateOptions {
    pub include_mel: bool,
}

impl Default for EvaluateOptions {
    fn default() -> Self {
        Self { include_mel: true }
    }
}

/// Compile and evaluate `graph` against `context`.
///
/// Takes plain handles rather than Tauri `State` so the headless harness and the
/// binding providers can call it. `pool` serves as both the local DB (tracks,
/// beats, onsets, roots) and the project DB (fixtures, groups) — they are the same
/// pool at every call site today.
pub async fn evaluate_graph(
    pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    fft: &FftService,
    graph: &Graph,
    context: &GraphContext,
    opts: EvaluateOptions,
) -> Result<GraphEvaluation, String> {
    let span = (context.start_time, context.end_time);
    let args = merge_arg_values(graph, context.arg_values.as_ref(), false);

    // An empty graph has nothing to select against and nothing to compile; skip
    // the DB round-trip rather than resolving "all" fixtures for a no-op plan.
    let plan = if graph.nodes.is_empty() {
        empty_plan(span)
    } else {
        let (ctx, primitive_ids) = build_resident_context(
            pool,
            pool,
            storage,
            resource_root,
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
                "[evaluate_graph] WARNING: selection resolved to 0 fixtures \
                 (venue_id={:?}) — output will be empty",
                context.venue_id
            );
        }

        compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids)
            .map_err(|e| format!("Failed to compile graph: {:?}", e))?
    };

    let mut arena = Arena::default();

    // One frame at the span start for the immediate visualizer snapshot.
    let universe_state = if graph.nodes.is_empty() {
        None
    } else {
        crate::eval::eval(&plan, &[context.start_time], &mut arena).pop()
    };

    // The dense grid is computed whether or not anything taps it — it is the run's
    // time axis, and a caller with no view nodes still wants to know it. Only the
    // evaluation is conditional.
    let times_s = view_time_grid(span, max_view_cols(&plan));
    let views = if plan.views.is_empty() {
        HashMap::new()
    } else {
        crate::eval::eval_views(&plan, &times_s, &mut arena)
            .into_iter()
            .map(|(node_id, signal)| {
                let channels = plan
                    .views
                    .iter()
                    .find(|(id, _)| *id == node_id)
                    .map(|(_, tap)| plan.view_channels(tap))
                    .unwrap_or_default();
                (node_id, SemanticSignal { signal, channels })
            })
            .collect()
    };

    let mel_views = opts
        .include_mel
        .then(|| compute_mel_specs(graph, &plan.ctx, fft));

    let primitive_ids = plan.primitive_ids.clone();
    let positions = plan.ctx.positions.clone();
    Ok(GraphEvaluation {
        graph_hash: graph_hash(graph),
        arg_hash: arg_hash(&args),
        selection_hash: selection_hash(&primitive_ids),
        views,
        mel_views,
        times_s,
        primitive_ids,
        positions,
        span,
        track_id: context.track_id.clone(),
        venue_id: context.venue_id.clone(),
        universe_state,
        plan: Arc::new(plan),
    })
}

impl GraphEvaluation {
    /// Project onto the graph editor's wire type. Lossy by design — `RunResult`
    /// predates this struct and carries neither the time axis nor the channel
    /// names; changing it would ripple through the frontend.
    pub fn into_run_result(self) -> RunResult {
        RunResult {
            views: self.views.into_iter().map(|(k, v)| (k, v.signal)).collect(),
            mel_specs: self
                .mel_views
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (k, v.mel))
                .collect(),
            color_views: HashMap::new(),
            universe_state: self.universe_state,
        }
    }
}

/// A compiled plan with no ops — what an empty graph evaluates to.
fn empty_plan(span: (f32, f32)) -> Plan {
    Plan {
        ops: Vec::new(),
        slots: Vec::new(),
        slot_channels: Vec::new(),
        n: 0,
        primitive_ids: Vec::new(),
        outputs: Default::default(),
        ctx: ResidentContext {
            span,
            ..Default::default()
        },
        prologue_baked: Vec::new(),
        views: Vec::new(),
    }
}

/// Widest `n*c` across the plan's taps (`Events` taps rasterize to `1`), which
/// sets how fine the shared time grid can be within the per-view float budget.
fn max_view_cols(plan: &Plan) -> usize {
    plan.views
        .iter()
        .map(|(_, tap)| match tap {
            ViewTap::Slot(slot) => {
                let spec = &plan.slots[*slot as usize];
                (spec.n.max(1) * spec.c.max(1)) as usize
            }
            ViewTap::Events(_) => 1,
        })
        .max()
        .unwrap_or(1)
}

/// The absolute-second sampling grid for a run: uniform over `span`, hitting both
/// endpoints exactly. The floor matches the view canvas at retina density (720
/// logical px × 2 dpr) so short spans don't render chunky; longer spans follow
/// 44 Hz up to a ceiling, and wide taps (`max_cols`) pull it back down so the
/// per-run payload stays bounded.
pub fn view_time_grid(span: (f32, f32), max_cols: usize) -> Vec<f32> {
    let duration = (span.1 - span.0).max(1e-3);
    let steps = ((duration * 44.0).ceil() as usize)
        .clamp(2048, 4096)
        .min((VIEW_FLOAT_BUDGET / max_cols.max(1)).max(1024));
    (0..steps)
        .map(|i| span.0 + duration * (i as f32 / (steps - 1) as f32))
        .collect()
}

/// The arg map a graph actually evaluates with: what the caller supplied, with
/// every arg it omitted backfilled from the pattern's declared default.
///
/// `force_selection_all` replaces Selection args with the `all` expression — what
/// the hover-card preview wants, since it has no venue selection context.
///
/// This is the map `arg_hash` fingerprints; the raw `context.arg_values` is not a
/// complete description of a run.
pub fn merge_arg_values(
    graph: &Graph,
    provided: Option<&HashMap<String, Value>>,
    force_selection_all: bool,
) -> HashMap<String, Value> {
    use crate::models::node_graph::PatternArgType;

    let mut args = provided.cloned().unwrap_or_default();
    for ad in &graph.args {
        if force_selection_all && matches!(ad.arg_type, PatternArgType::Selection) {
            args.insert(
                ad.id.clone(),
                serde_json::json!({ "expression": "all", "spatialReference": "global" }),
            );
        } else {
            args.entry(ad.id.clone())
                .or_insert_with(|| ad.default_value.clone());
        }
    }
    args
}

// ---------------------------------------------------------------------------
// Fingerprints
//
// All three are SHA-256 over a canonical JSON rendering (object keys sorted at
// every level), hex-encoded. Never `DefaultHasher` — that is explicitly not
// stable across Rust releases, and these values are meant to survive restarts.
// ---------------------------------------------------------------------------

/// Fingerprint of the graph's *meaning*: nodes (id, type, params) sorted by id,
/// edges sorted, and the arg declarations.
///
/// **Node positions are excluded.** The agent's `apply` re-runs auto-layout and
/// rewrites every `position_x`/`position_y`, so hashing them would invalidate a
/// cached run on a change that cannot affect output.
pub fn graph_hash(graph: &Graph) -> String {
    let mut nodes: Vec<Value> = graph
        .nodes
        .iter()
        .map(|n| {
            let mut v = serde_json::to_value(n).unwrap_or(Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.remove("positionX");
                obj.remove("positionY");
            }
            v
        })
        .collect();
    nodes.sort_by(|a, b| canonical_json(a).cmp(&canonical_json(b)));

    let mut edges: Vec<Value> = graph
        .edges
        .iter()
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    edges.sort_by(|a, b| canonical_json(a).cmp(&canonical_json(b)));

    let mut args: Vec<Value> = graph
        .args
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or(Value::Null))
        .collect();
    args.sort_by(|a, b| canonical_json(a).cmp(&canonical_json(b)));

    sha256_hex(&canonical_json(&serde_json::json!({
        "nodes": nodes,
        "edges": edges,
        "args": args,
    })))
}

/// Fingerprint of the **merged** arg map (see [`merge_arg_values`]) — two runs
/// that differ only in which defaults were sent explicitly hash the same.
pub fn arg_hash(args: &HashMap<String, Value>) -> String {
    let map: BTreeMap<&String, &Value> = args.iter().collect();
    sha256_hex(&canonical_json(
        &serde_json::to_value(map).unwrap_or(Value::Null),
    ))
}

/// Fingerprint of the *resolved* selection. The primitive-id vector is a stronger
/// key than the selection expression: it already folds in current venue group
/// membership, head expansion, and any preview override, and its order is the `n`
/// axis every tensor in the run is indexed by.
pub fn selection_hash(primitive_ids: &[String]) -> String {
    sha256_hex(&primitive_ids.join("\n"))
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

// ---------------------------------------------------------------------------
// Mel spectrograms
// ---------------------------------------------------------------------------

/// Spectrograms for the graph's `mel_spec_viewer` nodes, computed from the
/// resident audio cropped to the span. The viewer's `in` port is traced upstream
/// through pass-through audio nodes (filters) to its source: a `stem_splitter`
/// port selects that stem, anything else uses the full mix. (Filter nodes do not
/// transform the preview audio — the spectrogram shows the unfiltered source.)
fn compute_mel_specs(
    graph: &Graph,
    ctx: &ResidentContext,
    fft_service: &FftService,
) -> HashMap<String, SemanticMel> {
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

        // Columns aggregate the STFT frames of the crop in equal blocks, so column
        // `i` sits at the center of the `i`-th of `WIDTH` equal slices of the
        // cropped audio, in absolute track seconds.
        let (crop_s0, crop_s1) = (start as f32 / sr, end as f32 / sr);
        let times_s = (0..MEL_SPEC_WIDTH)
            .map(|i| crop_s0 + (crop_s1 - crop_s0) * ((i as f32 + 0.5) / MEL_SPEC_WIDTH as f32))
            .collect();

        out.insert(
            node.id.clone(),
            SemanticMel {
                mel: MelSpec {
                    width: MEL_SPEC_WIDTH,
                    height: MEL_SPEC_HEIGHT,
                    data,
                    beat_grid,
                },
                frequencies_hz: mel_center_frequencies(MEL_SPEC_HEIGHT, audio.sample_rate),
                times_s,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::node_graph::{Edge, NodeInstance, PatternArgDef, PatternArgType};

    fn node(id: &str, type_id: &str, params: &[(&str, Value)]) -> NodeInstance {
        NodeInstance {
            id: id.into(),
            type_id: type_id.into(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            position_x: None,
            position_y: None,
        }
    }
    fn edge(from: &str, fp: &str, to: &str, tp: &str) -> Edge {
        Edge {
            id: format!("{from}:{fp}->{to}:{tp}"),
            from_node: from.into(),
            from_port: fp.into(),
            to_node: to.into(),
            to_port: tp.into(),
        }
    }

    /// A colored path (`sample_palette` -> `view_signal`) and a scalar path
    /// (`ramp_between` -> `view_signal`) tapped from the same graph: the color tap
    /// must report r/g/b and the value tap a single `value` channel.
    fn color_and_value_graph() -> Graph {
        Graph {
            nodes: vec![
                node("pattern_args", "pattern_args", &[]),
                node("s0", "scalar", &[("value", Value::from(0.0))]),
                node("s1", "scalar", &[("value", Value::from(1.0))]),
                node("ramp", "ramp_between", &[]),
                node("sp", "sample_palette", &[]),
                node("view_color", "view_signal", &[]),
                node("view_value", "view_signal", &[]),
            ],
            edges: vec![
                edge("s0", "out", "ramp", "start"),
                edge("s1", "out", "ramp", "end"),
                edge("ramp", "out", "sp", "u"),
                edge("pattern_args", "gradient", "sp", "stops"),
                edge("sp", "out", "view_color", "in"),
                edge("ramp", "out", "view_value", "in"),
            ],
            args: vec![PatternArgDef {
                id: "gradient".into(),
                name: "Gradient".into(),
                arg_type: PatternArgType::Gradient,
                default_value: serde_json::json!({
                    "stops": [
                        { "t": 0.0, "color": "#000000" },
                        { "t": 1.0, "color": "#ffffff" },
                    ]
                }),
            }],
        }
    }

    fn compile(graph: &Graph, primitive_ids: Vec<String>, span: (f32, f32)) -> Plan {
        let args = merge_arg_values(graph, None, false);
        let ctx = ResidentContext {
            span,
            positions: vec![[0.0, 0.0, 0.0]; primitive_ids.len()],
            ..Default::default()
        };
        compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids).expect("compile")
    }

    /// The grid is uniform and lands exactly on both span endpoints — the whole
    /// point of publishing it instead of letting consumers re-derive it.
    #[test]
    fn time_grid_is_uniform_over_the_span() {
        let span = (12.5f32, 20.0f32);
        let times = view_time_grid(span, 1);
        assert!(times.len() >= 2048);
        assert_eq!(times[0], span.0);
        assert_eq!(*times.last().unwrap(), span.1);

        let steps = times.len();
        let dur = span.1 - span.0;
        for (i, t) in times.iter().enumerate() {
            let expect = span.0 + dur * (i as f32 / (steps - 1) as f32);
            assert!((t - expect).abs() < 1e-4, "step {i}: {t} vs {expect}");
        }
    }

    /// Wide taps pull the grid back to the float budget, but never below the
    /// canvas floor, and the endpoints still hold.
    #[test]
    fn time_grid_narrows_for_wide_taps() {
        let span = (0.0f32, 60.0f32);
        let narrow = view_time_grid(span, 1);
        let wide = view_time_grid(span, 64);
        assert!(wide.len() < narrow.len());
        assert_eq!(wide.len(), 1024);
        assert_eq!(*wide.last().unwrap(), span.1);
    }

    /// `times_s` exists even with nothing tapping it — a caller with no view
    /// nodes still needs the run's time axis.
    #[test]
    fn time_grid_is_computed_without_views() {
        let graph = Graph {
            nodes: vec![node("s0", "scalar", &[("value", Value::from(1.0))])],
            edges: vec![],
            args: vec![],
        };
        let plan = compile(&graph, vec!["p0".into()], (0.0, 4.0));
        assert!(plan.views.is_empty());
        let times = view_time_grid(plan.ctx.span, max_view_cols(&plan));
        assert!(times.len() >= 2048);
        assert_eq!(times[0], 0.0);
        assert_eq!(*times.last().unwrap(), 4.0);
    }

    /// The n axis is one thing: the plan's ids, the published ids, and the
    /// positions vector all agree.
    #[test]
    fn primitive_ids_and_positions_are_aligned() {
        let graph = color_and_value_graph();
        let ids: Vec<String> = (0..5).map(|i| format!("fix-{i}:0")).collect();
        let plan = compile(&graph, ids.clone(), (0.0, 4.0));

        assert_eq!(plan.primitive_ids, ids);
        assert_eq!(plan.ctx.positions.len(), plan.primitive_ids.len());
        assert_eq!(plan.n as usize, plan.primitive_ids.len());
    }

    /// Channel labels reach the view taps: a color path reports r/g/b, a scalar
    /// path reports a single `value`.
    #[test]
    fn view_taps_carry_channel_labels() {
        let graph = color_and_value_graph();
        let plan = compile(&graph, vec!["p0".into(), "p1".into()], (0.0, 4.0));

        let channels = |id: &str| {
            plan.views
                .iter()
                .find(|(n, _)| n == id)
                .map(|(_, tap)| plan.view_channels(tap))
                .unwrap_or_else(|| panic!("no view tap for {id}"))
        };
        assert_eq!(channels("view_color"), vec!["r", "g", "b"]);
        assert_eq!(channels("view_value"), vec!["value"]);

        // …and every slot is labelled to its own width.
        for (spec, labels) in plan.slots.iter().zip(&plan.slot_channels) {
            assert_eq!(spec.c as usize, labels.len());
        }
    }

    /// An `Events` tap has no slot; it still names its single channel.
    #[test]
    fn event_taps_are_labelled() {
        let plan = compile(&color_and_value_graph(), vec!["p0".into()], (0.0, 1.0));
        assert_eq!(
            plan.view_channels(&ViewTap::Events(vec![0.5])),
            vec!["events"]
        );
    }

    /// Reordering nodes and edges, and moving every node on the canvas, must not
    /// change the graph fingerprint. Changing a param must.
    #[test]
    fn graph_hash_ignores_order_and_position() {
        let base = color_and_value_graph();
        let h = graph_hash(&base);

        let mut shuffled = base.clone();
        shuffled.nodes.reverse();
        shuffled.edges.reverse();
        for (i, n) in shuffled.nodes.iter_mut().enumerate() {
            n.position_x = Some(i as f64 * 137.0);
            n.position_y = Some(i as f64 * -42.5);
        }
        assert_eq!(graph_hash(&shuffled), h);

        let mut retuned = base.clone();
        retuned.nodes[1]
            .params
            .insert("value".into(), Value::from(0.25));
        assert_ne!(graph_hash(&retuned), h);

        let mut rewired = base.clone();
        rewired.edges.pop();
        assert_ne!(graph_hash(&rewired), h);
    }

    /// The arg fingerprint is over the merged map, so sending a default
    /// explicitly and omitting it are the same run.
    #[test]
    fn arg_hash_is_over_the_merged_map() {
        let graph = color_and_value_graph();
        let backfilled = merge_arg_values(&graph, None, false);
        assert!(backfilled.contains_key("gradient"));

        let explicit: HashMap<String, Value> =
            [("gradient".to_string(), graph.args[0].default_value.clone())]
                .into_iter()
                .collect();
        let merged_explicit = merge_arg_values(&graph, Some(&explicit), false);

        assert_eq!(arg_hash(&backfilled), arg_hash(&merged_explicit));

        let overridden: HashMap<String, Value> = [(
            "gradient".to_string(),
            serde_json::json!({ "stops": [{ "t": 0.0, "color": "#ff0000" }] }),
        )]
        .into_iter()
        .collect();
        assert_ne!(
            arg_hash(&merge_arg_values(&graph, Some(&overridden), false)),
            arg_hash(&backfilled)
        );
    }

    /// Selection args are forced to `all` only when asked (the preview path).
    #[test]
    fn merge_can_force_selection_all() {
        let graph = Graph {
            nodes: vec![],
            edges: vec![],
            args: vec![PatternArgDef {
                id: "selection".into(),
                name: "Selection".into(),
                arg_type: PatternArgType::Selection,
                default_value: serde_json::json!({ "expression": "front_wash" }),
            }],
        };
        let kept = merge_arg_values(&graph, None, false);
        assert_eq!(kept["selection"]["expression"], "front_wash");

        let forced = merge_arg_values(&graph, None, true);
        assert_eq!(forced["selection"]["expression"], "all");
    }

    /// The selection fingerprint follows the resolved ids, order included.
    #[test]
    fn selection_hash_follows_resolved_ids() {
        let a = vec!["f1:0".to_string(), "f1:1".to_string()];
        let mut reordered = a.clone();
        reordered.reverse();
        assert_eq!(selection_hash(&a), selection_hash(&a.clone()));
        assert_ne!(selection_hash(&a), selection_hash(&reordered));
        assert_ne!(selection_hash(&a), selection_hash(&a[..1]));
    }

    /// Canonical JSON is insensitive to key insertion order at every level.
    #[test]
    fn canonical_json_sorts_keys() {
        let a: Value =
            serde_json::from_str(r#"{"b":1,"a":{"z":2,"y":[3,{"q":4,"p":5}]}}"#).unwrap();
        let b: Value =
            serde_json::from_str(r#"{"a":{"y":[3,{"p":5,"q":4}],"z":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"y":[3,{"p":5,"q":4}],"z":2},"b":1}"#
        );
    }
}
