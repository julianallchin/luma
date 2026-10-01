//! Pure edits of a clip graph, with no UI in them: labels, the resolved spec
//! of an input, promotion, links, unwiring and the clean-up after each. The
//! inspector, the timeline's fade handles and the tests all go through
//! these.

use std::collections::BTreeSet;

use luma_patterns as p;
use p::clip_graph::{definition, ClipGraph, Input, InputType, Kind, Node, Unit};

/// "Curve 2" for `curve2`: a kind's name then a number reads as the kind in
/// sentence case and its number. Any other id is a name someone gave the
/// node (`cut`, `bloom_far`) and reads as it is.
pub(crate) fn label(id: &str) -> String {
    let split = id.find(|c: char| c.is_ascii_digit()).unwrap_or(id.len());
    let (word, number) = id.split_at(split);
    match Kind::from_name(word) {
        Some(kind) if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{} {number}", kind.label())
        }
        _ => id.to_owned(),
    }
}

/// The output node's id.
pub(crate) fn output(graph: &ClipGraph) -> Option<String> {
    graph.output().map(|(id, _)| id.to_owned())
}

// -- units and types ----------------------------------------------------------

/// What a number field shows after the value. A share shows as a percent.
pub(crate) fn suffix(unit: Option<Unit>) -> Option<&'static str> {
    match unit? {
        Unit::Share => Some("%"),
        Unit::Beats => Some("beats"),
        Unit::Degrees => Some("°"),
        Unit::Metres => Some("m"),
        Unit::Hz => Some("Hz"),
        Unit::Heads => Some("heads"),
        Unit::Uvz | Unit::Rgb => None,
    }
}

/// What a field multiplies a stored number by to show it.
pub(crate) fn scale(unit: Option<Unit>) -> f64 {
    if unit == Some(Unit::Share) {
        100.
    } else {
        1.
    }
}

/// What an input holds, once a curve's bound is resolved to a number or a
/// vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    Number,
    Vector,
    Color,
    Points,
    Gradient,
    Heads,
    /// A wire from a time node: a shuffle's events.
    Time,
    Coordinate,
    /// A math node's items: numbers and value wires.
    Values,
}

/// An input's resolved type, unit and range, for one node in one graph: a
/// curve's low and high take their destination's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Spec {
    pub ty: Ty,
    pub unit: Option<Unit>,
    pub range: [f64; 2],
}

fn range(unit: Option<Unit>, bounds: Option<[Option<f64>; 2]>) -> [f64; 2] {
    let wide = match unit {
        // Degrees have no wide range: an aim angle has its own, and a phase
        // takes any number and wraps.
        Some(Unit::Share) => [-4., 4.],
        Some(Unit::Beats) => [-1e4, 1e4],
        Some(Unit::Metres) => [-1e3, 1e3],
        Some(Unit::Heads) => [1., 1e4],
        _ => [-1e6, 1e6],
    };
    let [low, high] = bounds.unwrap_or([None, None]);
    let low = match (low, unit) {
        // A value above 0 is at least the smallest step a field makes.
        (Some(low), Some(Unit::Beats)) if low <= 0. => wide[0],
        (Some(low), Some(Unit::Share)) if low <= 0. && high.is_none() => 0.01,
        (Some(low), _) => low,
        (None, _) => wide[0],
    };
    [low, high.unwrap_or(wide[1])]
}

pub(crate) fn spec(graph: &ClipGraph, id: &str, input: &str) -> Option<Spec> {
    let node = graph.nodes.get(id)?;
    let def = definition(node.kind).input(input)?;
    let ty = match def.ty {
        InputType::Number => Ty::Number,
        InputType::Vector => Ty::Vector,
        InputType::Color => Ty::Color,
        InputType::Points => Ty::Points,
        InputType::Gradient => Ty::Gradient,
        InputType::Heads => Ty::Heads,
        InputType::Time => Ty::Time,
        InputType::Coordinate => Ty::Coordinate,
        InputType::Values => Ty::Values,
        // A value node's own value: the type its wires ask of it, in the
        // unit and range of the input it lands in.
        InputType::Constant => {
            let mut spec = landing(graph, id).unwrap_or(Spec {
                ty: Ty::Number,
                unit: None,
                range: [-1e6, 1e6],
            });
            spec.ty = match graph.value_kind(id) {
                Some("vector") => Ty::Vector,
                Some("color") => Ty::Color,
                _ => Ty::Number,
            };
            return Some(spec);
        }
        InputType::Bound => {
            let mut spec = landing(graph, id).unwrap_or(Spec {
                ty: Ty::Number,
                unit: None,
                range: [-1e6, 1e6],
            });
            spec.ty = if node.setting("kind") == Some("vector") {
                Ty::Vector
            } else {
                Ty::Number
            };
            return Some(spec);
        }
    };
    Some(Spec {
        ty,
        unit: def.unit,
        range: range(def.unit, def.range),
    })
}

/// The spec of the input value node `id` (a curve, a math or a value) lands in,
/// through any math nodes between: a curve that is an item of a product
/// into a brightness takes the brightness's unit.
fn landing(graph: &ClipGraph, id: &str) -> Option<Spec> {
    let mut at = id.to_owned();
    // A graph that checks has no cycle; the bound keeps one from spinning.
    for _ in 0..=graph.nodes.len() {
        let (to, name) = destination(graph, &at)?;
        let spec = spec(graph, &to, &name)?;
        if spec.ty != Ty::Values {
            return Some(spec);
        }
        at = to;
    }
    None
}

/// What an empty input stands for, when that is a value a field can show:
/// the node reference's "Empty" column. `None` where empty means something no
/// value says: best fit, uniform, one fixture, same as every.
pub(crate) fn empty(graph: &ClipGraph, id: &str, input: &str) -> Option<Input> {
    let node = graph.nodes.get(id)?;
    let vector = node.setting("kind") == Some("vector");
    Some(match (node.kind, input) {
        (Kind::Color, "color") => Input::Color([1., 1., 1.]),
        (Kind::Color | Kind::Aim | Kind::Strobe, "alpha") | (Kind::Color, "brightness") => {
            Input::Number(1.)
        }
        (Kind::Aim, "direction") => Input::Vector([0., 0.766, -0.643]),
        (Kind::Aim, "point") => Input::Vector([0., 0., 0.]),
        (Kind::Aim, "yaw" | "pitch") => Input::Number(0.),
        (Kind::Strobe, "rate") => Input::Number(0.5),
        (Kind::Time, "delay" | "phase") | (Kind::Space, "at" | "shift") => Input::Number(0.),
        (Kind::Space, "centre") => Input::Vector([0.5; 3]),
        (Kind::Space, "scale") => Input::Number(1.),
        (Kind::Mirror, "at") => Input::Number(0.5),
        (Kind::Noise, "speed") => Input::Number(4.),
        (Kind::Noise, "contrast") => Input::Number(0.),
        (Kind::Audio, "low_hz") => Input::Number(40.),
        (Kind::Audio, "high_hz") => Input::Number(100.),
        (Kind::Curve, "shape") => Input::Points(preset_curve("Ramp up")),
        (Kind::Curve, "low") if !vector => Input::Number(0.),
        (Kind::Curve, "high") if !vector => Input::Number(1.),
        _ => return None,
    })
}

/// What an empty input with no value to show means, in a word or two.
pub(crate) fn empty_note(node: &Node, input: &str) -> &'static str {
    match (node.kind, input) {
        (_, "direction") => "Best fit",
        (Kind::Noise, "scale") => "Uniform",
        (Kind::Group, "size") => "One fixture",
        (Kind::Time, "every") => "Once",
        (Kind::Time, "duration") if node.inputs.contains_key("every") => "Same as every",
        (Kind::Time, "duration") => "Whole clip",
        (Kind::Curve, "gradient") => "Needs colors",
        (Kind::Curve, _) => "Needs a value",
        _ => "Empty",
    }
}

/// A value to give an input that has none and no value to show: what the
/// "Value" pick writes there.
pub(crate) fn first_value(graph: &ClipGraph, id: &str, input: &str) -> Option<Input> {
    let spec = spec(graph, id, input)?;
    let node = graph.nodes.get(id)?;
    Some(match spec.ty {
        Ty::Number => Input::Number(match (node.kind, input) {
            (Kind::Time, "duration") => match node.inputs.get("every") {
                Some(Input::Number(every)) => *every,
                _ => 1.,
            },
            (Kind::Noise, "scale") => 0.5,
            _ => 1.,
        }),
        Ty::Vector if spec.unit == Some(Unit::Metres) => Input::Vector([0., 0., 0.]),
        Ty::Vector if spec.unit == Some(Unit::Share) => Input::Vector([0.5; 3]),
        Ty::Vector => Input::Vector([1., 0., 0.]),
        Ty::Color => Input::Color([1., 1., 1.]),
        Ty::Points => Input::Points(preset_curve("Ramp up")),
        Ty::Gradient => Input::Gradient(gradient(&[([0., 0., 0.], 0.), ([1., 1., 1.], 1.)])),
        Ty::Heads | Ty::Time | Ty::Coordinate | Ty::Values => return None,
    })
}

/// An input's value as shown: its own, or what empty stands for. A math
/// node's list shows its items one by one. A value node's three numbers
/// show as a color or a vector by what it feeds.
pub(crate) fn shown(graph: &ClipGraph, id: &str, input: &str) -> Option<Input> {
    let node = graph.nodes.get(id)?;
    match node.inputs.get(input) {
        Some(Input::Wire(_) | Input::List(_)) => None,
        Some(Input::Vector(v) | Input::Color(v)) if node.kind == Kind::Value => {
            Some(match graph.value_kind(id) {
                Some("color") => Input::Color(*v),
                _ => Input::Vector(*v),
            })
        }
        Some(value) => Some(value.clone()),
        None => empty(graph, id, input),
    }
}

// -- structure ----------------------------------------------------------------

/// Every place that wires `target`: (node, input), in stored order.
pub(crate) fn references(graph: &ClipGraph, target: &str) -> Vec<(String, String)> {
    graph
        .nodes
        .iter()
        .flat_map(|(id, node)| {
            node.wires()
                .filter(|(_, from)| *from == target)
                .map(move |(name, _)| (id.clone(), name.to_owned()))
        })
        .collect()
}

/// Every wire the output reaches, depth first from the output, inputs in row
/// order: (node, input, the node wired there).
pub(crate) fn wires_in_order(graph: &ClipGraph) -> Vec<(String, String, String)> {
    fn walk(
        graph: &ClipGraph,
        id: &str,
        seen: &mut BTreeSet<String>,
        wires: &mut Vec<(String, String, String)>,
    ) {
        if !seen.insert(id.to_owned()) {
            return;
        }
        let Some(node) = graph.nodes.get(id) else {
            return;
        };
        for (name, _) in &definition(node.kind).inputs {
            for to in node.inputs.get(*name).into_iter().flat_map(Input::sources) {
                wires.push((id.to_owned(), (*name).to_owned(), to.to_owned()));
                walk(graph, to, seen, wires);
            }
        }
    }
    let mut wires = Vec::new();
    if let Some(root) = output(graph) {
        walk(graph, &root, &mut BTreeSet::new(), &mut wires);
    }
    wires
}

/// Where `id` is first wired, in row order from the output: the node and
/// input it feeds there.
pub(crate) fn destination(graph: &ClipGraph, id: &str) -> Option<(String, String)> {
    wires_in_order(graph)
        .into_iter()
        .find(|(_, _, to)| to == id)
        .map(|(from, name, _)| (from, name))
        .or_else(|| references(graph, id).into_iter().next())
}

/// The nodes `id` reaches through its inputs, itself included.
fn upstream(graph: &ClipGraph, id: &str) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let mut stack = vec![id.to_owned()];
    while let Some(at) = stack.pop() {
        if !reached.insert(at.clone()) {
            continue;
        }
        if let Some(node) = graph.nodes.get(&at) {
            stack.extend(node.wires().map(|(_, from)| from.to_owned()));
        }
    }
    reached
}

/// Drop every node the output no longer reaches.
pub(crate) fn prune(graph: &mut ClipGraph) {
    let Some(root) = output(graph) else {
        return;
    };
    let keep = upstream(graph, &root);
    graph.nodes.retain(|id, _| keep.contains(id));
}

/// A signature of the graph's shape: kinds, ids, settings and wires. Clips
/// of one shape are edited together.
pub(crate) fn shape(graph: &ClipGraph) -> String {
    signature(graph, false)
}

/// The shape and which inputs hold a value: what the widgets are built for.
/// An input that gains or loses its value is a new widget.
pub(crate) fn layout(graph: &ClipGraph) -> String {
    signature(graph, true)
}

fn signature(graph: &ClipGraph, values: bool) -> String {
    let mut out = String::new();
    for (id, node) in &graph.nodes {
        out.push_str(&format!("{id}:{}", node.kind.name()));
        for (name, setting) in &node.settings {
            out.push_str(&format!(" {name}={setting}"));
        }
        for (name, input) in &node.inputs {
            match input {
                Input::Wire(to) => out.push_str(&format!(" {name}<{to}")),
                Input::List(items) => {
                    out.push_str(&format!(" {name}["));
                    for item in items {
                        match item.source() {
                            Some(to) => out.push_str(&format!("<{to}")),
                            None => out.push('#'),
                        }
                    }
                    out.push(']');
                }
                _ if values => out.push_str(&format!(" {name}")),
                _ => {}
            }
        }
        out.push(';');
    }
    out
}

// -- edits --------------------------------------------------------------------

/// Where a promoted input's values come from: the coordinate node kinds.
pub(crate) const SOURCES: [Kind; 4] = [Kind::Time, Kind::Space, Kind::Noise, Kind::Audio];

/// A coordinate kind's menu entry.
pub(crate) fn source_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Time => "Over time",
        Kind::Space => "Over space",
        other => other.label(),
    }
}

/// The shapers a heads input takes.
pub(crate) const SHAPERS: [Kind; 4] = [Kind::Mirror, Kind::Shuffle, Kind::Group, Kind::Split];

pub(crate) fn set_input(graph: &mut ClipGraph, id: &str, input: &str, value: Option<Input>) {
    let Some(node) = graph.nodes.get_mut(id) else {
        return;
    };
    match value {
        Some(value) => node.inputs.insert(input.to_owned(), value),
        None => node.inputs.remove(input),
    };
    prune(graph);
}

/// Set a setting. A space's wrap follows its kind while it sits at the old
/// kind's default: an angle wraps, a line does not.
pub(crate) fn set_setting(graph: &mut ClipGraph, id: &str, name: &str, value: &str) {
    let Some(node) = graph.nodes.get_mut(id) else {
        return;
    };
    let wrap = node.setting("wrap").map(str::to_owned);
    let was = node.settings.insert(name.to_owned(), value.to_owned());
    if node.kind == Kind::Space && name == "kind" {
        let wraps = |kind: Option<&str>| if kind == Some("angle") { "yes" } else { "no" };
        if wrap.as_deref() == Some(wraps(was.as_deref())) {
            node.settings
                .insert("wrap".into(), wraps(Some(value)).into());
        }
    }
}

fn add(graph: &mut ClipGraph, node: Node) -> String {
    let id = graph.next_id(node.kind);
    graph.nodes.insert(id.clone(), node);
    id
}

pub(crate) fn gradient(stops: &[([f64; 3], f64)]) -> p::Gradient {
    p::Gradient {
        stops: stops
            .iter()
            .map(|(color, t)| p::ColorStop {
                t: *t,
                color: *color,
                alpha: 1.,
            })
            .collect(),
    }
}

/// `v` turned `degrees` toward +Z, keeping its length. A vector that already
/// points up turns toward +V instead.
fn turned_up(v: [f64; 3], degrees: f64) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < 1e-9 {
        return [0., 0., 1.];
    }
    let n = v.map(|c| c / length);
    let up = [-n[2] * n[0], -n[2] * n[1], 1. - n[2] * n[2]];
    let norm = (up[0] * up[0] + up[1] * up[1] + up[2] * up[2]).sqrt();
    let up = if norm < 1e-6 {
        [0., 1., 0.]
    } else {
        up.map(|c| c / norm)
    };
    let (sin, cos) = degrees.to_radians().sin_cos();
    [0, 1, 2].map(|i| (cos * n[i] + sin * up[i]) * length)
}

/// A named shipped curve shape.
pub(crate) fn preset_curve(name: &str) -> p::Envelope {
    super::presets::curve(name).unwrap_or_else(|| p::Envelope::linear(vec![[0., 0.], [1., 1.]]))
}

fn curve(kind: &str, shape: &str) -> Node {
    Node::new(Kind::Curve)
        .with_setting("kind", kind)
        .with_input("shape", preset_curve(shape))
}

/// The curve a promoted input takes: the defaults table of spec 7.2, so the
/// effect shows at once. `current` is the input's value before.
fn promoted(spec: Spec, current: Option<&Input>) -> Node {
    let now = match current {
        Some(Input::Number(v)) => Some(*v),
        _ => None,
    };
    let bounds = |shape: &str, low: f64, high: f64| {
        curve("number", shape)
            .with_input("low", low)
            .with_input("high", high)
    };
    match (spec.ty, spec.unit) {
        (Ty::Color, _) => {
            let color = match current {
                Some(Input::Color(color)) => *color,
                _ => [1., 1., 1.],
            };
            curve("color", "Ramp up")
                .with_input("gradient", gradient(&[([0., 0., 0.], 0.), (color, 1.)]))
        }
        (Ty::Vector, unit) => {
            let v = match current {
                Some(Input::Vector(v)) => *v,
                _ => [1., 0., 0.],
            };
            let (low, high) = if unit == Some(Unit::Metres) {
                (v.map(|c| c - 1.), v.map(|c| c + 1.))
            } else {
                (v, turned_up(v, 30.))
            };
            curve("vector", "Ramp up")
                .with_input("low", Input::Vector(low))
                .with_input("high", Input::Vector(high))
        }
        (_, Some(Unit::Share)) => {
            let v = now.unwrap_or(1.);
            bounds("Ramp up", 0., if v == 0. { 1. } else { v })
        }
        (_, Some(Unit::Degrees)) => bounds("Sine", -30., 30.),
        (_, Some(Unit::Beats)) => {
            let v = now.unwrap_or(1.).max(1. / 32.);
            bounds("Ramp down", v / 2., v)
        }
        (_, Some(Unit::Metres)) => {
            let v = now.unwrap_or(0.);
            bounds("Ramp up", v - 1., v + 1.)
        }
        (_, Some(Unit::Hz)) => {
            let v = now.unwrap_or(100.);
            bounds("Ramp up", (v / 2.).max(20.), (v * 2.).min(20000.))
        }
        (_, Some(Unit::Heads)) => bounds("Ramp up", 1., now.unwrap_or(1.).max(2.)),
        _ => bounds("Ramp up", 0., 1.),
    }
}

/// Promote a value input to `source`, a coordinate kind: a coordinate node
/// and a curve wired into the input, or into a list as one more item
/// ([`link`]). Returns the value it held, for an unwire to give back.
pub(crate) fn promote(graph: &mut ClipGraph, id: &str, input: &str, source: Kind) -> Option<Input> {
    let spec = spec(graph, id, input)?;
    let held = graph
        .nodes
        .get(id)?
        .inputs
        .get(input)
        .filter(|held| !matches!(held, Input::Wire(_) | Input::List(_)))
        .cloned();
    let current = shown(graph, id, input);
    let curve = match (graph.nodes[id].kind, input) {
        (Kind::Space, "shift") => {
            // A shift that moves slides the shape in from wholly before the
            // axis to past its end: from minus its scale to 1.
            let scale = match shown(graph, id, "scale") {
                Some(Input::Number(scale)) => scale,
                _ => 1.,
            };
            curve("number", "Ramp up")
                .with_input("low", -scale)
                .with_input("high", 1.)
        }
        // A phase is in degrees as an aim angle is, but it wraps at 360: a
        // phase that moves spreads the heads over half an event.
        (Kind::Time, "phase") => curve("number", "Ramp up")
            .with_input("low", 0.)
            .with_input("high", 180.),
        _ => promoted(spec, current.as_ref()),
    };
    let x = add(graph, Node::new(source));
    let curve = add(graph, curve.with_input("x", Input::wire(x)));
    link(graph, id, input, &curve);
    held
}

/// Point a curve's `x` at a new coordinate node of `source`.
pub(crate) fn recoordinate(graph: &mut ClipGraph, curve: &str, source: Kind) {
    let x = add(graph, Node::new(source));
    if let Some(node) = graph.nodes.get_mut(curve) {
        node.inputs.insert("x".into(), Input::wire(x));
    }
    prune(graph);
}

/// Wire a new node of `kind` (a shaper, or a time) into a heads or time
/// input. A new time has events every beat.
pub(crate) fn insert(graph: &mut ClipGraph, id: &str, input: &str, kind: Kind) {
    let mut new = Node::new(kind);
    if kind == Kind::Time {
        new.inputs.insert("every".into(), Input::Number(1.));
    }
    let new = add(graph, new);
    if let Some(node) = graph.nodes.get_mut(id) {
        node.inputs.insert(input.to_owned(), Input::wire(new));
    }
    prune(graph);
}

/// Wire an existing node into an input: the two places share it. A math
/// node's values take the wire as one more item; any other input holds only
/// the wire after.
pub(crate) fn link(graph: &mut ClipGraph, id: &str, input: &str, target: &str) {
    let listed = spec(graph, id, input).is_some_and(|spec| spec.ty == Ty::Values);
    if let Some(node) = graph.nodes.get_mut(id) {
        match node.inputs.get_mut(input) {
            Some(Input::List(items)) if listed => items.push(Input::wire(target)),
            _ if listed => {
                node.inputs
                    .insert(input.to_owned(), Input::List(vec![Input::wire(target)]));
            }
            _ => {
                node.inputs.insert(input.to_owned(), Input::wire(target));
            }
        }
    }
    prune(graph);
}

/// Rename node `from` to `to`, and every wire into it, single or a list's
/// item, with it. The checker says which names a node may take; a name
/// another node has is refused here, since ids are the map's keys. On an
/// error the graph is as it was, and the error says why in a sentence.
pub(crate) fn rename(graph: &mut ClipGraph, from: &str, to: &str) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    if graph.nodes.contains_key(to) {
        return Err(format!("Another node is named {to}"));
    }
    let mut renamed = graph.clone();
    let Some(node) = renamed.nodes.remove(from) else {
        return Err(format!("No node is named {from}"));
    };
    renamed.nodes.insert(to.to_owned(), node);
    for node in renamed.nodes.values_mut() {
        for input in node.inputs.values_mut() {
            let items = match input {
                Input::List(items) => items.as_mut_slice(),
                one => std::slice::from_mut(one),
            };
            for item in items {
                if item.source() == Some(from) {
                    *item = Input::wire(to);
                }
            }
        }
    }
    renamed.check().map_err(|error| {
        let error = error.to_string();
        let error = error.strip_prefix("graph: ").unwrap_or(&error);
        let mut chars = error.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
        })
    })?;
    *graph = renamed;
    Ok(())
}

/// Whether an input can be multiplied ([`multiply`]): a number, or a
/// vector or color that a value node feeds, since a math node's items are
/// numbers and value wires.
pub(crate) fn can_multiply(graph: &ClipGraph, id: &str, input: &str) -> bool {
    let wired = graph
        .nodes
        .get(id)
        .and_then(|node| node.inputs.get(input))
        .and_then(Input::source)
        .is_some();
    match spec(graph, id, input).map(|spec| spec.ty) {
        Some(Ty::Number) => true,
        Some(Ty::Vector | Ty::Color) => wired,
        _ => false,
    }
}

/// Multiply an input by one more item, a 1: what it holds (its value, what
/// empty stands for, or its wire) and the 1 become the items of a new `*`
/// math node wired into it. A product that feeds only this input takes the
/// 1 as one more item instead.
pub(crate) fn multiply(graph: &mut ClipGraph, id: &str, input: &str) {
    if !can_multiply(graph, id, input) {
        return;
    }
    let held = graph
        .nodes
        .get(id)
        .and_then(|node| node.inputs.get(input).cloned())
        .or_else(|| shown(graph, id, input));
    if let Some(product) = held.as_ref().and_then(Input::source).filter(|to| {
        graph
            .nodes
            .get(*to)
            .is_some_and(|node| node.kind == Kind::Math && node.setting("op") == Some("*"))
            && references(graph, to).len() == 1
    }) {
        let product = product.to_owned();
        if let Some(Input::List(items)) = graph
            .nodes
            .get_mut(&product)
            .and_then(|node| node.inputs.get_mut("values"))
        {
            items.push(Input::Number(1.));
        }
        return;
    }
    let first = match held {
        Some(item @ (Input::Number(_) | Input::Wire(_))) => item,
        _ => Input::Number(1.),
    };
    let math = add(
        graph,
        Node::new(Kind::Math)
            .with_setting("op", "*")
            .with_input("values", Input::List(vec![first, Input::Number(1.)])),
    );
    if let Some(node) = graph.nodes.get_mut(id) {
        node.inputs.insert(input.to_owned(), Input::wire(math));
    }
}

/// Add an item, a 1, to a math node's values.
pub(crate) fn add_item(graph: &mut ClipGraph, id: &str, input: &str) {
    let Some(node) = graph.nodes.get_mut(id) else {
        return;
    };
    match node.inputs.get_mut(input) {
        Some(Input::List(items)) => items.push(Input::Number(1.)),
        _ => {
            node.inputs
                .insert(input.to_owned(), Input::List(vec![Input::Number(1.)]));
        }
    }
}

/// Set item `index` of a math node's values.
pub(crate) fn set_item(graph: &mut ClipGraph, id: &str, input: &str, index: usize, value: Input) {
    if let Some(Input::List(items)) = graph
        .nodes
        .get_mut(id)
        .and_then(|node| node.inputs.get_mut(input))
    {
        if let Some(item) = items.get_mut(index) {
            *item = value;
        }
    }
}

/// Take item `index` out of a math node's values; see [`settle`].
pub(crate) fn remove_item(graph: &mut ClipGraph, id: &str, input: &str, index: usize) {
    if let Some(Input::List(items)) = graph
        .nodes
        .get_mut(id)
        .and_then(|node| node.inputs.get_mut(input))
    {
        if index < items.len() {
            items.remove(index);
        }
    }
    settle(graph, id);
    prune(graph);
}

/// A math node left with one item gives way to it: each input it fed takes
/// that item, a number or a wire, and the math node goes. With none left,
/// those inputs go empty. A number cannot stand for a vector or a color, so
/// such an input goes empty too.
fn settle(graph: &mut ClipGraph, math: &str) {
    let Some(node) = graph.nodes.get(math).filter(|node| node.kind == Kind::Math) else {
        return;
    };
    let items = match node.inputs.get("values") {
        Some(Input::List(items)) => items.clone(),
        Some(one) => vec![one.clone()],
        None => Vec::new(),
    };
    if items.len() >= 2 {
        return;
    }
    let item = items.into_iter().next();
    for (to, name) in references(graph, math) {
        let number_only =
            spec(graph, &to, &name).is_some_and(|spec| matches!(spec.ty, Ty::Vector | Ty::Color));
        let item = item
            .clone()
            .filter(|item| !(number_only && matches!(item, Input::Number(_))));
        let Some(node) = graph.nodes.get_mut(&to) else {
            continue;
        };
        match (node.inputs.get_mut(&name), item) {
            (Some(Input::List(list)), item) => {
                let at = list.iter().position(|it| it.source() == Some(math));
                if let Some(at) = at {
                    match item {
                        Some(item) => list[at] = item,
                        None => {
                            list.remove(at);
                        }
                    }
                }
            }
            (_, Some(item)) => {
                node.inputs.insert(name, item);
            }
            (_, None) => {
                node.inputs.remove(&name);
            }
        }
    }
    graph.nodes.remove(math);
}

/// The nodes an input may link to: those of the wire type it takes, that it
/// does not already hold, and that would not make a cycle.
pub(crate) fn link_candidates(graph: &ClipGraph, id: &str, input: &str) -> Vec<String> {
    let Some(spec) = spec(graph, id, input) else {
        return Vec::new();
    };
    let held: Vec<&str> = graph
        .nodes
        .get(id)
        .and_then(|node| node.inputs.get(input))
        .into_iter()
        .flat_map(Input::sources)
        .collect();
    graph
        .ids_in_order()
        .into_iter()
        .filter(|target| !held.contains(target) && !upstream(graph, target).contains(id))
        // A value already wired keeps one unit.
        .filter(|target| {
            graph.nodes[*target].kind != Kind::Value
                || spec.ty == Ty::Values
                || landing(graph, target).is_none_or(|landed| landed.unit == spec.unit)
        })
        .filter(|target| {
            let node = &graph.nodes[*target];
            let value = graph.value_kind(target);
            match spec.ty {
                Ty::Number => value == Some("number"),
                Ty::Vector => value == Some("vector"),
                Ty::Color => value == Some("color"),
                Ty::Values => value.is_some(),
                Ty::Heads => SHAPERS.contains(&node.kind),
                Ty::Time => node.kind == Kind::Time,
                Ty::Coordinate => SOURCES.contains(&node.kind),
                Ty::Points | Ty::Gradient => false,
            }
        })
        .map(str::to_owned)
        .collect()
}

/// Whether an input needs its wire: a curve's `x` has no value to go back
/// to, so taking its source away takes the curve too.
fn needs_wire(graph: &ClipGraph, id: &str, input: &str) -> bool {
    spec(graph, id, input).is_some_and(|spec| spec.ty == Ty::Coordinate)
}

/// Delete node `id`. Each input it fed takes `restore`'s value (its last
/// value, or empty), and a math node's values lose the node's items (see
/// [`settle`]); a curve whose `x` it was goes too. The output node stays.
pub(crate) fn delete(
    graph: &mut ClipGraph,
    id: &str,
    restore: impl Fn(&ClipGraph, &str, &str) -> Option<Input>,
) {
    if graph.nodes.get(id).is_none_or(|node| node.kind.is_output()) {
        return;
    }
    let mut doomed = vec![id.to_owned()];
    while let Some(at) = doomed.pop() {
        // A value node gives each input it fed its value back.
        let value = graph
            .nodes
            .get(&at)
            .filter(|node| node.kind == Kind::Value)
            .and_then(|node| node.inputs.get("value").cloned());
        for (to, input) in references(graph, &at) {
            if needs_wire(graph, &to, &input) {
                doomed.push(to);
                continue;
            }
            if let Some(value) = &value {
                let value = match (value, spec(graph, &to, &input).map(|spec| spec.ty)) {
                    (Input::Vector(v), Some(Ty::Color)) => Input::Color(*v),
                    (Input::Color(v), Some(Ty::Vector)) => Input::Vector(*v),
                    (value, _) => value.clone(),
                };
                let node = graph.nodes.get_mut(&to).expect("a reference");
                let held = node.inputs.get_mut(&input).expect("a reference");
                let items = match held {
                    Input::List(items) => items.as_mut_slice(),
                    one => std::slice::from_mut(one),
                };
                for item in items {
                    if item.source() == Some(at.as_str()) {
                        *item = value.clone();
                    }
                }
                continue;
            }
            if let Some(Input::List(items)) = graph
                .nodes
                .get_mut(&to)
                .and_then(|node| node.inputs.get_mut(&input))
            {
                items.retain(|item| item.source() != Some(at.as_str()));
                settle(graph, &to);
                continue;
            }
            let back = restore(graph, &to, &input);
            if let Some(node) = graph.nodes.get_mut(&to) {
                match back {
                    Some(value) => node.inputs.insert(input, value),
                    None => node.inputs.remove(&input),
                };
            }
        }
        graph.nodes.remove(&at);
    }
    prune(graph);
}

/// Whether a new node of `kind` can feed an input: directly (a value node
/// takes any number, vector or color), or through the curve a promotion
/// puts between them.
pub(crate) fn accepts(graph: &ClipGraph, id: &str, input: &str, kind: Kind) -> bool {
    let Some(spec) = spec(graph, id, input) else {
        return false;
    };
    match spec.ty {
        Ty::Number | Ty::Vector | Ty::Color | Ty::Values if kind == Kind::Value => true,
        Ty::Number | Ty::Vector | Ty::Color if kind == Kind::Math => can_multiply(graph, id, input),
        Ty::Number | Ty::Vector | Ty::Color | Ty::Values => {
            SOURCES.contains(&kind) || kind == Kind::Curve
        }
        Ty::Coordinate => SOURCES.contains(&kind),
        Ty::Heads => SHAPERS.contains(&kind),
        Ty::Time => kind == Kind::Time,
        Ty::Points | Ty::Gradient => false,
    }
}

/// Wire a new node of `kind` into an input, with what it needs to check: a
/// coordinate into a value input comes through a new curve, a new curve
/// reads a new time, a new math multiplies what the input held by 1, and a
/// new value holds what the input held.
/// Returns `None` when `kind` cannot feed the input, else the value the
/// input held, for an unwire to give back.
pub(crate) fn attach(
    graph: &mut ClipGraph,
    id: &str,
    input: &str,
    kind: Kind,
) -> Option<Option<Input>> {
    if !accepts(graph, id, input, kind) {
        return None;
    }
    let ty = spec(graph, id, input)?.ty;
    Some(match ty {
        Ty::Number | Ty::Vector | Ty::Color | Ty::Values if kind == Kind::Value => {
            let held = graph
                .nodes
                .get(id)?
                .inputs
                .get(input)
                .filter(|held| !matches!(held, Input::Wire(_) | Input::List(_)))
                .cloned();
            let value = match ty {
                Ty::Values => Input::Number(1.),
                _ => shown(graph, id, input).or_else(|| first_value(graph, id, input))?,
            };
            let value = add(graph, Node::new(Kind::Value).with_input("value", value));
            link(graph, id, input, &value);
            held
        }
        Ty::Number | Ty::Vector | Ty::Color if kind == Kind::Math => {
            let held = graph
                .nodes
                .get(id)?
                .inputs
                .get(input)
                .filter(|held| !matches!(held, Input::Wire(_)))
                .cloned();
            multiply(graph, id, input);
            held
        }
        Ty::Number | Ty::Vector | Ty::Color | Ty::Values => {
            let source = if kind == Kind::Curve {
                Kind::Time
            } else {
                kind
            };
            promote(graph, id, input, source)
        }
        Ty::Coordinate => {
            recoordinate(graph, id, kind);
            None
        }
        _ => {
            insert(graph, id, input, kind);
            None
        }
    })
}

/// The canvas's columns, right to left: the output alone, then each card
/// in the column one past the farthest card it feeds, so every wire runs
/// left to right. Within a column, cards follow the rows they feed in the
/// column nearest them, and a math node's items in their order, so wires
/// cross as little as the order allows. The same graph always gives the
/// same columns.
pub(crate) fn columns(graph: &ClipGraph) -> Vec<Vec<String>> {
    let Some(root) = output(graph) else {
        return Vec::new();
    };
    let mut depth: std::collections::BTreeMap<String, usize> = [(root.clone(), 0)].into();
    // A graph that checks has no cycle; the bound keeps one from spinning.
    for _ in 0..=graph.nodes.len() {
        let mut changed = false;
        for (id, node) in &graph.nodes {
            let Some(&at) = depth.get(id) else {
                continue;
            };
            for (_, from) in node.wires() {
                if !graph.nodes.contains_key(from) {
                    continue;
                }
                if depth.get(from).is_none_or(|&d| d < at + 1) {
                    depth.insert(from.to_owned(), at + 1);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let count = depth.values().max().map_or(0, |deepest| deepest + 1);
    let mut columns: Vec<Vec<String>> = vec![Vec::new(); count];
    columns[0].push(root);
    for column in 1..count {
        let mut keyed: Vec<((usize, usize, usize, usize), String)> = graph
            .ids_in_order()
            .into_iter()
            .filter(|id| depth.get(*id) == Some(&column))
            .map(|id| {
                let key = references(graph, id)
                    .into_iter()
                    .filter_map(|(to, input)| {
                        let at = *depth.get(&to)?;
                        let row = columns[at].iter().position(|placed| *placed == to)?;
                        let node = graph.nodes.get(&to)?;
                        let slot = definition(node.kind)
                            .inputs
                            .iter()
                            .position(|(name, _)| *name == input)?;
                        let item = match node.inputs.get(&input) {
                            Some(Input::List(items)) => items
                                .iter()
                                .position(|item| item.source() == Some(id))
                                .unwrap_or(0),
                            _ => 0,
                        };
                        Some((column - at, row, slot, item))
                    })
                    .min()
                    .unwrap_or((usize::MAX, 0, 0, 0));
                (key, id.to_owned())
            })
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        columns[column] = keyed.into_iter().map(|(_, id)| id).collect();
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wash() -> ClipGraph {
        ClipGraph::new([("color1".to_owned(), Node::new(Kind::Color))])
    }

    fn wired(graph: &ClipGraph, id: &str, input: &str) -> String {
        graph.nodes[id].inputs[input].source().unwrap().to_owned()
    }

    #[test]
    fn labels_read_as_kind_and_number() {
        assert_eq!(label("curve2"), "Curve 2");
        assert_eq!(label("color1"), "Color 1");
        // A name someone gave reads as it is, digits and all.
        for named in ["cut", "bloom_far", "cut2", "curve2b", "curve_2"] {
            assert_eq!(label(named), named);
        }
    }

    #[test]
    fn a_rename_moves_every_wire_and_refuses_what_the_checker_refuses() {
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Time);
        let curve = wired(&graph, "color1", "brightness");
        link(&mut graph, "color1", "alpha", &curve);
        multiply(&mut graph, "color1", "brightness");
        let product = wired(&graph, "color1", "brightness");
        let before = graph.clone();
        for bad in ["time", "2x", "class", "a-b", &"x".repeat(33), "color1"] {
            assert!(rename(&mut graph, &curve, bad).is_err(), "{bad}");
            assert_eq!(graph, before, "{bad} left the graph as it was");
        }
        rename(&mut graph, &curve, "cut").unwrap();
        assert!(!graph.nodes.contains_key(&curve));
        assert_eq!(wired(&graph, "color1", "alpha"), "cut");
        let Some(Input::List(items)) = graph.nodes[&product].inputs.get("values") else {
            panic!("a product");
        };
        assert_eq!(items[0].source(), Some("cut"));
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        // A fresh node never takes a given name, and still counts from 1.
        promote(&mut graph, "color1", "alpha", Kind::Time);
        assert_eq!(wired(&graph, "color1", "alpha"), "curve1");
    }

    #[test]
    fn a_space_shows_no_shift_at_scale_one_and_a_moving_shift_sweeps_through() {
        let mut graph = chase();
        let curve = wired(&graph, "color1", "brightness");
        let space = wired(&graph, &curve, "x");
        let number = |graph: &ClipGraph, id: &str, input: &str| match shown(graph, id, input) {
            Some(Input::Number(v)) => v,
            other => panic!("{id}.{input}: {other:?}"),
        };
        assert_eq!(number(&graph, &space, "shift"), 0.);
        assert_eq!(number(&graph, &space, "scale"), 1.);
        set_input(&mut graph, &space, "scale", Some(Input::Number(0.25)));
        promote(&mut graph, &space, "shift", Kind::Time);
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        let sweep = wired(&graph, &space, "shift");
        // From wholly before the axis to wholly past it.
        assert!(number(&graph, &sweep, "low") <= -0.25);
        assert!(number(&graph, &sweep, "high") >= 1.);
    }

    #[test]
    fn a_moving_phase_ramps_over_half_an_event_and_an_aim_angle_swings_about_zero() {
        let bounds = |graph: &ClipGraph, curve: &str| {
            ["low", "high"].map(|bound| match shown(graph, curve, bound) {
                Some(Input::Number(v)) => v,
                other => panic!("{curve}.{bound}: {other:?}"),
            })
        };
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Time);
        let time = wired(&graph, &wired(&graph, "color1", "brightness"), "x");
        // A phase is degrees with no clamp: it wraps, so 450 and -90 are
        // phases too.
        let phase = spec(&graph, &time, "phase").unwrap();
        assert_eq!(phase.unit, Some(Unit::Degrees));
        assert!(phase.range[0] <= -90. && phase.range[1] >= 450.);
        promote(&mut graph, &time, "phase", Kind::Space);
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        let sweep = wired(&graph, &time, "phase");
        assert_eq!(bounds(&graph, &sweep), [0., 180.]);
        assert_eq!(
            graph.nodes[&sweep].inputs.get("shape"),
            Some(&Input::Points(preset_curve("Ramp up")))
        );
        let mut aim = ClipGraph::new([("aim1".to_owned(), Node::new(Kind::Aim))]);
        promote(&mut aim, "aim1", "yaw", Kind::Time);
        let [low, high] = bounds(&aim, &wired(&aim, "aim1", "yaw"));
        assert!(low < 0. && high > 0. && low == -high, "{low}..{high}");
    }

    #[test]
    fn over_time_adds_a_time_and_a_curve_and_unwire_takes_both_back() {
        let mut graph = wash();
        let held = promote(&mut graph, "color1", "brightness", Kind::Time);
        assert_eq!(graph.nodes.len(), 3);
        let curve = wired(&graph, "color1", "brightness");
        assert_eq!(graph.nodes[&wired(&graph, &curve, "x")].kind, Kind::Time);
        // The ramp ends at the value it replaced, so the clip still lights.
        assert_eq!(
            graph.nodes[&curve].inputs.get("high"),
            Some(&Input::Number(1.))
        );
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        set_input(&mut graph, "color1", "brightness", held);
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn every_promotion_of_every_value_input_checks() {
        for output in [Kind::Color, Kind::Aim, Kind::Strobe] {
            for (input, def) in &definition(output).inputs {
                if def.ty.is_wire_only() || def.ty.is_value_only() {
                    continue;
                }
                for source in SOURCES {
                    let id = format!("{}1", output.name());
                    let mut graph = ClipGraph::new([(id.clone(), Node::new(output))]);
                    promote(&mut graph, &id, input, source);
                    assert!(
                        graph.check().is_ok(),
                        "{output:?}.{input} over {source:?}: {:?}",
                        graph.check()
                    );
                }
            }
        }
    }

    #[test]
    fn a_linked_node_survives_one_unwire() {
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Time);
        let curve = wired(&graph, "color1", "brightness");
        assert!(link_candidates(&graph, "color1", "alpha").contains(&curve));
        link(&mut graph, "color1", "alpha", &curve);
        set_input(&mut graph, "color1", "brightness", None);
        assert!(graph.nodes.contains_key(&curve));
        assert_eq!(
            references(&graph, &curve),
            [("color1".into(), "alpha".into())]
        );
    }

    #[test]
    fn a_link_never_makes_a_cycle() {
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Noise);
        let above = wired(&graph, "color1", "brightness");
        let noise = wired(&graph, &above, "x");
        assert!(!link_candidates(&graph, &noise, "contrast").contains(&above));
    }

    fn chase() -> ClipGraph {
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Space);
        graph
    }

    #[test]
    fn columns_run_from_the_sources_to_the_output() {
        let mut graph = chase();
        multiply(&mut graph, "color1", "brightness");
        let product = wired(&graph, "color1", "brightness");
        promote(&mut graph, &product, "values", Kind::Time);
        let columns = columns(&graph);
        assert_eq!(columns[0], ["color1"]);
        let column = |id: &str| columns.iter().position(|c| c.iter().any(|n| n == id));
        // Every node is a card, and every wire runs from a column further
        // left into one further right.
        for (id, node) in &graph.nodes {
            for (_, from) in node.wires() {
                assert!(column(from) > column(id), "{from} feeds {id}");
            }
        }
        assert_eq!(
            columns.iter().map(Vec::len).sum::<usize>(),
            graph.nodes.len()
        );
        // A product's items stand in its order.
        let Some(Input::List(items)) = graph.nodes[&product].inputs.get("values") else {
            panic!("no items");
        };
        let wires: Vec<&str> = items.iter().filter_map(Input::source).collect();
        let at = &columns[column(wires[0]).unwrap()];
        let place = |id: &str| at.iter().position(|n| n == id);
        assert!(
            wires.windows(2).all(|pair| place(pair[0]) < place(pair[1])),
            "{columns:?}"
        );
        assert_eq!(columns, super::columns(&graph.clone()));
    }

    #[test]
    fn deleting_a_coordinate_takes_its_curve_and_gives_the_value_back() {
        let mut graph = chase();
        let curve = wired(&graph, "color1", "brightness");
        let space = wired(&graph, &curve, "x");
        delete(&mut graph, &space, |_, _, _| Some(Input::Number(0.5)));
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(
            graph.nodes["color1"].inputs.get("brightness"),
            Some(&Input::Number(0.5))
        );
        delete(&mut graph, "color1", |_, _, _| None);
        assert!(graph.nodes.contains_key("color1"), "the output stays");
    }

    #[test]
    fn attach_puts_a_curve_between_a_new_source_and_a_value() {
        let mut graph = wash();
        assert!(attach(&mut graph, "color1", "brightness", Kind::Shuffle).is_none());
        assert!(attach(&mut graph, "color1", "brightness", Kind::Noise).is_some());
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        let curve = wired(&graph, "color1", "brightness");
        let time = {
            let mut graph = wash();
            attach(&mut graph, "color1", "alpha", Kind::Curve);
            let curve = wired(&graph, "color1", "alpha");
            graph.nodes[&wired(&graph, &curve, "x")].kind
        };
        assert_eq!(time, Kind::Time);
        assert_eq!(graph.nodes[&wired(&graph, &curve, "x")].kind, Kind::Noise);
    }

    #[test]
    fn multiply_builds_one_product_that_grows_and_gives_way_to_its_last_item() {
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Time);
        let curve = wired(&graph, "color1", "brightness");
        multiply(&mut graph, "color1", "brightness");
        let product = wired(&graph, "color1", "brightness");
        assert_eq!(graph.nodes[&product].kind, Kind::Math);
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        // Multiplying again grows the same product.
        multiply(&mut graph, "color1", "brightness");
        assert_eq!(wired(&graph, "color1", "brightness"), product);
        let items = |graph: &ClipGraph| match graph.nodes[&product].inputs.get("values") {
            Some(Input::List(items)) => items.clone(),
            other => panic!("values: {other:?}"),
        };
        assert_eq!(items(&graph).len(), 3);
        // A second curve, shared from alpha, joins the values as an item.
        promote(&mut graph, "color1", "alpha", Kind::Space);
        let other = wired(&graph, "color1", "alpha");
        assert!(link_candidates(&graph, &product, "values").contains(&other));
        link(&mut graph, &product, "values", &other);
        assert_eq!(items(&graph).len(), 4);
        assert!(!link_candidates(&graph, &product, "values").contains(&curve));
        // Deleting a wired item's node takes only that item.
        delete(&mut graph, &other, |_, _, _| None);
        assert_eq!(items(&graph).len(), 3);
        // Down to one item, the product gives way to it.
        remove_item(&mut graph, &product, "values", 2);
        remove_item(&mut graph, &product, "values", 1);
        assert!(!graph.nodes.contains_key(&product));
        assert_eq!(wired(&graph, "color1", "brightness"), curve);
        assert!(graph.check().is_ok(), "{:?}", graph.check());
    }

    #[test]
    fn a_plain_value_multiplies_with_a_one_and_a_lone_number_goes_back_to_the_input() {
        let mut graph = wash();
        set_input(&mut graph, "color1", "brightness", Some(Input::Number(0.5)));
        multiply(&mut graph, "color1", "brightness");
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        let product = wired(&graph, "color1", "brightness");
        remove_item(&mut graph, &product, "values", 1);
        assert_eq!(
            graph.nodes["color1"].inputs.get("brightness"),
            Some(&Input::Number(0.5))
        );
        // A color holds no number, so a plain color has nothing to multiply.
        assert!(!can_multiply(&graph, "color1", "color"));
    }

    #[test]
    fn a_mirror_plane_sits_at_the_centre_when_empty() {
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Space);
        let space = wired(&graph, &wired(&graph, "color1", "brightness"), "x");
        insert(&mut graph, &space, "heads", Kind::Mirror);
        let mirror = wired(&graph, &space, "heads");
        assert_eq!(shown(&graph, &mirror, "at"), Some(Input::Number(0.5)));
        assert_eq!(
            spec(&graph, &mirror, "at").and_then(|s| s.unit),
            Some(Unit::Share)
        );
    }

    #[test]
    fn a_shuffle_takes_a_new_time_with_events_every_beat() {
        let mut graph = wash();
        promote(&mut graph, "color1", "brightness", Kind::Space);
        let space = wired(&graph, &wired(&graph, "color1", "brightness"), "x");
        insert(&mut graph, &space, "heads", Kind::Shuffle);
        let shuffle = wired(&graph, &space, "heads");
        assert!(accepts(&graph, &shuffle, "time", Kind::Time));
        assert!(!accepts(&graph, &shuffle, "time", Kind::Noise));
        insert(&mut graph, &shuffle, "time", Kind::Time);
        let time = wired(&graph, &shuffle, "time");
        assert_eq!(
            graph.nodes[&time].inputs.get("every"),
            Some(&Input::Number(1.))
        );
        assert!(graph.check().is_ok(), "{:?}", graph.check());
    }

    #[test]
    fn a_new_value_holds_what_the_input_held_and_gives_it_back_on_delete() {
        let mut graph = wash();
        set_input(&mut graph, "color1", "brightness", Some(Input::Number(0.4)));
        attach(&mut graph, "color1", "brightness", Kind::Value);
        let value = wired(&graph, "color1", "brightness");
        assert_eq!(graph.nodes[&value].kind, Kind::Value);
        assert_eq!(shown(&graph, &value, "value"), Some(Input::Number(0.4)));
        assert_eq!(
            spec(&graph, &value, "value").map(|s| (s.ty, s.unit)),
            Some((Ty::Number, Some(Unit::Share)))
        );
        assert!(graph.check().is_ok(), "{:?}", graph.check());
        // A share may share it; a beat count may not.
        assert!(link_candidates(&graph, "color1", "alpha").contains(&value));
        promote(&mut graph, "color1", "alpha", Kind::Time);
        let time = wired(&graph, &wired(&graph, "color1", "alpha"), "x");
        assert!(!link_candidates(&graph, &time, "delay").contains(&value));
        link(&mut graph, "color1", "alpha", &value);
        delete(&mut graph, &value, |_, _, _| None);
        for input in ["brightness", "alpha"] {
            assert_eq!(
                graph.nodes["color1"].inputs.get(input),
                Some(&Input::Number(0.4))
            );
        }
        // Into a color, three numbers show as a color.
        attach(&mut graph, "color1", "color", Kind::Value);
        let value = wired(&graph, "color1", "color");
        assert_eq!(spec(&graph, &value, "value").map(|s| s.ty), Some(Ty::Color));
        assert_eq!(
            shown(&graph, &value, "value"),
            Some(Input::Color([1., 1., 1.]))
        );
        assert!(graph.check().is_ok(), "{:?}", graph.check());
    }

    #[test]
    fn a_turned_vector_keeps_its_length_and_rises() {
        let v = turned_up([0., 1., 0.], 30.);
        assert!((v[0].powi(2) + v[1].powi(2) + v[2].powi(2) - 1.).abs() < 1e-9);
        assert!(v[2] > 0.49 && v[2] < 0.51);
    }
}
