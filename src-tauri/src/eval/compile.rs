//! Compiler: `Vec<NodeInstance>` (+ edges, args, track/venue context) -> [`Plan`].
//! See `docs/eval-ir.md`. This is the serial "brain" that the per-category op
//! kernels hang off. Built in milestones, each validated against `tests/golden/`:
//!
//!   C1  single-annotation pure lowering (gradient): selection -> n axis,
//!       shape/slot alloc, phase inference, edge wiring, apply_color -> HSV split.
//!   C2  seed hashing (noise/wander/random_mask), Stops inlining, FilterSelection.keep.
//!   C3  frozen reductions: dense-grid stat pass -> ctx.frozen[stat_idx]=[min,max].
//!   C4  audio: ResidentContext audio/stems population + STFT CSE injection.
//!   C5  multi-annotation segments + z-ordered Blend (the 302-annotation composite).
//!
//! Status: C1 lowers the `gradient` pattern end-to-end (scalar / ramp_between /
//! sample_palette / apply_color), validated frame-for-frame against its golden.
//! The remaining node types extend `lower_node` (a per-category fan-out, like the
//! kernels). Selection geometry / beats / audio are loaded by REUSING the legacy
//! loaders (wired in `build_resident_context`, C1b/C4) — not reimplemented.

use crate::eval::ops::color::ColorOp;
use crate::eval::ops::math::MathOp;
use crate::eval::{Op, OpKind, OutputBinding, Phase, Plan, ResidentContext, SlotId, SlotSpec};
use crate::models::node_graph::{Edge, NodeInstance, Stops};
use serde_json::Value;
use std::collections::HashMap;

/// Accumulates the lowered program as nodes are visited in topo order.
pub struct Lowerer {
    pub ops: Vec<Op>,
    pub slots: Vec<SlotSpec>,
    pub outputs: OutputBinding,
    /// node id + output port -> the slot carrying that value.
    pub node_slot: HashMap<(String, String), SlotId>,
    /// Primitive count (the `n` axis), fixed once selection is resolved.
    pub n: u32,
}

impl Lowerer {
    pub fn new(n: u32) -> Self {
        Self {
            ops: Vec::new(),
            slots: Vec::new(),
            outputs: OutputBinding::default(),
            node_slot: HashMap::new(),
            n,
        }
    }

    /// Reserve a fresh output slot of the given shape.
    pub fn alloc(&mut self, n: u32, c: u32) -> SlotId {
        let id = self.slots.len() as SlotId;
        self.slots.push(SlotSpec { n, c });
        id
    }
}

#[derive(Debug)]
pub enum CompileError {
    /// A node references an op the engine does not implement. Hard error (we do
    /// NOT inherit the legacy silent-skip — see docs/eval-ir.md §6).
    UnknownNode { id: String, type_id: String },
    /// A node's required input port has no resolved upstream slot.
    MissingInput { node: String, port: String },
    Graph(String),
}

/// Lower a single pattern graph against a resolved resident context (selection ->
/// `primitive_ids` gives `n`) and the pattern's arg values. C1 target; C5 wraps
/// this per-annotation with segmenting + blend.
pub fn compile_pattern(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, Value>,
    ctx: ResidentContext,
    primitive_ids: Vec<String>,
) -> Result<Plan, CompileError> {
    let n = primitive_ids.len() as u32;
    let mut low = Lowerer::new(n);
    let by_id: HashMap<&str, &NodeInstance> =
        nodes.iter().map(|nd| (nd.id.as_str(), nd)).collect();

    for node in topo_order(nodes, edges)? {
        lower_node(node, edges, args, &by_id, &mut low)?;
    }

    Ok(Plan {
        ops: low.ops,
        slots: low.slots,
        n,
        primitive_ids,
        outputs: low.outputs,
        ctx,
    })
}

/// The slot feeding `node`'s `port` (the upstream edge's source output slot).
fn input_slot(low: &Lowerer, edges: &[Edge], node: &str, port: &str) -> Option<SlotId> {
    let e = edges
        .iter()
        .find(|e| e.to_node == node && e.to_port == port)?;
    low.node_slot
        .get(&(e.from_node.clone(), e.from_port.clone()))
        .copied()
}

fn require_input(low: &Lowerer, edges: &[Edge], node: &str, port: &str) -> Result<SlotId, CompileError> {
    input_slot(low, edges, node, port).ok_or_else(|| CompileError::MissingInput {
        node: node.to_string(),
        port: port.to_string(),
    })
}

/// Resolve a `Stops` input that is wired from a `pattern_args` output port (the
/// compiler inlines the arg's Stops into the consuming op — there is no Stops
/// slot type in the IR).
fn resolve_stops_input(
    edges: &[Edge],
    args: &HashMap<String, Value>,
    by_id: &HashMap<&str, &NodeInstance>,
    node: &str,
    port: &str,
) -> Result<Stops, CompileError> {
    let e = edges
        .iter()
        .find(|e| e.to_node == node && e.to_port == port)
        .ok_or_else(|| CompileError::MissingInput {
            node: node.to_string(),
            port: port.to_string(),
        })?;
    // Expect the source to be a pattern_args node; the from_port is the arg id.
    if by_id.get(e.from_node.as_str()).map(|n| n.type_id.as_str()) == Some("pattern_args") {
        if let Some(v) = args.get(&e.from_port) {
            return Ok(parse_stops(v));
        }
    }
    Err(CompileError::MissingInput {
        node: node.to_string(),
        port: format!("{port} (stops arg)"),
    })
}

fn lower_node(
    node: &NodeInstance,
    edges: &[Edge],
    args: &HashMap<String, Value>,
    by_id: &HashMap<&str, &NodeInstance>,
    low: &mut Lowerer,
) -> Result<(), CompileError> {
    match node.type_id.as_str() {
        "scalar" => {
            let v = node
                .params
                .get("value")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0) as f32;
            let slot = low.alloc(1, 1);
            low.ops.push(Op {
                kind: OpKind::Math(MathOp::Scalar(v)),
                inputs: vec![],
                out: slot,
                phase: Phase::Prologue,
            });
            low.node_slot.insert((node.id.clone(), "out".into()), slot);
        }
        "ramp_between" => {
            let start = require_input(low, edges, &node.id, "start")?;
            let end = require_input(low, edges, &node.id, "end")?;
            let slot = low.alloc(1, 1);
            low.ops.push(Op {
                kind: OpKind::Math(MathOp::RampBetween),
                inputs: vec![start, end],
                out: slot,
                phase: Phase::Kernel,
            });
            low.node_slot.insert((node.id.clone(), "out".into()), slot);
        }
        "sample_palette" => {
            let u = require_input(low, edges, &node.id, "u")?;
            let stops = resolve_stops_input(edges, args, by_id, &node.id, "stops")?;
            let slot = low.alloc(1, 3); // n follows u (n=1 here), c=3 RGB
            low.ops.push(Op {
                kind: OpKind::Color(ColorOp::SamplePalette { stops }),
                inputs: vec![u],
                out: slot,
                phase: Phase::Kernel,
            });
            low.node_slot.insert((node.id.clone(), "out".into()), slot);
        }
        "apply_color" => {
            // Legacy apply_color HSV-splits: dimmer = max(r,g,b); color = (r,g,b)/v.
            let sig = require_input(low, edges, &node.id, "signal")?;
            let dim = low.alloc(1, 1);
            low.ops.push(Op {
                kind: OpKind::Color(ColorOp::HsvValue),
                inputs: vec![sig],
                out: dim,
                phase: Phase::Kernel,
            });
            let col = low.alloc(1, 3);
            low.ops.push(Op {
                kind: OpKind::Color(ColorOp::HsvNormalize),
                inputs: vec![sig],
                out: col,
                phase: Phase::Kernel,
            });
            low.outputs.dimmer = Some(dim);
            low.outputs.color = Some(col);
        }
        // pattern_args supplies arg values (resolved at consuming edges); audio_input
        // here is dead (no consumers) and view_signal is preview-only — all no-ops.
        "pattern_args" | "audio_input" | "view_signal" => {}
        _ => {
            return Err(CompileError::UnknownNode {
                id: node.id.clone(),
                type_id: node.type_id.clone(),
            })
        }
    }
    Ok(())
}

/// Kahn's algorithm, ids sorted for deterministic order.
fn topo_order<'a>(
    nodes: &'a [NodeInstance],
    edges: &[Edge],
) -> Result<Vec<&'a NodeInstance>, CompileError> {
    let by_id: HashMap<&str, &NodeInstance> =
        nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut indeg: HashMap<&str, usize> = nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        if by_id.contains_key(e.from_node.as_str()) && by_id.contains_key(e.to_node.as_str()) {
            *indeg.get_mut(e.to_node.as_str()).unwrap() += 1;
            adj.entry(e.from_node.as_str()).or_default().push(e.to_node.as_str());
        }
    }
    let mut queue: Vec<&str> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| *k)
        .collect();
    queue.sort_unstable();
    let mut order = Vec::with_capacity(nodes.len());
    let mut qi = 0;
    while qi < queue.len() {
        let id = queue[qi];
        qi += 1;
        order.push(by_id[id]);
        if let Some(succ) = adj.get(id) {
            let mut s = succ.clone();
            s.sort_unstable();
            for m in s {
                let d = indeg.get_mut(m).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push(m);
                }
            }
        }
    }
    if order.len() != nodes.len() {
        return Err(CompileError::Graph("cycle in graph".into()));
    }
    Ok(order)
}

/// Parse a gradient/palette arg (`{stops: [{color: "#rrggbb", t: f}]}`) into `Stops`.
fn parse_stops(v: &Value) -> Stops {
    let mut stops: Vec<(f32, [f32; 4])> = Vec::new();
    if let Some(arr) = v.get("stops").and_then(|s| s.as_array()) {
        for s in arr {
            let t = s.get("t").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            let rgba = s
                .get("color")
                .and_then(|c| c.as_str())
                .map(parse_hex)
                .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            stops.push((t, rgba));
        }
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Stops { stops }
}

fn parse_hex(h: &str) -> [f32; 4] {
    let h = h.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
    if h.len() >= 6 {
        [byte(0), byte(2), byte(4), if h.len() >= 8 { byte(6) } else { 1.0 }]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{eval, Arena};

    fn node(id: &str, type_id: &str, params: &[(&str, Value)]) -> NodeInstance {
        NodeInstance {
            id: id.into(),
            type_id: type_id.into(),
            params: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
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

    /// The first real end-to-end proof: compile the `gradient` pattern and match
    /// its captured golden frame-for-frame. Portable — reads only the committed
    /// fixture (no luma.db). gradient is spatially uniform, so dummy positions are
    /// fine; only n + primitive ids matter.
    #[test]
    fn gradient_matches_golden() {
        let path = format!(
            "{}/tests/golden/fixtures/gradient.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let golden: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("golden fixture"))
                .expect("valid json");

        let primitive_ids: Vec<String> = golden["frames"][0]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["primitive_id"].as_str().unwrap().to_string())
            .collect();
        let times: Vec<f32> = golden["sample_times"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_f64().unwrap() as f32)
            .collect();
        let span = (
            golden["start_time"].as_f64().unwrap() as f32,
            golden["end_time"].as_f64().unwrap() as f32,
        );
        let mut args = HashMap::new();
        args.insert("gradient".to_string(), golden["arg_values"]["gradient"].clone());

        // The gradient graph (from the DB), as literals:
        // scalar(0)->ramp.start, scalar(1)->ramp.end, ramp->sp.u,
        // pattern_args:gradient->sp.stops, sp->apply.signal.
        let nodes = vec![
            node("pattern_args", "pattern_args", &[]),
            node("s0", "scalar", &[("value", Value::from(0.0))]),
            node("s1", "scalar", &[("value", Value::from(1.0))]),
            node("ramp", "ramp_between", &[]),
            node("sp", "sample_palette", &[]),
            node("apply", "apply_color", &[]),
            node("view", "view_signal", &[]),
        ];
        let edges = vec![
            edge("s0", "out", "ramp", "start"),
            edge("s1", "out", "ramp", "end"),
            edge("ramp", "out", "sp", "u"),
            edge("pattern_args", "gradient", "sp", "stops"),
            edge("sp", "out", "apply", "signal"),
            edge("sp", "out", "view", "in"),
            edge("pattern_args", "selection", "apply", "selection"),
        ];

        let ctx = ResidentContext {
            span,
            ..Default::default()
        };
        let plan = compile_pattern(&nodes, &edges, &args, ctx, primitive_ids).unwrap();

        let mut arena = Arena::default();
        let frames = eval(&plan, &times, &mut arena);

        // Semantic match (not bit-exact): legacy render_frame quantizes the ramp
        // grid, so allow a small tolerance at the gradient extremes.
        const TOL: f32 = 2.0e-2;
        let mut max_diff = 0.0f32;
        for (fi, gframe) in golden["frames"].as_array().unwrap().iter().enumerate() {
            for gp in gframe["primitives"].as_array().unwrap() {
                let id = gp["primitive_id"].as_str().unwrap();
                let got = &frames[fi].primitives[id];
                let gd = gp["dimmer"].as_f64().unwrap() as f32;
                let gc: Vec<f32> = gp["color"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_f64().unwrap() as f32)
                    .collect();
                max_diff = max_diff.max((got.dimmer - gd).abs());
                for ch in 0..3 {
                    max_diff = max_diff.max((got.color[ch] - gc[ch]).abs());
                }
                assert!(
                    (got.dimmer - gd).abs() < TOL,
                    "frame {fi} {id}: dimmer {} vs golden {}",
                    got.dimmer,
                    gd
                );
                for ch in 0..3 {
                    assert!(
                        (got.color[ch] - gc[ch]).abs() < TOL,
                        "frame {fi} {id} ch{ch}: {} vs golden {}",
                        got.color[ch],
                        gc[ch]
                    );
                }
            }
        }
        println!("[gradient golden] max field diff = {max_diff:.5}");
    }
}
