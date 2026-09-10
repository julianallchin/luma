//! Lower the original untyped numerical vocabulary into ordinary editable
//! signal graphs. Helpers retain old node cards; their bodies use canonical ops.
use super::{argument_value, ordered_nodes, Graph, NodeInstance, PatternArgType, ROOT};
use luma_patterns::{
    self as p, Binding as B, Body, Definition, Input, Node, Output, Rate, Value, ValueType,
};
use std::collections::{BTreeMap, BTreeSet};
mod audio;
pub(super) mod events;
mod harmony;
mod noise;
mod selection;
mod spatial;
mod typed;
mod voronoi;

const SUPPORTED: &[&str] = &[
    "pattern_args",
    "audio_input",
    "stem_splitter",
    "frequency_amplitude",
    "lowpass_filter",
    "highpass_filter",
    "beat_pulses",
    "beat_envelope",
    "drum_events",
    "adsr",
    "select",
    "random_select_mask",
    "soft_voronoi",
    "filter_selection",
    "view_signal",
    "view_uv",
    "view_events",
    "mel_spec_viewer",
    "scalar",
    "color",
    "gradient",
    "palette",
    "sample_palette",
    "rainbow",
    "ramp",
    "ramp_between",
    "math",
    "round",
    "normalize",
    "invert",
    "threshold",
    "remap",
    "modulo",
    "sine_wave",
    "time_delay",
    "circle",
    "figure_8",
    "sweep",
    "apply_color",
    "apply_dimmer",
    "apply_strobe",
    "apply_speed",
    "apply_movement",
    "get_attribute",
    "mirror",
    "falloff",
    "harmony_analysis",
    "chroma_palette",
    "spectral_shift",
    "noise",
    "wander",
];

pub(crate) fn supports(graph: &Graph) -> bool {
    graph.nodes.iter().all(|n| {
        SUPPORTED.contains(&n.type_id.as_str())
            || n.type_id.starts_with(super::super::lighting::PREFIX)
    })
}

pub(crate) fn convert(source: &Graph, name: &str) -> Result<p::Score, String> {
    let base = p::standard_library();
    let mut score = p::Score::default();
    let mut root = p::Graph::default();
    let mut inputs = BTreeMap::new();
    let mut sources = BTreeMap::<(String, String), B>::new();
    let mut bundles = BTreeMap::new();
    let mut consumed = BTreeSet::new();
    let origin = source
        .nodes
        .iter()
        .find(|n| n.type_id == "pattern_args")
        .map(|n| [n.position_x.unwrap_or(0.), n.position_y.unwrap_or(0.)])
        .unwrap_or([0., 0.]);
    for arg in &source.args {
        let kind = match arg.arg_type {
            PatternArgType::Seed => ValueType::Seed,
            PatternArgType::Selection => continue,
            PatternArgType::Color => ValueType::Color,
            PatternArgType::Gradient | PatternArgType::Palette => ValueType::Gradient,
            PatternArgType::Scalar => ValueType::Number,
            PatternArgType::Beats => ValueType::Beats,
            PatternArgType::Proportion => ValueType::Proportion,
            PatternArgType::Position => ValueType::Position,
            PatternArgType::Boolean => ValueType::Boolean,
            PatternArgType::Mapping => ValueType::Mapping,
            PatternArgType::Boundary => ValueType::Boundary,
            PatternArgType::Envelope => ValueType::Envelope,
            PatternArgType::AudioSource => ValueType::AudioSource,
            PatternArgType::Drum => ValueType::Drum,
        };
        let value = argument_value(&arg.arg_type, kind, &arg.default_value)?;
        let kind = kind.signal_type().map(ValueType::Signal).unwrap_or(kind);
        inputs.insert(
            arg.id.clone(),
            Input {
                rate: Rate::Fixed,
                ..input(&arg.name, kind, Some(value))
            },
        );
        root.input_nodes.insert(
            arg.id.clone(),
            p::InputNode {
                name: arg.name.clone(),
                position: Some([origin[0], origin[1] + root.input_nodes.len() as f64 * 68.]),
            },
        );
        for node in source.nodes.iter().filter(|n| n.type_id == "pattern_args") {
            sources.insert(
                (node.id.clone(), arg.id.clone()),
                B::Input {
                    input: arg.id.clone(),
                },
            );
        }
    }
    let mut terminal = BTreeMap::new();
    let mixed = source
        .nodes
        .iter()
        .any(|n| n.type_id.starts_with(super::super::lighting::PREFIX));
    let write = |terminal: &mut BTreeMap<String, B>, key: &str, binding: B| -> Result<(), String> {
        if mixed && terminal.contains_key(key) {
            return Err(format!(
                "Mixed graph has multiple {key} outputs; combine them explicitly"
            ));
        }
        terminal.insert(key.into(), binding);
        Ok(())
    };
    for node in ordered_nodes(&source.nodes, &source.edges)? {
        if node.type_id == "audio_input" {
            sources.insert(
                (node.id.clone(), "out".into()),
                Value::AudioSource(p::AudioSource::Mix.into()).into(),
            );
            continue;
        }
        if matches!(node.type_id.as_str(), "pattern_args" | "select") {
            continue;
        }
        let mut source_binding = |port: &str| -> Result<Option<B>, String> {
            let mut edges = source
                .edges
                .iter()
                .filter(|e| e.to_node == node.id && e.to_port == port);
            let Some(edge) = edges.next() else {
                return Ok(None);
            };
            if edges.next().is_some() {
                return Err(format!("{}.{} has multiple sources", node.id, port));
            }
            consumed.insert(edge.id.clone());
            if port == "events_in"
                && edge.from_port == "events_out"
                && source
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.from_node && node.type_id == "beat_pulses")
            {
                return Ok(Some(wire(&edge.from_node, "trigger")));
            }
            sources
                .get(&(edge.from_node.clone(), edge.from_port.clone()))
                .cloned()
                .map(Some)
                .ok_or_else(|| format!("{}.{} has no numerical source", node.id, port))
        };
        if node.type_id == "mel_spec_viewer" {
            let binding = source_binding("in")?
                .unwrap_or_else(|| Value::AudioSource(p::AudioSource::Mix.into()).into());
            let library = score.library(&base).map_err(|e| e.to_string())?;
            if library
                .binding_type(&inputs, &root, &binding)
                .map_err(|e| e.to_string())?
                .0
                != ValueType::AudioSource
            {
                return Err(format!("{}.in needs audio", node.id));
            }
            let grid: Vec<_> = source
                .edges
                .iter()
                .filter(|edge| edge.to_node == node.id && edge.to_port == "grid")
                .collect();
            if grid.len() > 1 {
                return Err(format!("{}.grid has multiple sources", node.id));
            }
            if let Some(edge) = grid.first() {
                // The original audio_input emitted this undocumented port.
                // Beat overlays now use the preview's real track clock directly.
                if edge.from_port != "grid_out"
                    || !source
                        .nodes
                        .iter()
                        .any(|n| n.id == edge.from_node && n.type_id == "audio_input")
                {
                    return Err(format!("{}.grid needs the track beat grid", node.id));
                }
                consumed.insert(edge.id.clone());
            }
            root.outputs.insert(format!("view/{}", node.id), binding);
            continue;
        }
        if matches!(
            node.type_id.as_str(),
            "view_signal" | "view_uv" | "view_events"
        ) {
            let port = match node.type_id.as_str() {
                "view_uv" => "uv",
                "view_events" => "events_in",
                _ => "in",
            };
            if let Some(binding) = source_binding(port)? {
                if node.type_id == "view_events" {
                    let library = score.library(&base).map_err(|e| e.to_string())?;
                    if library
                        .binding_type(&inputs, &root, &binding)
                        .map_err(|e| e.to_string())?
                        .0
                        != ValueType::Events
                    {
                        return Err(format!("{}.events_in needs events", node.id));
                    }
                }
                root.outputs.insert(format!("view/{}", node.id), binding);
            }
            continue;
        }
        if node.type_id == "time_delay" {
            // This historical node executed as identity; no delay was applied.
            // Preserve its evaluated behavior by reconnecting its input directly.
            let binding = source_binding("in")?
                .ok_or_else(|| format!("{}.in needs a connection", node.id))?;
            sources.insert((node.id.clone(), "out".into()), binding);
            continue;
        }
        if node.type_id.starts_with(super::super::lighting::PREFIX) {
            let converted = typed::lower(node, source, &mut source_binding, &bundles)?;
            consumed.extend(converted.bundle_edges);
            for (port, value) in converted.outputs {
                match value {
                    p::migration::Expanded::Single(binding) => {
                        sources.insert((node.id.clone(), port), binding);
                    }
                    p::migration::Expanded::Bundle(caps) => {
                        if !source
                            .edges
                            .iter()
                            .any(|e| e.from_node == node.id && e.from_port == port)
                        {
                            for (cap, binding) in &caps {
                                write(&mut terminal, cap.key(), binding.clone())?;
                            }
                        }
                        bundles.insert((node.id.clone(), port), caps);
                    }
                }
            }
            root.nodes.insert(node.id.clone(), converted.node);
            for (id, definition) in converted.definitions {
                if score.definitions.insert(id.clone(), definition).is_some() {
                    return Err(format!("Duplicate migrated definition {id}"));
                }
            }
            continue;
        }
        let library = score.library(&base).map_err(|e| e.to_string())?;
        if matches!(node.type_id.as_str(), "stem_splitter" | "harmony_analysis") {
            // Both nodes used the current track's stored analysis. Validate
            // their contextual audio wire before replacing it with that source.
            if let Some(binding) = source_binding("audio_in")? {
                if library
                    .binding_type(&inputs, &root, &binding)
                    .map_err(|e| e.to_string())?
                    .0
                    != ValueType::AudioSource
                {
                    return Err(format!("{}.audio_in needs an audio source", node.id));
                }
            }
        }
        let widths = source
            .edges
            .iter()
            .filter(|e| e.to_node == node.id)
            .filter_map(|edge| {
                sources
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .map(|binding| (edge.to_port.clone(), binding))
            })
            .map(|(port, binding)| {
                let kind = library
                    .binding_type(&inputs, &root, binding)
                    .map_err(|e| e.to_string())?
                    .0;
                Ok((
                    port,
                    kind.signal_type()
                        .and_then(|s| s.channels)
                        .map(|c| c.count()),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let helper = lower(node, &widths)?;
        let mut bound = BTreeMap::new();
        for (key, spec) in &helper.inputs {
            if let Some(binding) = source_binding(key)? {
                bound.insert(key.clone(), binding);
            } else if spec.default.is_none() {
                return Err(format!("{}.{} needs a connection", node.id, key));
            }
        }
        if node.type_id == "noise" && !bound.contains_key("x") {
            if let Some(reference) = bound.get("y").cloned() {
                // An absent X used the selected fixture index when Y was a
                // field. Make that contextual default an ordinary connection.
                let mut id = format!("{}_x_index", node.id);
                while root.nodes.contains_key(&id) || source.nodes.iter().any(|n| n.id == id) {
                    id.push('_');
                }
                root.nodes.insert(
                    id.clone(),
                    Node {
                        position: None,
                        definition: "core/domain_index".into(),
                        inputs: BTreeMap::from([("value".into(), reference)]),
                    },
                );
                bound.insert("x".into(), wire(&id, "value"));
            }
        }
        let helper_id = format!("node/{}", node.id);
        for port in helper.outputs.keys() {
            sources.insert((node.id.clone(), port.clone()), wire(&node.id, port));
        }
        match node.type_id.as_str() {
            "apply_color" => {
                for key in ["color", "dimmer"] {
                    write(&mut terminal, key, wire(&node.id, key))?;
                }
            }
            "apply_dimmer" => {
                write(&mut terminal, "dimmer", wire(&node.id, "out"))?;
            }
            "apply_strobe" => {
                write(&mut terminal, "strobe", wire(&node.id, "out"))?;
            }
            "apply_speed" => {
                write(&mut terminal, "speed", wire(&node.id, "out"))?;
            }
            "apply_movement" => {
                for key in ["pan", "tilt"] {
                    write(&mut terminal, key, wire(&node.id, key))?;
                }
            }
            _ => (),
        }
        root.nodes.insert(
            node.id.clone(),
            Node {
                position: node.position_x.zip(node.position_y).map(|(x, y)| [x, y]),
                definition: helper_id.clone(),
                inputs: bound,
            },
        );
        if score
            .definitions
            .insert(helper_id.clone(), helper)
            .is_some()
        {
            return Err(format!("Duplicate migrated definition {helper_id}"));
        }
    }
    for edge in &source.edges {
        if !consumed.contains(&edge.id) && !selection_edge(source, edge) {
            return Err(format!(
                "Unknown connection {}.{} → {}.{}",
                edge.from_node, edge.from_port, edge.to_node, edge.to_port
            ));
        }
    }
    // Unfinished graphs get an explicit unwritten Output; they remain editable.
    let mut output = "output".to_string();
    while root.nodes.contains_key(&output) {
        output.push('_');
    }
    root.nodes.insert(
        output.clone(),
        Node {
            position: None,
            definition: "output".into(),
            inputs: terminal,
        },
    );
    root.outputs
        .insert("lighting".into(), wire(&output, "lighting"));
    let library = score.library(&base).map_err(|e| e.to_string())?;
    let outputs = root
        .outputs
        .iter()
        .map(|(key, binding)| {
            library
                .binding_type(&inputs, &root, binding)
                .map(|(value_type, _)| {
                    (
                        key.clone(),
                        Output {
                            value_type,
                            rate: Rate::Frame,
                        },
                    )
                })
                .map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;
    score.definitions.insert(
        ROOT.into(),
        Definition {
            name: name.into(),
            inputs,
            outputs,
            body: Body::Graph(root),
        },
    );
    score.validate(&base).map_err(|e| e.to_string())?;
    Ok(score)
}

fn selection_edge(graph: &Graph, edge: &crate::models::node_graph::Edge) -> bool {
    if edge.to_port != "selection" {
        return false;
    }
    let Some(from) = graph.nodes.iter().find(|n| n.id == edge.from_node) else {
        return false;
    };
    let Some(to) = graph.nodes.iter().find(|n| n.id == edge.to_node) else {
        return false;
    };
    let source_is_selection = if from.type_id == "pattern_args" {
        graph
            .args
            .iter()
            .any(|a| a.id == edge.from_port && a.arg_type == PatternArgType::Selection)
    } else {
        matches!(
            from.type_id.as_str(),
            "select" | "filter_selection" | "mirror"
        ) && edge.from_port == "out"
    };
    source_is_selection
        && (matches!(to.type_id.as_str(), "select" | "apply_dimmer")
            || crate::node_graph::nodes::get_node_types()
                .iter()
                .any(|kind| {
                    kind.id == to.type_id
                        && kind.inputs.iter().any(|port| {
                            port.id == edge.to_port
                                && port.port_type == crate::models::node_graph::PortType::Selection
                        })
                }))
}

fn input(name: &str, kind: ValueType, default: Option<Value>) -> Input {
    Input {
        optional: false,
        name: name.into(),
        description: String::new(),
        value_type: kind,
        rate: Rate::Frame,
        default,
    }
}
fn number(v: f64) -> B {
    Value::Number(v).into()
}
fn wire(node: &str, output: &str) -> B {
    B::Connection {
        node: node.into(),
        output: output.into(),
    }
}

#[derive(Default)]
struct Builder {
    graph: p::Graph,
    inputs: BTreeMap<String, Input>,
}
impl Builder {
    fn historical_beats(&mut self, relative: bool) -> B {
        let time = self.node("core/track_time", []);
        let seconds = if relative {
            self.math(
                "subtract",
                wire(&time, "seconds"),
                wire(&time, "clip_start"),
            )
        } else {
            wire(&time, "seconds")
        };
        let seconds = self.math("divide", seconds, Value::Seconds(1.).into());
        let rate = self.math("divide", wire(&time, "bpm"), number(60.));
        self.math("multiply", seconds, rate)
    }
    fn input(&mut self, id: &str, name: &str, kind: ValueType, default: Option<Value>) -> B {
        self.inputs.insert(id.into(), input(name, kind, default));
        B::Input { input: id.into() }
    }
    fn numeric(&mut self, node: &NodeInstance, id: &str, name: &str, default: Option<f64>) -> B {
        let value = node
            .params
            .get(id)
            .and_then(serde_json::Value::as_f64)
            .or(default)
            .map(Value::Number);
        self.input(id, name, ValueType::Signal(p::SignalType::ANY), value)
    }
    fn scalar(&mut self, node: &NodeInstance, id: &str, name: &str, default: f64) -> B {
        let value = node
            .params
            .get(id)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default);
        self.input(id, name, ValueType::Number, Some(Value::Number(value)))
    }
    fn node(
        &mut self,
        definition: &str,
        inputs: impl IntoIterator<Item = (&'static str, B)>,
    ) -> String {
        let id = format!("op{}", self.graph.nodes.len());
        self.graph.nodes.insert(
            id.clone(),
            Node {
                position: None,
                definition: definition.into(),
                inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            },
        );
        id
    }
    fn call(
        &mut self,
        definition: &str,
        inputs: impl IntoIterator<Item = (&'static str, B)>,
        output: &str,
    ) -> B {
        let id = self.node(definition, inputs);
        wire(&id, output)
    }
    fn math(&mut self, op: &str, a: B, b: B) -> B {
        self.call(&format!("core/{op}"), [("a", a), ("b", b)], "value")
    }
    fn unary(&mut self, op: &str, value: B) -> B {
        self.call(&format!("core/{op}"), [("value", value)], "value")
    }
    fn greater(&mut self, a: B, b: B) -> B {
        self.call("core/greater", [("a", a), ("b", b)], "mask")
    }
    fn choose(&mut self, condition: B, yes: B, no: B) -> B {
        self.call(
            "core/choose",
            [("condition", condition), ("yes", yes), ("no", no)],
            "value",
        )
    }
    fn clamp(&mut self, value: B) -> B {
        self.call("core/clamp_coverage", [("value", value)], "mask")
    }
    fn channel(&mut self, value: B) -> B {
        self.channel_at(value, 0)
    }
    fn channel_at(&mut self, value: B, index: usize) -> B {
        self.call(
            "core/channel",
            [("value", value), ("index", number(index as f64))],
            "value",
        )
    }
    fn join(&mut self, a: B, b: B) -> B {
        self.call("core/join_channels", [("a", a), ("b", b)], "value")
    }
    fn cosine(&mut self, phase: B) -> B {
        let phase = self.math("add", phase, number(0.25));
        self.unary("sine", phase)
    }
    fn resize(&mut self, value: B, from: usize, to: usize) -> B {
        let mut out = self.channel(value.clone());
        for index in 1..to {
            let channel = self.channel_at(value.clone(), index.min(from - 1));
            out = self.join(out, channel);
        }
        out
    }
    fn ceil(&mut self, value: B) -> B {
        let neg = self.math("multiply", value, number(-1.));
        let floor = self.unary("floor", neg);
        self.math("multiply", floor, number(-1.))
    }
    fn trunc(&mut self, value: B) -> B {
        let positive = self.greater(value.clone(), number(0.));
        let floor = self.unary("floor", value.clone());
        let ceil = self.ceil(value);
        self.choose(positive, floor, ceil)
    }
    fn round(&mut self, value: B) -> B {
        let positive = self.greater(value.clone(), number(0.));
        let hi = self.math("add", value.clone(), number(0.5));
        let hi = self.unary("floor", hi);
        let lo = self.math("subtract", value, number(0.5));
        let lo = self.ceil(lo);
        self.choose(positive, hi, lo)
    }
    fn finish(
        mut self,
        name: &str,
        outputs: impl IntoIterator<Item = (&'static str, B)>,
    ) -> Result<Definition, String> {
        self.graph.outputs = outputs.into_iter().map(|(k, v)| (k.into(), v)).collect();
        let library = p::standard_library();
        let outputs = self
            .graph
            .outputs
            .iter()
            .map(|(key, b)| {
                library
                    .binding_type(&self.inputs, &self.graph, b)
                    .map(|(value_type, _)| {
                        (
                            key.clone(),
                            Output {
                                value_type,
                                rate: Rate::Frame,
                            },
                        )
                    })
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        Ok(Definition {
            name: name.into(),
            inputs: self.inputs,
            outputs,
            body: Body::Graph(self.graph),
        })
    }
}

fn lower(
    node: &NodeInstance,
    widths: &BTreeMap<String, Option<usize>>,
) -> Result<Definition, String> {
    if node.type_id == "soft_voronoi" {
        return voronoi::lower(node);
    }
    if node.type_id == "random_select_mask" {
        return selection::lower(node);
    }
    if matches!(
        node.type_id.as_str(),
        "beat_pulses" | "beat_envelope" | "drum_events" | "adsr"
    ) {
        return events::lower(node);
    }
    if matches!(
        node.type_id.as_str(),
        "stem_splitter" | "frequency_amplitude" | "lowpass_filter" | "highpass_filter"
    ) {
        return audio::lower(node);
    }
    if matches!(node.type_id.as_str(), "noise" | "wander") {
        return noise::lower(node);
    }
    if matches!(node.type_id.as_str(), "get_attribute" | "mirror") {
        return spatial::lower(node, widths);
    }
    if matches!(
        node.type_id.as_str(),
        "harmony_analysis" | "chroma_palette" | "spectral_shift"
    ) {
        return harmony::lower(node);
    }
    let mut b = Builder::default();
    let kind = node.type_id.as_str();
    let param = |key: &str, default: &'static str| {
        node.params
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(default)
    };
    let name;
    let out = match kind {
        "normalize" | "invert" => {
            name = if kind == "normalize" {
                "Normalize"
            } else {
                "Invert over clip"
            };
            let value = b.numeric(node, "in", "Signal", None);
            b.call(kind, [("value", value)], "value")
        }
        "scalar" => {
            name = "Value";
            b.numeric(node, "value", "Value", Some(0.))
        }
        "filter_selection" => {
            name = "Selection mask";
            number(1.)
        }
        "ramp" => {
            name = "Beat time";
            b.historical_beats(true)
        }
        "ramp_between" => {
            name = "Ramp";
            let a = b.numeric(node, "start", "Start", Some(0.));
            let a = b.channel(a);
            let end = b.numeric(node, "end", "End", Some(1.));
            let end = b.channel(end);
            let time = b.node("core/track_time", []);
            let elapsed = b.math(
                "subtract",
                wire(&time, "seconds"),
                wire(&time, "clip_start"),
            );
            let progress = b.math("divide", elapsed, wire(&time, "clip_duration"));
            let progress = b.clamp(progress);
            let range = b.math("subtract", end, a.clone());
            let travel = b.math("multiply", range, progress);
            b.math("add", a, travel)
        }
        "math" => {
            let mut a = b.numeric(node, "a", "A", Some(0.));
            let mut c = b.numeric(node, "b", "B", Some(0.));
            let aw = widths.get("a").copied().flatten().unwrap_or(1);
            let bw = widths.get("b").copied().flatten().unwrap_or(1);
            // The old numerical engine repeated the final component for
            // unequal vectors. Make that adaptation explicit in the graph.
            if aw != bw && aw > 1 && bw > 1 {
                if aw < bw {
                    a = b.resize(a, aw, bw);
                } else {
                    c = b.resize(c, bw, aw);
                }
            }
            let op = param("operation", "add");
            name = match op {
                "add" => "Add",
                "subtract" => "Subtract",
                "multiply" => "Multiply",
                "divide" => "Divide",
                "min" => "Minimum",
                "max" => "Maximum",
                "abs_diff" => "Absolute difference",
                "circular_distance" => "Circular distance",
                "modulo" => "Remainder",
                _ => return Err(format!("unsupported math operation {op}")),
            };
            match op {
                "add" | "subtract" | "multiply" | "divide" => b.math(op, a, c),
                "min" => b.math("minimum", a, c),
                "max" => b.math("maximum", a, c),
                "abs_diff" => {
                    let d = b.math("subtract", a, c);
                    b.unary("absolute", d)
                }
                "circular_distance" => {
                    let d = b.math("subtract", a, c);
                    let d = b.unary("absolute", d);
                    let d = b.unary("fraction", d);
                    let other = b.math("subtract", number(1.), d.clone());
                    b.math("minimum", d, other)
                }
                "modulo" => {
                    let quotient = b.math("divide", a.clone(), c.clone());
                    let whole = b.trunc(quotient);
                    let used = b.math("multiply", whole, c.clone());
                    let rest = b.math("subtract", a, used);
                    let abs = b.unary("absolute", c);
                    let nonzero = b.greater(abs, number(0.));
                    b.choose(nonzero, rest, number(0.))
                }
                _ => return Err(format!("unsupported math operation {op}")),
            }
        }
        "falloff" => {
            name = "Falloff";
            let value = b.numeric(node, "in", "Value", None);
            let width = b.scalar(node, "width", "Width", 1.);
            let curve = b.scalar(node, "curve", "Curve", 0.);
            let width = b.math("maximum", width, number(1e-6));
            let value = b.clamp(value);
            let value = b.math("multiply", value, width);
            let value = b.clamp(value);
            let abs = b.unary("absolute", curve.clone());
            let scaled = b.math("multiply", abs.clone(), number(5.));
            let exponent = b.math("add", number(1.), scaled);
            let positive = b.greater(curve, number(0.));
            let inverse = b.math("subtract", number(1.), value.clone());
            let base = b.choose(positive.clone(), value.clone(), inverse);
            let shaped = b.call(
                "core/power",
                [("base", base), ("exponent", exponent)],
                "value",
            );
            let inverse = b.math("subtract", number(1.), shaped.clone());
            let shaped = b.choose(positive, shaped, inverse);
            let linear = b.call(
                "core/greater",
                [("a", number(0.001)), ("b", abs), ("tolerance", number(0.))],
                "mask",
            );
            b.choose(linear, value, shaped)
        }
        "round" => {
            let value = b.numeric(node, "in", "Value", None);
            let op = param("operation", "round");
            name = match op {
                "floor" => "Floor",
                "ceil" => "Ceiling",
                "round" => "Round",
                _ => return Err(format!("unsupported rounding operation {op}")),
            };
            match op {
                "floor" => b.unary("floor", value),
                "ceil" => b.ceil(value),
                "round" => b.round(value),
                _ => return Err(format!("unsupported rounding operation {op}")),
            }
        }
        "threshold" => {
            name = "Threshold";
            let v = b.numeric(node, "in", "Value", None);
            let cutoff = b.numeric(node, "threshold", "Threshold", Some(0.5));
            let below = b.greater(cutoff, v);
            b.math("subtract", number(1.), below)
        }
        "modulo" => {
            name = "Wrap";
            let v = b.numeric(node, "in", "Value", None);
            let d = b.numeric(node, "divisor", "Period", Some(1.));
            let ratio = b.math("divide", v, d.clone());
            let wrapped = b.unary("fraction", ratio);
            let result = b.math("multiply", wrapped, d.clone());
            let valid = b.greater(d, number(0.));
            b.choose(valid, result, number(0.))
        }
        "remap" => {
            name = "Remap";
            let mut v = b.numeric(node, "in", "Value", None);
            let low = b.numeric(node, "in_min", "Input minimum", Some(-1.));
            let high = b.numeric(node, "in_max", "Input maximum", Some(1.));
            let start = b.numeric(node, "out_min", "Output minimum", Some(0.));
            let end = b.numeric(node, "out_max", "Output maximum", Some(180.));
            let clamp = b.numeric(node, "clamp", "Clamp input", Some(1.));
            let lo = b.math("minimum", low.clone(), high.clone());
            let hi = b.math("maximum", low.clone(), high.clone());
            let clamped = b.math("maximum", v.clone(), lo);
            let clamped = b.math("minimum", clamped, hi);
            let flag = b.greater(clamp, number(0.5));
            v = b.choose(flag, clamped, v);
            let range = b.math("subtract", high, low.clone());
            let abs = b.unary("absolute", range.clone());
            let tiny = b.greater(number(1e-6), abs);
            let denominator = b.choose(tiny, number(1.), range);
            let offset = b.math("subtract", v, low);
            let u = b.math("divide", offset, denominator);
            let span = b.math("subtract", end, start.clone());
            let scaled = b.math("multiply", u, span);
            b.math("add", start, scaled)
        }
        "sine_wave" => {
            name = "Sine wave";
            let rate = b.numeric(node, "subdivision", "Cycles per beat", Some(1.));
            let phase = b.numeric(node, "phase_deg", "Phase (degrees)", Some(0.));
            let amplitude = b.numeric(node, "amplitude", "Amplitude", Some(1.));
            let offset = b.numeric(node, "offset", "Offset", Some(0.));
            let beat = b.historical_beats(false);
            let time = b.math("multiply", beat, rate);
            let phase = b.math("divide", phase, number(360.));
            let time = b.math("add", time, phase);
            let wave = b.unary("sine", time);
            let wave = b.math("multiply", wave, amplitude);
            b.math("add", wave, offset)
        }
        "circle" | "figure_8" | "sweep" => {
            let speed = b.scalar(
                node,
                "speed",
                "Cycles per beat",
                if kind == "sweep" { 0.5 } else { 0.25 },
            );
            let beat = b.historical_beats(false);
            let phase = b.math("multiply", beat, speed);
            let (x, y) = match kind {
                "circle" => {
                    let radius = b.scalar(node, "radius", "Radius", 1.);
                    let cosine = b.cosine(phase.clone());
                    let sine = b.unary("sine", phase);
                    (
                        b.math("multiply", cosine, radius.clone()),
                        b.math("multiply", sine, radius),
                    )
                }
                "figure_8" => {
                    let width = b.scalar(node, "width", "Width", 1.);
                    let height = b.scalar(node, "height", "Height", 0.5);
                    let cosine = b.cosine(phase.clone());
                    let twice = b.math("multiply", phase, number(2.));
                    let sine = b.unary("sine", twice);
                    (
                        b.math("multiply", cosine, width),
                        b.math("multiply", sine, height),
                    )
                }
                _ => {
                    let angle = b.scalar(node, "angle", "Direction (degrees)", 0.);
                    let angle = b.math("divide", angle, number(360.));
                    let cosine = b.cosine(angle.clone());
                    let sine = b.unary("sine", angle);
                    let range = b.scalar(node, "range", "Range", 1.);
                    let motion = b.unary("sine", phase);
                    let motion = b.math("multiply", motion, range);
                    (
                        b.math("multiply", cosine, motion.clone()),
                        b.math("multiply", sine, motion),
                    )
                }
            };
            let uv = b.join(x, y);
            return b.finish(
                match kind {
                    "circle" => "Circle",
                    "figure_8" => "Figure eight",
                    _ => "Sweep",
                },
                [("uv", uv)],
            );
        }
        "color" => {
            name = "Color";
            let raw = node
                .params
                .get("color")
                .cloned()
                .unwrap_or(serde_json::json!("#ff0000"));
            let color = super::values::parse_node_color(&raw);
            b.input(
                "color",
                "Color",
                ValueType::Color,
                Some(Value::Color([
                    color[0] as f64,
                    color[1] as f64,
                    color[2] as f64,
                ])),
            )
        }
        "gradient" | "palette" => {
            name = if kind == "palette" {
                "Palette"
            } else {
                "Gradient"
            };
            let raw = node
                .params
                .get("value")
                .cloned()
                .unwrap_or(serde_json::json!({"colors":["#000000"]}));
            let stops = super::values::parse_stops(&parse_json(raw));
            let value = gradient(&stops);
            b.input("value", "Colors", ValueType::Gradient, Some(value))
        }
        "sample_palette" => {
            name = "Sample gradient";
            let stops = b.input("stops", "Gradient", ValueType::Gradient, None);
            let u = b.numeric(node, "u", "Position", None);
            let u = b.channel(u);
            b.call(
                "sample_gradient",
                [("gradient", stops), ("position", u)],
                "color",
            )
        }
        "rainbow" => {
            name = "Rainbow";
            let v = b.numeric(node, "in", "Hue", None);
            let v = b.channel(v);
            let offset = b.numeric(node, "offset", "Offset", Some(0.));
            let spread = b.numeric(node, "spread", "Spread", Some(1.));
            let saturation = b.numeric(node, "saturation", "Saturation", Some(1.));
            let saturation = b.clamp(saturation);
            let hue = b.math("multiply", v, spread);
            let hue = b.math("add", hue, offset);
            let hue = b.unary("fraction", hue);
            // HSL at lightness .5 is HSV with V=(1+S)/2 and S'=2S/(1+S).
            let one_plus = b.math("add", number(1.), saturation.clone());
            let value = b.math("divide", one_plus.clone(), number(2.));
            let twice = b.math("multiply", saturation, number(2.));
            let sat = b.math("divide", twice, one_plus);
            b.call(
                "hsv",
                [("hue", hue), ("saturation", sat), ("value", value)],
                "color",
            )
        }
        "apply_color" => {
            let mut raw = b.numeric(node, "signal", "Color", None);
            if let Some(width) = widths.get("signal").copied().flatten() {
                if width != 1 && width != 3 {
                    raw = b.resize(raw, width, 3);
                }
            }
            let rgb = b.math("multiply", raw, Value::Color([1.; 3]).into());
            let parts = b.node(
                "core/color_components",
                [("color", rgb), ("mask", Value::Proportion(1.).into())],
            );
            return b.finish(
                "Color output",
                [
                    ("color", wire(&parts, "color")),
                    ("dimmer", wire(&parts, "dimmer")),
                ],
            );
        }
        "apply_dimmer" | "apply_strobe" | "apply_speed" => {
            name = match kind {
                "apply_dimmer" => "Dimmer output",
                "apply_strobe" => "Strobe output",
                _ => "Movement speed",
            };
            let key = if kind == "apply_speed" {
                "speed"
            } else {
                "signal"
            };
            let value = b.numeric(node, key, "Value", None);
            let value = b.channel(value);
            if kind == "apply_speed" {
                b.greater(value, number(0.5))
            } else if kind == "apply_dimmer" {
                value
            } else {
                b.clamp(value)
            }
        }
        "apply_movement" => {
            let value = b.numeric(node, "uv", "Position", None);
            let width = widths
                .get("uv")
                .copied()
                .flatten()
                .ok_or("movement needs a signal with a known channel count")?;
            let pan = b.channel(value.clone());
            let tilt = b.channel_at(value, 1.min(width - 1));
            // The old evaluator passed these coordinates directly as angles.
            let pan = b.math("multiply", pan, Value::Degrees(1.).into());
            let tilt = b.math("multiply", tilt, Value::Degrees(1.).into());
            return b.finish("Movement output", [("pan", pan), ("tilt", tilt)]);
        }
        _ => return Err(format!("unsupported historical node {kind}")),
    };
    b.finish(name, [("out", out)])
}
fn parse_json(raw: serde_json::Value) -> serde_json::Value {
    if let Some(s) = raw.as_str() {
        if !s.starts_with('#') {
            return serde_json::from_str(s).unwrap_or(raw);
        }
    }
    raw
}
fn gradient(stops: &crate::models::node_graph::Stops) -> Value {
    let result = p::Gradient {
        stops: stops
            .stops
            .iter()
            .map(|(t, c)| p::ColorStop {
                alpha: f64::from(c[3]),
                t: *t as f64,
                color: [c[0] as f64, c[1] as f64, c[2] as f64],
            })
            .collect(),
    };
    Value::Gradient(result)
}

#[cfg(test)]
mod tests;
