//! Compiler: `Vec<NodeInstance>` (+ edges, args, context) -> [`Plan`]. See
//! `docs/eval-ir.md`. The per-node lowering is split by category (one file each,
//! mirroring the op kernels) so it can be extended in parallel. `mod.rs` owns the
//! driver, the `LowerCtx` helper API, the dispatch, and shared helpers; each
//! `compile/<cat>.rs` owns `lower_<cat>` for its node `type_id`s.
//!
//! A `lower_<cat>` returns `Some(Ok)` if it handled the node, `Some(Err)` if it
//! handled-but-failed, `None` if the node isn't its category's. Dispatch tries
//! them in order; if none claim the node it's a hard `UnknownNode` error
//! (no legacy silent-skip — docs/eval-ir.md §6).
//!
//! Milestones (validated by `cargo run --release --bin run_goldens`):
//!   C1 gradient ✅ · C2 seed/Stops/mask · C3 frozen reductions ·
//!   C4 audio/STFT · C5 segments + z-order Blend.

mod audio;
mod color;
mod math;
mod select_apply;
mod signals;
mod spatial;
mod structural;

use crate::eval::{Op, OpKind, OutputBinding, Phase, Plan, ResidentContext, SlotId, SlotSpec};
use crate::models::node_graph::{Edge, NodeInstance, Stops};
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

/// Output port ids a node type declares in the canonical registry
/// (`node_graph::nodes::get_node_types`). Lowerers record their op output under
/// these names so downstream edges — which reference registry port ids — resolve,
/// instead of hardcoding port strings. Memoized; the registry is pure (no I/O).
fn registry_output_ports(type_id: &str) -> &'static [String] {
    static MAP: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        crate::node_graph::nodes::get_node_types()
            .into_iter()
            .map(|t| (t.id, t.outputs.into_iter().map(|p| p.id).collect()))
            .collect()
    });
    map.get(type_id).map(Vec::as_slice).unwrap_or(&[])
}

/// Accumulates the lowered program as nodes are visited in topo order.
pub struct Lowerer {
    pub ops: Vec<Op>,
    pub slots: Vec<SlotSpec>,
    pub outputs: OutputBinding,
    /// node id + output port -> the slot carrying that value.
    pub node_slot: HashMap<(String, String), SlotId>,
    /// Primitive count (the `n` axis).
    pub n: u32,
    /// Frozen-stat requests: pair `i` reduces `frozen_reqs[i]`'s global
    /// (min,max), stored at `ctx.frozen[2i]` / `[2i+1]` by the compiler's stat
    /// pass. The op carries `stat_idx = 2i`.
    pub frozen_reqs: Vec<SlotId>,
}

impl Lowerer {
    pub fn new(n: u32) -> Self {
        Self {
            ops: Vec::new(),
            slots: Vec::new(),
            outputs: OutputBinding::default(),
            node_slot: HashMap::new(),
            n,
            frozen_reqs: Vec::new(),
        }
    }
    /// Register a global (min,max) reduction over `input`. Returns the `stat_idx`
    /// the op stores (an offset into `ctx.frozen`); the compiler's stat pass fills
    /// `frozen[stat_idx]`/`[stat_idx+1]`.
    pub fn alloc_frozen(&mut self, input: SlotId) -> usize {
        let pair = self.frozen_reqs.len();
        self.frozen_reqs.push(input);
        pair * 2
    }
    /// Reserve a fresh output slot of the given shape.
    pub fn alloc(&mut self, n: u32, c: u32) -> SlotId {
        let id = self.slots.len() as SlotId;
        self.slots.push(SlotSpec { n, c });
        id
    }
    /// Push an op writing a freshly-allocated `(n,c)` slot, record `node:out`, return the slot.
    pub fn emit(&mut self, kind: OpKind, inputs: Vec<SlotId>, n: u32, c: u32, phase: Phase, node: &str, port: &str) -> SlotId {
        let out = self.alloc(n, c);
        self.ops.push(Op { kind, inputs, out, phase });
        self.node_slot.insert((node.to_string(), port.to_string()), out);
        out
    }
    pub fn slot_shape(&self, id: SlotId) -> (u32, u32) {
        let s = self.slots[id as usize];
        (s.n, s.c)
    }
}

#[derive(Debug)]
pub enum CompileError {
    /// A node references an op the engine does not implement. Hard error.
    UnknownNode { id: String, type_id: String },
    /// A node's required input port has no resolved upstream slot.
    MissingInput { node: String, port: String },
    Graph(String),
}

/// Everything a `lower_<cat>` needs about the node being lowered.
pub struct LowerCtx<'a> {
    pub node: &'a NodeInstance,
    pub edges: &'a [Edge],
    pub args: &'a HashMap<String, Value>,
    pub by_id: &'a HashMap<&'a str, &'a NodeInstance>,
}

impl LowerCtx<'_> {
    pub fn type_id(&self) -> &str {
        &self.node.type_id
    }
    pub fn param_f32(&self, key: &str, default: f32) -> f32 {
        self.node.params.get(key).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(default)
    }
    pub fn param_bool(&self, key: &str, default: bool) -> bool {
        self.node.params.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }
    pub fn param_str(&self, key: &str) -> Option<String> {
        self.node.params.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    }
    pub fn param(&self, key: &str) -> Option<&Value> {
        self.node.params.get(key)
    }
    /// The edge feeding `port`, if any.
    fn edge_to(&self, port: &str) -> Option<&Edge> {
        self.edges.iter().find(|e| e.to_node == self.node.id && e.to_port == port)
    }
    /// The slot feeding `port` (upstream edge's resolved output slot).
    pub fn input(&self, low: &Lowerer, port: &str) -> Option<SlotId> {
        let e = self.edge_to(port)?;
        low.node_slot.get(&(e.from_node.clone(), e.from_port.clone())).copied()
    }
    pub fn require(&self, low: &Lowerer, port: &str) -> Result<SlotId, CompileError> {
        self.input(low, port).ok_or_else(|| CompileError::MissingInput {
            node: self.node.id.clone(),
            port: port.to_string(),
        })
    }
    /// Output `n` of the slot feeding `port` (for shape inference / broadcast).
    pub fn input_n(&self, low: &Lowerer, port: &str) -> u32 {
        self.input(low, port).map(|s| low.slot_shape(s).0).unwrap_or(1)
    }
    /// Constant value of a config port fed directly by a `scalar` node (compile-time
    /// fold). Falls back to a same-named node param, then `default`.
    pub fn const_input(&self, port: &str, default: f32) -> f32 {
        if let Some(e) = self.edge_to(port) {
            if let Some(src) = self.by_id.get(e.from_node.as_str()) {
                if src.type_id == "scalar" {
                    return src.params.get("value").and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(default);
                }
            }
        }
        self.param_f32(port, default)
    }
    /// Resolve a `Stops` input wired from a `pattern_args` output port (inlined —
    /// there is no Stops slot type in the IR).
    pub fn resolve_stops(&self, port: &str) -> Result<Stops, CompileError> {
        let e = self.edge_to(port).ok_or_else(|| CompileError::MissingInput {
            node: self.node.id.clone(),
            port: port.to_string(),
        })?;
        if self.by_id.get(e.from_node.as_str()).map(|n| n.type_id.as_str()) == Some("pattern_args") {
            if let Some(v) = self.args.get(&e.from_port) {
                return Ok(parse_stops(v));
            }
        }
        Err(CompileError::MissingInput { node: self.node.id.clone(), port: format!("{port} (stops)") })
    }
    /// This node type's sole declared output port (falls back to `"out"`). Use
    /// instead of a hardcoded port string for single-output nodes.
    pub fn out_port(&self) -> &'static str {
        registry_output_ports(self.type_id()).first().map(String::as_str).unwrap_or("out")
    }
    /// All declared output ports, in registry order (for multi-output nodes like
    /// `stem_splitter`).
    pub fn out_ports(&self) -> &'static [String] {
        registry_output_ports(self.type_id())
    }
    /// Deterministic per-node seed: `DefaultHasher(node.id)` — matches legacy.
    pub fn seed(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.node.id.hash(&mut h);
        h.finish()
    }
}

type Lower = fn(&LowerCtx, &mut Lowerer) -> Option<Result<(), CompileError>>;

/// Compile a single pattern graph against a resolved resident context and args.
pub fn compile_pattern(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, Value>,
    ctx: ResidentContext,
    primitive_ids: Vec<String>,
) -> Result<Plan, CompileError> {
    let n = primitive_ids.len() as u32;
    let mut low = Lowerer::new(n);
    let by_id: HashMap<&str, &NodeInstance> = nodes.iter().map(|nd| (nd.id.as_str(), nd)).collect();

    for node in topo_order(nodes, edges)? {
        let lc = LowerCtx { node, edges, args, by_id: &by_id };
        lower_node(&lc, &mut low)?;
    }

    let mut plan = Plan {
        ops: low.ops,
        slots: low.slots,
        n,
        primitive_ids,
        outputs: low.outputs,
        ctx,
    };
    fill_frozen_stats(&mut plan, &low.frozen_reqs);
    Ok(plan)
}

/// Frozen-stat pass (eval-mode batchnorm): run the plan over a dense grid of the
/// annotation span and record each Normalize/Invert input's global (min,max) into
/// `plan.ctx.frozen`. v1 does a single pass (nested reductions read the inner's
/// pre-fill output — rare; refine later).
fn fill_frozen_stats(plan: &mut Plan, frozen_reqs: &[SlotId]) {
    if frozen_reqs.is_empty() {
        return;
    }
    // Size frozen up front so the Normalize/Invert ops don't index OOB during the
    // probe (their output is unused here — we only read their *input* slots).
    plan.ctx.frozen = vec![0.0; frozen_reqs.len() * 2];

    let (s0, s1) = plan.ctx.span;
    let dur = (s1 - s0).max(1e-3);
    let steps = ((dur * 44.0).ceil() as usize).clamp(2, 4096);
    let times: Vec<f32> = (0..steps)
        .map(|i| s0 + dur * (i as f32 / (steps - 1) as f32))
        .collect();

    let mut scratch = crate::eval::Arena::default();
    let stats = crate::eval::slot_stats(plan, &times, frozen_reqs, &mut scratch);
    let mut frozen = vec![0.0; frozen_reqs.len() * 2];
    for (i, (mn, mx)) in stats.into_iter().enumerate() {
        frozen[2 * i] = mn;
        frozen[2 * i + 1] = mx;
    }
    plan.ctx.frozen = frozen;
}

fn lower_node(lc: &LowerCtx, low: &mut Lowerer) -> Result<(), CompileError> {
    const LOWERERS: [Lower; 7] = [
        math::lower_math,
        color::lower_color,
        spatial::lower_spatial,
        signals::lower_signals,
        select_apply::lower_select_apply,
        audio::lower_audio,
        structural::lower_structural,
    ];
    for f in LOWERERS {
        if let Some(r) = f(lc, low) {
            return r;
        }
    }
    Err(CompileError::UnknownNode {
        id: lc.node.id.clone(),
        type_id: lc.node.type_id.clone(),
    })
}

/// Kahn's algorithm, ids sorted for deterministic order.
fn topo_order<'a>(nodes: &'a [NodeInstance], edges: &[Edge]) -> Result<Vec<&'a NodeInstance>, CompileError> {
    let by_id: HashMap<&str, &NodeInstance> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut indeg: HashMap<&str, usize> = nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        if by_id.contains_key(e.from_node.as_str()) && by_id.contains_key(e.to_node.as_str()) {
            *indeg.get_mut(e.to_node.as_str()).unwrap() += 1;
            adj.entry(e.from_node.as_str()).or_default().push(e.to_node.as_str());
        }
    }
    let mut queue: Vec<&str> = indeg.iter().filter(|(_, &d)| d == 0).map(|(k, _)| *k).collect();
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

/// Parse a gradient/palette arg (`{stops: [{color, t}]}`) into `Stops`.
pub(crate) fn parse_stops(v: &Value) -> Stops {
    let mut stops: Vec<(f32, [f32; 4])> = Vec::new();
    if let Some(arr) = v.get("stops").and_then(|s| s.as_array()) {
        for s in arr {
            let t = s.get("t").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            let rgba = s.get("color").and_then(|c| c.as_str()).map(parse_hex).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            stops.push((t, rgba));
        }
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Stops { stops }
}

pub(crate) fn parse_hex(h: &str) -> [f32; 4] {
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
        Edge { id: format!("{from}:{fp}->{to}:{tp}"), from_node: from.into(), from_port: fp.into(), to_node: to.into(), to_port: tp.into() }
    }

    /// First E2E proof: compile `gradient` and match its golden frame-for-frame.
    /// Portable (reads only the committed fixture).
    #[test]
    fn gradient_matches_golden() {
        let path = format!("{}/tests/golden/fixtures/gradient.json", env!("CARGO_MANIFEST_DIR"));
        let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("golden")).expect("json");

        let primitive_ids: Vec<String> = golden["frames"][0]["primitives"].as_array().unwrap().iter()
            .map(|p| p["primitive_id"].as_str().unwrap().to_string()).collect();
        let times: Vec<f32> = golden["sample_times"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap() as f32).collect();
        let span = (golden["start_time"].as_f64().unwrap() as f32, golden["end_time"].as_f64().unwrap() as f32);
        let mut args = HashMap::new();
        args.insert("gradient".to_string(), golden["arg_values"]["gradient"].clone());

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

        let ctx = ResidentContext { span, ..Default::default() };
        let plan = compile_pattern(&nodes, &edges, &args, ctx, primitive_ids).unwrap();
        let mut arena = Arena::default();
        let frames = eval(&plan, &times, &mut arena);

        const TOL: f32 = 2.0e-2;
        for (fi, gframe) in golden["frames"].as_array().unwrap().iter().enumerate() {
            for gp in gframe["primitives"].as_array().unwrap() {
                let id = gp["primitive_id"].as_str().unwrap();
                let got = &frames[fi].primitives[id];
                let gd = gp["dimmer"].as_f64().unwrap() as f32;
                assert!((got.dimmer - gd).abs() < TOL, "frame {fi} {id} dimmer {} vs {}", got.dimmer, gd);
                let gc = gp["color"].as_array().unwrap();
                for ch in 0..3 {
                    let c = gc[ch].as_f64().unwrap() as f32;
                    assert!((got.color[ch] - c).abs() < TOL, "frame {fi} {id} ch{ch} {} vs {}", got.color[ch], c);
                }
            }
        }
    }
}
