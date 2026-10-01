//! The one type checker (spec section 3). Score writes, the UI and Python
//! all run it. Each error reads
//! `<node id>.<input>: expected <shape and unit>; got <what was given>. Example: <one Python call>`.
use super::{definition, ClipGraph, InputDef, InputType, Kind, Node, Produces, Unit, VERSION};
use super::{Input, MAX_NODES};
use crate::{BlendMode, Clip, Error, Result};
use std::collections::{BTreeSet, HashMap};

/// Rules 1–5 of spec 3.1, in order: graph, node, value, wire, axes.
pub fn check(graph: &ClipGraph) -> Result<()> {
    structure(graph)?;
    let ids = graph.ids_in_order();
    for id in &ids {
        node_shape(id, &graph.nodes[*id])?;
    }
    for id in &ids {
        values(graph, id, &graph.nodes[*id])?;
    }
    for id in &ids {
        wires(graph, id, &graph.nodes[*id])?;
    }
    for id in &ids {
        if graph.nodes[*id].kind == Kind::Curve {
            curve_destinations(graph, id)?;
        }
    }
    let mut memo = HashMap::new();
    for id in &ids {
        axes(graph, id, &mut memo)?;
    }
    Ok(())
}

/// [`check`] and then rule 6: the clip's name, blend mode and time range.
pub fn check_clip(clip: &Clip) -> Result<()> {
    check(&clip.graph)?;
    let name = clip.name.trim();
    if name.is_empty() {
        return fail(r#"clip: expected a name; got none. Example: name="Kick chase""#);
    }
    let length = clip.name.chars().count();
    if length > 64 {
        return fail(format!(
            r#"clip: expected a name of at most 64 characters; got {length}. Example: name="Kick chase""#
        ));
    }
    if let Some(kind) = clip.graph.output_kind() {
        let modes = blend_modes(kind);
        if !modes.contains(&clip.blend_mode) {
            let names: Vec<&str> = modes.iter().map(|mode| mode.name()).collect();
            return fail(format!(
                r#"clip: expected a blend mode for {}: {}; got {}. Example: blend="replace""#,
                kind.name(),
                either(&names),
                clip.blend_mode.name()
            ));
        }
    }
    let (start, duration) = (clip.start, clip.duration);
    if !start.is_finite()
        || !duration.is_finite()
        || duration <= 0.
        || !(start + duration).is_finite()
    {
        return fail(format!(
            "clip: expected a finite start and a duration above 0; got start {start} and duration {duration}. Example: beats=(32, 40)"
        ));
    }
    Ok(())
}

/// The blend modes an output kind takes: aim blends by `replace` or
/// `offset`; color and strobe by the light set.
pub fn blend_modes(kind: Kind) -> &'static [BlendMode] {
    if kind == Kind::Aim {
        &[BlendMode::Replace, BlendMode::Offset]
    } else {
        &BlendMode::LIGHT
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(Error(message.into()))
}

// ---- rule 1: graph ----

fn structure(graph: &ClipGraph) -> Result<()> {
    if graph.version != VERSION {
        return fail(format!(
            r#"graph: expected version {VERSION}; got {}. Example: "version": {VERSION}"#,
            graph.version
        ));
    }
    let count = graph.nodes.len();
    if count == 0 || count > MAX_NODES {
        return fail(format!(
            "graph: expected 1–{MAX_NODES} nodes; got {count}. Example: color()"
        ));
    }
    let ids = graph.ids_in_order();
    if let Some(id) = ids.iter().find(|id| !is_name(id)) {
        return fail(format!(
            r#"graph: expected node ids that are Python names of at most {MAX_ID} letters, digits and _, such as cut or curve2; got "{id}". Example: cut"#
        ));
    }
    if let Some(id) = ids.iter().find(|id| RESERVED.contains(id)) {
        return fail(format!(
            r#"graph: expected node ids that are not a builder name or a Python keyword; got "{id}". Example: {id}_1"#
        ));
    }
    let outputs: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|id| graph.nodes[*id].kind.is_output())
        .collect();
    match outputs.len() {
        1 => {}
        0 => return fail("graph: expected one output node; got none. Example: color()"),
        _ => {
            return fail(format!(
                "graph: expected one output node; got {}. Example: one clip per output",
                list(&outputs)
            ))
        }
    }
    for id in &ids {
        let node = &graph.nodes[*id];
        for (input, source) in node.wires() {
            if !graph.nodes.contains_key(source) {
                return fail(format!(
                    "{id}.{input}: expected a wire to a node in this graph; got a wire to {source}. Example: {}",
                    example(node.kind, input)
                ));
            }
        }
    }
    if let Some(cycle) = find_cycle(graph, &ids) {
        return fail(format!(
            "graph: expected no loop; got {}. Example: wire each node only into nodes after it",
            cycle.join(" → ")
        ));
    }
    let mut feeds = BTreeSet::from([outputs[0]]);
    let mut pending = vec![outputs[0]];
    while let Some(id) = pending.pop() {
        for (_, source) in graph.nodes[id].wires() {
            if feeds.insert(source) {
                pending.push(source);
            }
        }
    }
    if let Some(id) = ids.iter().find(|id| !feeds.contains(**id)) {
        return fail(format!(
            "{id}: expected a wire into the output through other nodes; nothing reads it. Example: wire it into an input, or delete it"
        ));
    }
    Ok(())
}

/// The longest node id.
const MAX_ID: usize = 32;

/// Names a node id may not take: the Python builders, whose bare names a
/// cell binds, and Python's keywords. `source()` assigns each node to a
/// variable of its id.
const RESERVED: &[&str] = &[
    "clock", "time", "space", "noise", "audio", "curve", "mirror", "shuffle", "group", "split",
    "color", "aim", "strobe", "preset", "False", "None", "True", "and", "as", "assert", "async",
    "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally",
    "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass",
    "raise", "return", "try", "while", "with", "yield",
];

/// An ASCII Python identifier of at most [`MAX_ID`] characters: `cut`,
/// `curve2`, `bloom_far`.
fn is_name(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= MAX_ID
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A loop of wires, as the node ids that read each other, first id last
/// again.
fn find_cycle(graph: &ClipGraph, ids: &[&str]) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    fn visit<'a>(
        graph: &'a ClipGraph,
        id: &'a str,
        marks: &mut HashMap<&'a str, Mark>,
        path: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match marks.get(id) {
            Some(Mark::Done) => return None,
            Some(Mark::Open) => {
                let start = path.iter().position(|seen| *seen == id)?;
                let mut cycle: Vec<String> = path[start..].iter().map(|s| s.to_string()).collect();
                cycle.push(id.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(id, Mark::Open);
        path.push(id);
        for (_, source) in graph.nodes[id].wires() {
            if let Some(cycle) = visit(graph, source, marks, path) {
                return Some(cycle);
            }
        }
        path.pop();
        marks.insert(id, Mark::Done);
        None
    }
    let mut marks = HashMap::new();
    ids.iter()
        .find_map(|id| visit(graph, id, &mut marks, &mut Vec::new()))
}

// ---- rule 2: node ----

fn node_shape(id: &str, node: &Node) -> Result<()> {
    let definition = definition(node.kind);
    let kind = node.kind.name();
    for name in node.inputs.keys() {
        if definition.input(name).is_none() {
            let names: Vec<&str> = definition.inputs.iter().map(|(name, _)| *name).collect();
            let (first, _) = definition.inputs[0];
            return fail(format!(
                "{id}.{name}: expected an input of {kind}: {}; got {name}. Example: {}",
                list(&names),
                example(node.kind, first)
            ));
        }
    }
    for (name, value) in &node.settings {
        let Some(setting) = definition.setting(name) else {
            if definition.settings.is_empty() {
                return fail(format!(
                    "{id}.{name}: expected no settings on {kind}; got {name}. Example: {kind}()"
                ));
            }
            let names: Vec<&str> = definition.settings.iter().map(|(name, _)| *name).collect();
            return fail(format!(
                "{id}.{name}: expected a setting of {kind}: {}; got {name}. Example: {}=\"{}\"",
                list(&names),
                definition.settings[0].0,
                definition.settings[0].1.default
            ));
        };
        if !setting.options.contains(&value.as_str()) {
            return fail(format!(
                "{id}.{name}: expected {}; got \"{value}\". Example: {name}=\"{}\"",
                either(setting.options),
                setting.default
            ));
        }
    }
    Ok(())
}

// ---- rule 3: values ----

fn values(graph: &ClipGraph, id: &str, node: &Node) -> Result<()> {
    let definition = definition(node.kind);
    for (name, def) in &definition.inputs {
        let input = node.inputs.get(*name);
        let required = matches!(
            (node.kind, *name),
            (Kind::Clock, "every") | (Kind::Curve, "x")
        );
        let Some(input) = input else {
            if required {
                let expected = if def.ty == InputType::Number {
                    range_phrase(def)
                } else {
                    accepts(def)
                };
                return fail(format!(
                    "{id}.{name}: expected {expected}; got nothing. Example: {}",
                    example(node.kind, name)
                ));
            }
            continue;
        };
        let wrong_type = || {
            fail(format!(
                "{id}.{name}: expected {}; got {}. Example: {}",
                accepts(def),
                describe(graph, Some(input)),
                example(node.kind, name)
            ))
        };
        match (def.ty, input) {
            (_, Input::Wire(_)) => {}
            (_, Input::List(items)) => list_items(id, name, node.kind, def, items)?,
            (ty, _) if ty.is_wire_only() => return wrong_type(),
            (InputType::Number, Input::Number(value)) => {
                if !value.is_finite() || !def.in_range(*value) {
                    return fail(format!(
                        "{id}.{name}: expected {}; got {value}. Example: {}",
                        range_phrase(def),
                        example(node.kind, name)
                    ));
                }
            }
            (InputType::Vector, Input::Vector(value)) => {
                if value.iter().any(|v| !v.is_finite()) {
                    return fail(format!(
                        "{id}.{name}: expected a vector of finite numbers; got {}. Example: {}",
                        vector(*value),
                        example(node.kind, name)
                    ));
                }
                if def.nonzero && is_zero(*value) {
                    return fail(format!(
                        "{id}.{name}: expected a direction that is not zero; got {}. Example: {}",
                        vector(*value),
                        example(node.kind, name)
                    ));
                }
            }
            (InputType::Color, Input::Color(value)) => {
                if value
                    .iter()
                    .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
                {
                    return fail(format!(
                        "{id}.{name}: expected a color with each channel 0–1; got {}. Example: {}",
                        vector(*value),
                        example(node.kind, name)
                    ));
                }
            }
            (InputType::Points, Input::Points(points)) => {
                points
                    .check(|v| {
                        (!v.is_finite() || !(0. ..=1.).contains(v))
                            .then(|| format!("v {v} must be in 0–1"))
                    })
                    .or_else(|error| {
                        fail(format!(
                            "{id}.{name}: expected points with v 0–1; got {error}. Example: {}",
                            example(node.kind, name)
                        ))
                    })?;
            }
            (InputType::Gradient, Input::Gradient(gradient)) => {
                let checked = if gradient.stops.is_empty() {
                    Err(Error("no stops".into()))
                } else {
                    gradient.validate()
                };
                checked.or_else(|error| {
                    fail(format!(
                        "{id}.{name}: expected a gradient with 1–64 stops in order; got {error}. Example: {}",
                        example(node.kind, name)
                    ))
                })?;
            }
            (InputType::Bound, Input::Number(value)) if !value.is_finite() => {
                return fail(format!(
                    "{id}.{name}: expected a finite number; got {value}. Example: {}",
                    example(node.kind, name)
                ))
            }
            (InputType::Bound, Input::Vector(value)) if value.iter().any(|v| !v.is_finite()) => {
                return fail(format!(
                    "{id}.{name}: expected a vector of finite numbers; got {}. Example: {}",
                    vector(*value),
                    example(node.kind, name)
                ))
            }
            (InputType::Bound, Input::Number(_) | Input::Vector(_)) => {}
            _ => return wrong_type(),
        }
    }
    if node.kind == Kind::Audio {
        let hz = |name: &str, empty: f64| match node.inputs.get(name) {
            None => Some(empty),
            Some(Input::Number(hz)) => Some(*hz),
            _ => None,
        };
        if let (Some(low), Some(high)) = (hz("low_hz", 40.), hz("high_hz", 100.)) {
            if high <= low {
                return fail(format!(
                    "{id}.high_hz: expected Hz above low_hz ({low}); got {high}. Example: high_hz={}",
                    (low * 2.).min(20000.)
                ));
            }
        }
    }
    Ok(())
}

/// A list multiplies its items: two or more numbers in the input's range
/// or number curves, on an input that takes a list.
fn list_items(id: &str, name: &str, kind: Kind, def: &InputDef, items: &[Input]) -> Result<()> {
    if !def.takes_list() {
        let note = if def.ty == InputType::Number {
            ". A list multiplies only on an input with range 0–1, such as brightness"
        } else {
            ""
        };
        return fail(format!(
            "{id}.{name}: expected {}; got a list of {} items. Example: {}{note}",
            accepts(def),
            items.len(),
            example(kind, name)
        ));
    }
    if items.len() < 2 {
        return fail(format!(
            "{id}.{name}: expected a list of two or more items; got {}. Example: {name}=[curve1, curve2]",
            items.len()
        ));
    }
    for item in items {
        match item {
            Input::Wire(_) => {}
            Input::Number(value) if value.is_finite() && def.in_range(*value) => {}
            Input::Number(value) => {
                return fail(format!(
                    "{id}.{name}: expected list items {}; got {value}. Example: {name}=[curve1, 0.5]",
                    range_phrase(def)
                ))
            }
            _ => {
                return fail(format!(
                    "{id}.{name}: expected list items that are numbers or number curves; got a list holding another shape. Example: {name}=[curve1, 0.5]"
                ))
            }
        }
    }
    Ok(())
}

// ---- rule 4: wires ----

fn wires(graph: &ClipGraph, id: &str, node: &Node) -> Result<()> {
    if node.kind == Kind::Curve {
        curve_kind_inputs(graph, id, node)?;
    }
    let definition = definition(node.kind);
    for (name, def) in &definition.inputs {
        let Some(given) = node.inputs.get(*name) else {
            continue;
        };
        for source in given.sources() {
            let input = &Input::Wire(source.to_string());
            let source_node = &graph.nodes[source];
            let produces = definition_of(source_node).output;
            let fits = match def.ty {
                InputType::Number => curve_kind(source_node) == Some("number"),
                InputType::Vector => curve_kind(source_node) == Some("vector"),
                InputType::Color => curve_kind(source_node) == Some("color"),
                InputType::Bound => {
                    curve_kind(source_node).is_some() && curve_kind(source_node) == curve_kind(node)
                }
                InputType::Heads => produces == Produces::Heads,
                InputType::Clock => produces == Produces::Clock,
                InputType::Coordinate => produces == Produces::Coordinate,
                InputType::Points | InputType::Gradient => false,
            };
            if fits {
                continue;
            }
            let expected = if def.ty == InputType::Bound {
                let kind = curve_kind(node).unwrap_or("number");
                format!("a {kind} or a {kind} curve")
            } else {
                accepts(def)
            };
            let example = match (def.ty, produces) {
                (InputType::Number | InputType::Vector, Produces::Coordinate) => {
                    format!("{name}=curve({source}, \"Ramp up\")")
                }
                (InputType::Color, Produces::Coordinate) => {
                    format!("{name}=curve({source}, \"Ramp up\", gradient=\"Rainbow\")")
                }
                _ => example(node.kind, name),
            };
            return fail(format!(
                "{id}.{name}: expected {expected}; got {}. Example: {example}",
                describe(graph, Some(input))
            ));
        }
    }
    Ok(())
}

/// A color curve takes a gradient and no low or high; a number or vector
/// curve takes low and high and no gradient.
fn curve_kind_inputs(graph: &ClipGraph, id: &str, node: &Node) -> Result<()> {
    let kind = node.setting("kind").unwrap_or("number");
    if kind == "color" {
        if !node.inputs.contains_key("gradient") {
            return fail(format!(
                r#"{id}.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow""#
            ));
        }
        for bound in ["low", "high"] {
            if let Some(input) = node.inputs.get(bound) {
                return fail(format!(
                    r#"{id}.{bound}: expected no {bound} because kind is color; got {}. Example: gradient="Rainbow""#,
                    describe(graph, Some(input))
                ));
            }
        }
    } else if node.inputs.contains_key("gradient") {
        return fail(format!(
            r#"{id}.gradient: expected no gradient because kind is {kind}; got a gradient. Example: kind="color""#
        ));
    }
    Ok(())
}

/// A final destination of a curve's value: an input that is not another
/// curve's low or high.
struct Destination<'a> {
    node: &'a str,
    input: &'a str,
    def: &'a InputDef,
}
impl Destination<'_> {
    fn name(&self) -> String {
        format!("{}.{}", self.node, self.input)
    }
}

fn destinations<'a>(graph: &'a ClipGraph, curve: &str, out: &mut Vec<Destination<'a>>) {
    for id in graph.ids_in_order() {
        let node = &graph.nodes[id];
        for (input, source) in node.wires() {
            if source != curve {
                continue;
            }
            if node.kind == Kind::Curve && matches!(input, "low" | "high") {
                destinations(graph, id, out);
            } else if let Some(def) = definition(node.kind).input(input) {
                if !out.iter().any(|d| d.node == id && d.input == input) {
                    out.push(Destination {
                        node: id,
                        input,
                        def,
                    });
                }
            }
        }
    }
}

/// Rule 4 for one curve against the inputs it feeds: one unit, low and
/// high of the destination's type, in its range, and a direction's low and
/// high neither zero nor opposite.
fn curve_destinations(graph: &ClipGraph, id: &str) -> Result<()> {
    let node = &graph.nodes[id];
    let mut dests = Vec::new();
    destinations(graph, id, &mut dests);
    let mut units: Vec<Option<Unit>> = Vec::new();
    for dest in &dests {
        if !units.contains(&dest.def.unit) {
            units.push(dest.def.unit);
        }
    }
    if units.len() > 1 {
        let named: Vec<String> = dests
            .iter()
            .map(|dest| {
                let unit = dest.def.unit.map_or("no unit", Unit::name);
                format!("{} ({unit})", dest.name())
            })
            .collect();
        return fail(format!(
            "{id}: expected one unit; it feeds {}. Example: make two curves",
            list(&named)
        ));
    }
    let kind = node.setting("kind").unwrap_or("number");
    let Some(first) = dests.first() else {
        return Ok(());
    };
    match kind {
        "number" => {
            for (bound, empty) in [("low", 0.), ("high", 1.)] {
                let (value, given) = match node.inputs.get(bound) {
                    None => (empty, format!("nothing, which reads as {empty}")),
                    Some(Input::Number(value)) => (*value, format!("{value}")),
                    Some(Input::Vector(value)) => {
                        return fail(format!(
                            "{id}.{bound}: expected a number for {}; got {}. Example: {bound}={}",
                            first.name(),
                            vector(*value),
                            bound_example(first.def, bound)
                        ))
                    }
                    Some(_) => continue,
                };
                if let Some(dest) = dests.iter().find(|dest| !dest.def.in_range(value)) {
                    return fail(format!(
                        "{id}.{bound}: expected {} for {}; got {given}. Example: {bound}={}",
                        range_phrase(dest.def),
                        dest.name(),
                        bound_example(dest.def, bound)
                    ));
                }
            }
        }
        "vector" => {
            let direction = dests.iter().find(|dest| dest.def.nonzero);
            let example = if direction.is_some() {
                "low=(0, 1, -0.5), high=(0, 0.5, -1)"
            } else {
                "low=(-3, 3, 0), high=(3, 3, 0)"
            };
            let mut ends = Vec::new();
            for bound in ["low", "high"] {
                match node.inputs.get(bound) {
                    Some(Input::Vector(value)) => {
                        if let Some(dest) = direction.filter(|_| is_zero(*value)) {
                            return fail(format!(
                                "{id}.{bound}: expected a vector that is not zero for {}; got {}. Example: {example}",
                                dest.name(),
                                vector(*value)
                            ));
                        }
                        ends.push(*value);
                    }
                    Some(Input::Wire(_)) => {}
                    other => {
                        return fail(format!(
                            "{id}.{bound}: expected a vector for {}; got {}. Example: {example}",
                            first.name(),
                            describe(graph, other)
                        ))
                    }
                }
            }
            if let (Some(dest), [low, high]) = (direction, ends.as_slice()) {
                if opposite(*low, *high) {
                    return fail(format!(
                        r#"{id}: expected low and high not opposite for {}; got {} and {}. Example: rotate with kind="angle" instead"#,
                        dest.name(),
                        vector(*low),
                        vector(*high)
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// ---- rule 5: axes ----

/// The axes a node's output wire carries: whether it varies over heads,
/// and the clocks whose events it carries.
#[derive(Clone, Default)]
struct Axes {
    heads: bool,
    clocks: BTreeSet<String>,
}

fn axes(graph: &ClipGraph, id: &str, memo: &mut HashMap<String, Axes>) -> Result<Axes> {
    if let Some(known) = memo.get(id) {
        return Ok(known.clone());
    }
    let node = &graph.nodes[id];
    let definition = definition(node.kind);
    let mut out = Axes::default();
    // The first clock seen and where it came from: an input, or the node
    // itself when it is a clock.
    let mut first: Option<(String, Option<&str>)> = None;
    if node.kind == Kind::Clock {
        out.clocks.insert(id.to_string());
        first = Some((id.to_string(), None));
    }
    for (name, def) in &definition.inputs {
        let Some(given) = node.inputs.get(*name) else {
            continue;
        };
        // The items of one list multiply, so they carry one clock, even
        // on the output node.
        let mut list_clocks = BTreeSet::new();
        for source in given.sources() {
            let carried = axes(graph, source, memo)?;
            if matches!(given, Input::List(_)) {
                list_clocks.extend(carried.clocks.iter().cloned());
                if list_clocks.len() > 1 {
                    let clocks: Vec<&String> = list_clocks.iter().collect();
                    return fail(format!(
                    "{id}.{name}: expected list items of one clock; got {}. Example: use the same clock for every item",
                    list(&clocks)
                ));
                }
            }
            if def.time_only && carried.heads {
                return fail(format!(
                "{id}.{name}: expected one value for all heads (a value or a curve over time); got a wire that varies over heads from {source}. Example: {}",
                example(node.kind, name)
            ));
            }
            if !node.kind.is_output() {
                for clock in &carried.clocks {
                    match &first {
                        None => first = Some((clock.clone(), Some(name))),
                        Some((seen, _)) if seen == clock => {}
                        Some((seen, from)) => {
                            let through = if source == clock {
                                String::new()
                            } else {
                                format!(" through {source}")
                            };
                            let (whose, fix) = match from {
                                Some(input) => (
                                    format!("{input} follows {seen}"),
                                    "use the same clock for both",
                                ),
                                None => (
                                    format!("{seen} is a clock itself"),
                                    "leave clocks out of a clock's inputs",
                                ),
                            };
                            return fail(format!(
                            "{id}.{name}: expected wires of one clock; got {clock}{through} while {whose}. Example: {fix}"
                        ));
                        }
                    }
                }
            }
            out.heads |= carried.heads;
            out.clocks.extend(carried.clocks);
        }
    }
    match definition.output {
        Produces::Heads => out.heads = false,
        _ if matches!(node.kind, Kind::Space | Kind::Noise) => out.heads = true,
        _ => {}
    }
    memo.insert(id.to_string(), out.clone());
    Ok(out)
}

// ---- words ----

fn curve_kind(node: &Node) -> Option<&str> {
    (node.kind == Kind::Curve).then(|| node.setting("kind").unwrap_or("number"))
}

fn definition_of(node: &Node) -> &'static super::Definition {
    definition(node.kind)
}

/// `a`, `a and b`, `a, b and c`.
fn list(items: &[impl AsRef<str>]) -> String {
    joined(items, "and")
}

/// `a`, `a or b`, `a, b or c`.
fn either(items: &[impl AsRef<str>]) -> String {
    joined(items, "or")
}

fn joined(items: &[impl AsRef<str>], word: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_string(),
        [rest @ .., last] => {
            let rest: Vec<&str> = rest.iter().map(AsRef::as_ref).collect();
            format!("{} {word} {}", rest.join(", "), last.as_ref())
        }
    }
}

fn vector(v: [f64; 3]) -> String {
    format!("({},{},{})", v[0], v[1], v[2])
}

fn is_zero(v: [f64; 3]) -> bool {
    v.iter().all(|c| *c == 0.)
}

fn opposite(a: [f64; 3], b: [f64; 3]) -> bool {
    let length = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    dot / (length(a) * length(b)) <= -1. + 1e-9
}

/// What an input holds, in words.
fn describe(graph: &ClipGraph, input: Option<&Input>) -> String {
    match input {
        None => "nothing".into(),
        Some(Input::Number(value)) => format!("{value}"),
        Some(Input::Vector(value)) | Some(Input::Color(value)) => vector(*value),
        Some(Input::Points(_)) => "points".into(),
        Some(Input::Gradient(_)) => "a gradient".into(),
        Some(Input::List(items)) => format!("a list of {} items", items.len()),
        Some(Input::Wire(source)) => match graph.nodes.get(source) {
            None => format!("a wire to {source}, which is not in the graph"),
            Some(node) => match definition(node.kind).output {
                Produces::Clock => format!("a clock wire from {source}"),
                Produces::Heads => format!("a heads wire from {source}"),
                Produces::Coordinate => format!("a coordinate wire from {source}"),
                Produces::Value => format!(
                    "a {} wire from {source}",
                    node.setting("kind").unwrap_or("number")
                ),
                Produces::Output => format!("a wire from the output node {source}"),
            },
        },
    }
}

/// What an input accepts, for a type error.
fn accepts(def: &InputDef) -> String {
    let unit = def.unit.map_or("", Unit::name);
    match def.ty {
        InputType::Number => {
            let range = match def.range {
                Some([Some(min), Some(max)]) => format!(" {min}–{max}"),
                Some([Some(min), None]) if def.above_min => format!(" above {min}"),
                Some([Some(min), None]) => format!(" from {min}"),
                _ => String::new(),
            };
            format!("a number{range} ({unit}) or a number curve")
        }
        InputType::Vector if def.nonzero => {
            format!("a vector that is not zero ({unit}) or a vector curve")
        }
        InputType::Vector => format!("a vector ({unit}) or a vector curve"),
        InputType::Color => "a color (r, g, b) with each channel 0–1 or a color curve".into(),
        InputType::Points => "a shape: points or a curve preset name".into(),
        InputType::Gradient => "a gradient".into(),
        InputType::Heads => "a heads wire".into(),
        InputType::Clock => "a clock wire".into(),
        InputType::Coordinate => "a coordinate wire".into(),
        InputType::Bound => "a number or a vector".into(),
    }
}

/// A number input's range in words, such as `a share between 0 and 4`.
fn range_phrase(def: &InputDef) -> String {
    let noun = match def.unit {
        Some(Unit::Share) => "a share",
        Some(Unit::Beats) => "beats",
        Some(Unit::Degrees) => "degrees",
        Some(Unit::Metres) => "metres",
        Some(Unit::Hz) => "Hz",
        Some(Unit::Turns) => "turns",
        Some(Unit::Heads) => "heads",
        Some(Unit::Uvz) => "a vector",
        Some(Unit::Rgb) => "a color channel",
        None => "a number",
    };
    match def.range {
        Some([Some(min), Some(max)]) => format!("{noun} between {min} and {max}"),
        Some([Some(min), None]) if def.above_min => format!("{noun} above {min}"),
        Some([Some(min), None]) => format!("{noun} of at least {min}"),
        _ => format!("a finite number of {noun}"),
    }
}

/// `name=value` for an input.
fn example(kind: Kind, input: &str) -> String {
    let value = definition(kind).input(input).map_or("", |def| def.example);
    match (kind, input) {
        (Kind::Clock, "every") => {
            format!("every={value}, or leave the clock out for once over the clip")
        }
        _ => format!("{input}={value}"),
    }
}

/// An in-range example for a curve's low or high feeding `def`.
fn bound_example(def: &InputDef, bound: &str) -> &'static str {
    let low = bound == "low";
    match def.unit {
        Some(Unit::Degrees) => {
            if low {
                "-30"
            } else {
                "30"
            }
        }
        Some(Unit::Beats) => {
            if low {
                "0.5"
            } else {
                "1"
            }
        }
        Some(Unit::Hz) => {
            if low {
                "40"
            } else {
                "100"
            }
        }
        Some(Unit::Metres) => {
            if low {
                "-1"
            } else {
                "1"
            }
        }
        Some(Unit::Turns) => {
            if low {
                "0"
            } else {
                "0.5"
            }
        }
        Some(Unit::Heads) => {
            if low {
                "1"
            } else {
                "4"
            }
        }
        _ => {
            if low {
                "0"
            } else {
                "1"
            }
        }
    }
}
