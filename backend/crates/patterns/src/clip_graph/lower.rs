//! Lowering: a checked clip graph becomes a graph of kernel nodes over the
//! frame's cells (spec 5.1). Values become constants; the heads pipeline
//! becomes constant unit, span and position fields; every clock becomes
//! one event table; and the output node picks, per clock, the live event
//! with the biggest effect (spec 5.3).
use super::heads::{SplitBy, Units};
use super::kernels::{Kernel, AIM_MIRRORS};
use super::{ClipGraph, Input, Kind, Node as ClipNode};
use crate::{
    Binding, Body, Cell, Channels, Curve, Definition, Error, Frame, Gradient, Graph, Node, Output,
    Rate, Result, Signal, SignalType, Unit, Value, ValueType,
};
use ndarray::Array3;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The empty aim direction: the venue's default aim.
const DEFAULT_DIRECTION: [f64; 3] = [0., 0.766, -0.643];

/// A lowered value wire: one binding per component (a number uses the
/// first), whether a coordinate is inside its stroke, and the clock whose
/// events it carries.
#[derive(Clone)]
struct Lowered {
    parts: [Binding; 3],
    inside: Binding,
    clock: Option<String>,
}

/// A lowered heads wire.
#[derive(Clone)]
struct Heads {
    units: Units,
    /// Per cell, the unit's (folded) position: (heads, time, 3).
    positions: Binding,
    /// Per stacked mirror: whether each cell was folded, and the normal.
    mirrors: Vec<(Binding, Binding)>,
    /// A shuffle's event index and its clock.
    shuffle: Option<(Binding, Option<String>)>,
}

#[derive(Clone)]
struct Events {
    progress: Binding,
    present: Binding,
    index: Binding,
}

struct Lowering<'a> {
    graph: &'a ClipGraph,
    cells: Vec<Cell>,
    fixtures: Arc<[String]>,
    nodes: BTreeMap<String, Node>,
    clocks: HashMap<String, Events>,
    values: HashMap<String, Lowered>,
    heads: HashMap<String, Heads>,
}

fn number(value: f64) -> Binding {
    Value::Number(value).into()
}
fn output(node: &str, name: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: name.into(),
    }
}
fn constant(value: f64) -> Lowered {
    Lowered {
        parts: std::array::from_fn(|_| number(value)),
        inside: number(1.),
        clock: None,
    }
}
fn constant3(value: [f64; 3]) -> Lowered {
    Lowered {
        parts: value.map(number),
        inside: number(1.),
        clock: None,
    }
}
/// The one clock of several wires; the checker allows at most one outside
/// an output node.
fn clock_of<'a>(clocks: impl IntoIterator<Item = &'a Option<String>>) -> Option<String> {
    clocks.into_iter().flatten().next().cloned()
}

impl Lowering<'_> {
    fn node(&self, id: &str) -> &ClipNode {
        &self.graph.nodes[id]
    }

    fn kernel(&mut self, kernel: Kernel, inputs: Vec<(&str, Binding)>) -> String {
        let id = format!("k{}", self.nodes.len());
        self.nodes.insert(
            id.clone(),
            Node {
                definition: kernel.id().into(),
                inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            },
        );
        id
    }

    fn library_node(&mut self, definition: &str, inputs: Vec<(&str, Binding)>) -> String {
        let id = format!("k{}", self.nodes.len());
        self.nodes.insert(
            id.clone(),
            Node {
                definition: definition.into(),
                inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            },
        );
        id
    }

    /// A constant per-cell field of `width` channels.
    fn field(&self, values: Vec<f64>, width: usize) -> Result<Binding> {
        let heads = self.cells.len();
        let channels = Channels::components(width)?;
        Ok(Value::Signal(Signal::new(
            Array3::from_shape_vec((heads, 1, width), values).expect("field shape"),
            Unit::Number,
            channels,
            Some(self.fixtures.clone()),
        )?)
        .into())
    }
    fn index_field(&self, values: &[usize]) -> Result<Binding> {
        self.field(values.iter().map(|v| *v as f64).collect(), 1)
    }

    // ---- inputs ----

    /// A number input: its value, its curve, or `empty`.
    fn number(&mut self, id: &str, input: &str, empty: f64) -> Result<Lowered> {
        match self.node(id).inputs.get(input) {
            None => Ok(constant(empty)),
            Some(Input::Number(value)) => Ok(constant(*value)),
            Some(Input::Wire(source)) => {
                let source = source.clone();
                self.value(&source)
            }
            Some(_) => Err(Error(format!("{id}.{input}: expected a number"))),
        }
    }

    /// A vector or color input: its value, its curve, or `empty`.
    fn triple(&mut self, id: &str, input: &str, empty: [f64; 3]) -> Result<Lowered> {
        match self.node(id).inputs.get(input) {
            None => Ok(constant3(empty)),
            Some(Input::Vector(value) | Input::Color(value)) => Ok(constant3(*value)),
            Some(Input::Wire(source)) => {
                let source = source.clone();
                self.value(&source)
            }
            Some(_) => Err(Error(format!("{id}.{input}: expected a vector"))),
        }
    }

    /// A vector input that may be empty: `None` when it is.
    fn optional_triple(&mut self, id: &str, input: &str) -> Result<Option<Lowered>> {
        if self.node(id).inputs.contains_key(input) {
            self.triple(id, input, [0.; 3]).map(Some)
        } else {
            Ok(None)
        }
    }

    fn wire(&self, id: &str, input: &str) -> Option<String> {
        self.node(id)
            .inputs
            .get(input)
            .and_then(Input::source)
            .map(str::to_string)
    }

    /// A value wire from a coordinate node or a curve.
    fn value(&mut self, id: &str) -> Result<Lowered> {
        if let Some(known) = self.values.get(id) {
            return Ok(known.clone());
        }
        let lowered = match self.node(id).kind {
            Kind::Time => self.time(id)?,
            Kind::Space => self.space(id)?,
            Kind::Noise => self.noise(id)?,
            Kind::Audio => self.audio(id)?,
            Kind::Curve => self.curve(id)?,
            kind => {
                return Err(Error(format!(
                    "{id}: a {} node gives no value",
                    kind.name()
                )))
            }
        };
        self.values.insert(id.to_string(), lowered.clone());
        Ok(lowered)
    }

    // ---- clocks ----

    fn clock(&mut self, id: &str) -> Result<(Events, String)> {
        if let Some(known) = self.clocks.get(id) {
            return Ok((known.clone(), id.to_string()));
        }
        let every = self.number(id, "every", 1.)?;
        let duration = if self.node(id).inputs.contains_key("duration") {
            self.number(id, "duration", 1.)?
        } else {
            every.clone()
        };
        let every_table = self.kernel(Kernel::ClockTable, vec![("period", every.parts[0].clone())]);
        let duration_table = self.kernel(
            Kernel::ClockTable,
            vec![("period", duration.parts[0].clone())],
        );
        let events = self.kernel(
            Kernel::Events,
            vec![
                ("every", output(&every_table, "value")),
                ("duration", output(&duration_table, "value")),
            ],
        );
        let lowered = Events {
            progress: output(&events, "progress"),
            present: output(&events, "present"),
            index: output(&events, "index"),
        };
        self.clocks.insert(id.to_string(), lowered.clone());
        Ok((lowered, id.to_string()))
    }

    // ---- coordinates ----

    fn time(&mut self, id: &str) -> Result<Lowered> {
        let (progress, clock) = match self.wire(id, "clock") {
            Some(source) => {
                let (events, clock) = self.clock(&source)?;
                (events.progress, Some(clock))
            }
            None => (
                output(&self.kernel(Kernel::ClipProgress, vec![]), "value"),
                None,
            ),
        };
        let progress = if self.node(id).inputs.contains_key("phase") {
            let phase = self.number(id, "phase", 0.)?;
            let shifted = self.kernel(
                Kernel::Shift,
                vec![("progress", progress), ("phase", phase.parts[0].clone())],
            );
            return Ok(Lowered {
                parts: std::array::from_fn(|_| output(&shifted, "value")),
                inside: number(1.),
                clock: clock_of([&clock, &phase.clock]),
            });
        } else {
            progress
        };
        Ok(Lowered {
            parts: std::array::from_fn(|_| progress.clone()),
            inside: number(1.),
            clock,
        })
    }

    fn space(&mut self, id: &str) -> Result<Lowered> {
        let heads = self.heads_input(id)?;
        let node = self.node(id).clone();
        let kind = match node.setting("kind").unwrap_or("line") {
            "order" => 1.,
            "radial" => 2.,
            "angle" => 3.,
            _ => 0.,
        };
        let wrap = node.setting("wrap") == Some("yes");
        let direction = self.optional_triple(id, "direction")?;
        let offset = self.number(id, "offset", 0.)?;
        let width = self.number(id, "width", 1.)?;
        let rank = self.rank(&heads)?;
        let unit = self.index_field(&heads.units.unit)?;
        let span = self.index_field(&heads.units.span)?;
        let dir = direction.clone().unwrap_or_else(|| constant(0.));
        let axis = self.kernel(
            Kernel::Axis,
            vec![
                ("positions", heads.positions.clone()),
                ("unit", unit),
                ("span", span),
                ("rank", rank),
                ("dir_x", dir.parts[0].clone()),
                ("dir_y", dir.parts[1].clone()),
                ("dir_z", dir.parts[2].clone()),
                ("kind", number(kind)),
                (
                    "has_direction",
                    number(if direction.is_some() { 1. } else { 0. }),
                ),
            ],
        );
        let stroke = self.kernel(
            Kernel::Stroke,
            vec![
                ("a", output(&axis, "value")),
                ("offset", offset.parts[0].clone()),
                ("width", width.parts[0].clone()),
                ("wrap", number(if wrap { 1. } else { 0. })),
            ],
        );
        let shuffled = heads.shuffle.as_ref().and_then(|(_, clock)| clock.clone());
        Ok(Lowered {
            parts: std::array::from_fn(|_| output(&stroke, "x")),
            inside: output(&stroke, "inside"),
            clock: clock_of([&shuffled, &offset.clock, &width.clock]),
        })
    }

    /// Each unit's rank in its span: selection order, or the shuffle's.
    fn rank(&mut self, heads: &Heads) -> Result<Binding> {
        let (index, shuffle) = match &heads.shuffle {
            Some((index, _)) => (index.clone(), 1.),
            None => (number(0.), 0.),
        };
        let rank = self.kernel(
            Kernel::Rank,
            vec![
                ("index", index),
                ("unit", self.index_field(&heads.units.unit)?),
                ("span", self.index_field(&heads.units.span)?),
                ("first", self.index_field(&heads.units.first)?),
                ("order", self.field(heads.units.order.clone(), 1)?),
                ("shuffle", number(shuffle)),
            ],
        );
        Ok(output(&rank, "value"))
    }

    fn noise(&mut self, id: &str) -> Result<Lowered> {
        let heads = self.heads_input(id)?;
        let speed = self.number(id, "speed", 4.)?;
        let uniform = !self.node(id).inputs.contains_key("scale");
        let scale = self.number(id, "scale", 1.)?;
        let contrast = self.number(id, "contrast", 0.)?;
        let table = self.kernel(Kernel::ClockTable, vec![("period", speed.parts[0].clone())]);
        let turns = self.kernel(Kernel::Clock, vec![("table", output(&table, "value"))]);
        let salt = super::noise::fnv(id);
        let span = self.index_field(&heads.units.span)?;
        let noise = self.kernel(
            Kernel::Noise4,
            vec![
                ("positions", heads.positions.clone()),
                ("span", span),
                ("turns", output(&turns, "value")),
                ("scale", scale.parts[0].clone()),
                ("contrast", contrast.parts[0].clone()),
                ("uniform", number(if uniform { 1. } else { 0. })),
                ("salt_lo", number((salt & 0xffff_ffff) as f64)),
                ("salt_hi", number((salt >> 32) as f64)),
            ],
        );
        Ok(Lowered {
            parts: std::array::from_fn(|_| output(&noise, "value")),
            inside: number(1.),
            clock: clock_of([&scale.clock, &contrast.clock]),
        })
    }

    fn audio(&mut self, id: &str) -> Result<Lowered> {
        let hz = |this: &Self, input: &str, empty: f64| {
            match this.node(id).inputs.get(input) {
            None => Ok(empty),
            Some(Input::Number(hz)) => Ok(*hz),
            Some(_) => Err(Error(format!(
                "{id}.{input}: a band that moves over time does not play yet; use a value such as {input}={empty}"
            ))),
        }
        };
        let low = hz(self, "low_hz", 40.)?;
        let high = hz(self, "high_hz", 100.)?;
        let energy = self.library_node(
            "band_energy",
            vec![("low_hz", number(low)), ("high_hz", number(high))],
        );
        let range = self.library_node(
            "clip_range",
            vec![
                ("value", output(&energy, "value")),
                ("samples", number(1024.)),
            ],
        );
        let normalized = self.kernel(
            Kernel::Normalize,
            vec![
                ("value", output(&energy, "value")),
                ("minimum", output(&range, "minimum")),
                ("maximum", output(&range, "maximum")),
            ],
        );
        Ok(Lowered {
            parts: std::array::from_fn(|_| output(&normalized, "value")),
            inside: number(1.),
            clock: None,
        })
    }

    fn curve(&mut self, id: &str) -> Result<Lowered> {
        let node = self.node(id).clone();
        let source = self
            .wire(id, "x")
            .ok_or_else(|| Error(format!("{id}.x: expected a coordinate wire")))?;
        let x = self.value(&source)?;
        let kind = node.setting("kind").unwrap_or("number");
        let (low, high, code) = match kind {
            "vector" => (
                self.triple(id, "low", [0.; 3])?,
                self.triple(id, "high", [0.; 3])?,
                1.,
            ),
            "color" => (constant(0.), constant(0.), 2.),
            _ => (
                self.number(id, "low", 0.)?,
                self.number(id, "high", 1.)?,
                0.,
            ),
        };
        let shape = match node.inputs.get("shape") {
            Some(Input::Points(points)) => points.clone(),
            _ => Curve::linear(vec![[0., 0.], [1., 1.]]),
        };
        let gradient = match node.inputs.get("gradient") {
            Some(Input::Gradient(gradient)) => gradient.clone(),
            _ => Gradient::default(),
        };
        let curve = self.kernel(
            Kernel::Curve,
            vec![
                ("x", x.parts[0].clone()),
                ("inside", x.inside.clone()),
                ("low_x", low.parts[0].clone()),
                ("low_y", low.parts[1].clone()),
                ("low_z", low.parts[2].clone()),
                ("high_x", high.parts[0].clone()),
                ("high_y", high.parts[1].clone()),
                ("high_z", high.parts[2].clone()),
                ("shape", Value::Points(shape).into()),
                ("gradient", Value::Gradient(gradient).into()),
                ("kind", number(code)),
            ],
        );
        Ok(Lowered {
            parts: [
                output(&curve, "x"),
                output(&curve, "y"),
                output(&curve, "z"),
            ],
            inside: number(1.),
            clock: clock_of([&x.clock, &low.clock, &high.clock]),
        })
    }

    // ---- heads ----

    fn base_heads(&self) -> Result<Heads> {
        Ok(Heads {
            units: Units::base(&self.cells),
            positions: self.field(self.cells.iter().flat_map(|c| c.uvz).collect(), 3)?,
            mirrors: Vec::new(),
            shuffle: None,
        })
    }

    fn heads_input(&mut self, id: &str) -> Result<Heads> {
        match self.wire(id, "heads") {
            Some(source) => self.heads(&source),
            None => self.base_heads(),
        }
    }

    fn heads(&mut self, id: &str) -> Result<Heads> {
        if let Some(known) = self.heads.get(id) {
            return Ok(known.clone());
        }
        let mut heads = self.heads_input(id)?;
        let node = self.node(id).clone();
        match node.kind {
            Kind::Mirror => {
                let normal = self.optional_triple(id, "normal")?;
                let offset = self.number(id, "offset", 0.)?;
                let plane = normal.clone().unwrap_or_else(|| constant(0.));
                let fold = self.kernel(
                    Kernel::Fold,
                    vec![
                        ("positions", heads.positions.clone()),
                        ("span", self.index_field(&heads.units.span)?),
                        ("normal_x", plane.parts[0].clone()),
                        ("normal_y", plane.parts[1].clone()),
                        ("normal_z", plane.parts[2].clone()),
                        ("offset", offset.parts[0].clone()),
                        ("has_normal", number(if normal.is_some() { 1. } else { 0. })),
                    ],
                );
                heads.positions = output(&fold, "positions");
                heads
                    .mirrors
                    .push((output(&fold, "folded"), output(&fold, "normal")));
            }
            Kind::Group => {
                let size = match node.inputs.get("size") {
                    None => None,
                    Some(Input::Number(size)) => Some(size.round().max(1.) as usize),
                    Some(_) => {
                        return Err(Error(format!(
                            "{id}.size: a group size that moves over time does not play yet; use a value such as size=2"
                        )))
                    }
                };
                heads.units = heads.units.group(&self.cells, size);
                let unit = self.index_field(&heads.units.unit)?;
                let group = self.kernel(
                    Kernel::Group,
                    vec![("positions", heads.positions.clone()), ("unit", unit)],
                );
                heads.positions = output(&group, "positions");
            }
            Kind::Split => {
                let by = match node.setting("by") {
                    Some("group") => SplitBy::Group,
                    _ => SplitBy::Fixture,
                };
                heads.units = heads.units.split(&self.cells, by);
            }
            Kind::Shuffle => {
                heads.shuffle = Some(match self.wire(id, "clock") {
                    Some(source) => {
                        let (events, clock) = self.clock(&source)?;
                        (events.index, Some(clock))
                    }
                    None => (number(0.), None),
                });
            }
            kind => {
                return Err(Error(format!(
                    "{id}: a {} node gives no heads",
                    kind.name()
                )))
            }
        }
        self.heads.insert(id.to_string(), heads.clone());
        Ok(heads)
    }

    // ---- output ----

    /// Per clock, the winning event per head and time (spec 5.3): `factors`
    /// name each input's part in the effect.
    fn resolve(&mut self, inputs: &mut [(&str, Lowered)], role: Role) -> Result<()> {
        let mut clocks: Vec<String> = inputs.iter().filter_map(|(_, l)| l.clock.clone()).collect();
        clocks.sort();
        clocks.dedup();
        for clock in clocks {
            let events = self.clocks[&clock].clone();
            let carries = |name: &str, inputs: &[(&str, Lowered)]| {
                inputs
                    .iter()
                    .find(|(n, l)| *n == name && l.clock.as_deref() == Some(clock.as_str()))
                    .map(|(_, l)| l.clone())
            };
            let one = || number(1.);
            let peak = role
                .peak
                .and_then(|name| carries(name, inputs))
                .map_or_else(|| [one(), one(), one()], |l| l.parts);
            let scale: Vec<Binding> = role
                .scale
                .iter()
                .map(|name| carries(name, inputs).map_or_else(one, |l| l.parts[0].clone()))
                .collect();
            let turn: Vec<Option<Lowered>> =
                role.turn.iter().map(|name| carries(name, inputs)).collect();
            let turns = turn.iter().any(Option::is_some);
            let turn: Vec<Binding> = turn
                .into_iter()
                .map(|l| l.map_or_else(|| number(0.), |l| l.parts[0].clone()))
                .collect();
            let [px, py, pz] = peak;
            let pick = self.kernel(
                Kernel::Pick,
                vec![
                    ("present", events.present.clone()),
                    ("index", events.index.clone()),
                    ("peak_x", px),
                    ("peak_y", py),
                    ("peak_z", pz),
                    ("scale_a", scale.first().cloned().unwrap_or_else(one)),
                    ("scale_b", scale.get(1).cloned().unwrap_or_else(one)),
                    (
                        "turn_a",
                        turn.first().cloned().unwrap_or_else(|| number(0.)),
                    ),
                    ("turn_b", turn.get(1).cloned().unwrap_or_else(|| number(0.))),
                    ("turns", number(if turns { 1. } else { 0. })),
                ],
            );
            let winner = output(&pick, "winner");
            for (_, lowered) in inputs.iter_mut() {
                if lowered.clock.as_deref() != Some(clock.as_str()) {
                    continue;
                }
                let parts = lowered.parts.clone().map(|part| {
                    let gather = self.kernel(
                        Kernel::Gather,
                        vec![("value", part), ("winner", winner.clone())],
                    );
                    output(&gather, "value")
                });
                lowered.parts = parts;
                lowered.clock = None;
            }
        }
        Ok(())
    }
}

/// Which output inputs make an event's effect.
struct Role {
    /// Its peak channel counts.
    peak: Option<&'static str>,
    /// Multiplied.
    scale: &'static [&'static str],
    /// |a| + |b|.
    turn: &'static [&'static str],
}

impl<'a> Lowering<'a> {
    fn new(graph: &'a ClipGraph, frame: Frame<'_>) -> Self {
        let mut cells = frame.cells.to_vec();
        cells.sort_by(|a, b| a.id.cmp(&b.id));
        let fixtures: Arc<[String]> = cells
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>()
            .into();
        Lowering {
            graph,
            cells,
            fixtures,
            nodes: BTreeMap::new(),
            clocks: HashMap::new(),
            values: HashMap::new(),
            heads: HashMap::new(),
        }
    }
}

/// The output of [`lower_coordinate`] with the coordinate's value.
pub(crate) const COORDINATE: &str = "x";
/// The output of [`lower_coordinate`] that is 1 where a head is inside its
/// stroke. Only a coordinate that can be outside (`space`) has it.
pub(crate) const INSIDE: &str = "inside";

/// Coordinate node `id` of a checked graph alone, per head and live event:
/// its value as [`COORDINATE`], and [`INSIDE`] when it has a stroke. An
/// editor reads it to mark where the heads fall on the coordinate.
pub(crate) fn lower_coordinate(
    graph: &ClipGraph,
    id: &str,
    frame: Frame<'_>,
) -> Result<Definition> {
    if !graph.nodes.contains_key(id) {
        return Err(Error(format!("{id}: no such node")));
    }
    let mut lowering = Lowering::new(graph, frame);
    let value = lowering.value(id)?;
    let signal = Output {
        value_type: ValueType::Signal(SignalType::ANY),
        rate: Rate::Frame,
    };
    let mut outputs = BTreeMap::from([(COORDINATE.to_string(), value.parts[0].clone())]);
    if matches!(value.inside, Binding::Connection { .. }) {
        outputs.insert(INSIDE.to_string(), value.inside);
    }
    let types = outputs
        .keys()
        .map(|name| (name.clone(), signal.clone()))
        .collect();
    Ok(Definition {
        name: "Clip graph coordinate".into(),
        inputs: BTreeMap::new(),
        outputs: types,
        body: Body::Graph(Graph {
            nodes: lowering.nodes,
            outputs,
        }),
    })
}

pub(crate) fn lower(graph: &ClipGraph, frame: Frame<'_>) -> Result<Definition> {
    let mut lowering = Lowering::new(graph, frame);
    let (id, out) = graph
        .output()
        .ok_or_else(|| Error("graph: expected one output node; got none".into()))?;
    let id = id.to_string();
    let mut outputs = BTreeMap::new();
    let mut types = BTreeMap::from([(
        super::OUTPUT.to_string(),
        Output {
            value_type: ValueType::Lighting,
            rate: Rate::Frame,
        },
    )]);
    let terminal = match out.kind {
        Kind::Color => {
            let mut inputs = [
                ("color", lowering.triple(&id, "color", [1.; 3])?),
                ("brightness", lowering.number(&id, "brightness", 1.)?),
                ("alpha", lowering.number(&id, "alpha", 1.)?),
            ];
            lowering.resolve(
                &mut inputs,
                Role {
                    peak: Some("color"),
                    scale: &["brightness", "alpha"],
                    turn: &[],
                },
            )?;
            let [(_, color), (_, brightness), (_, alpha)] = inputs;
            let [r, g, b] = color.parts;
            let out = lowering.kernel(
                Kernel::ColorOut,
                vec![
                    ("r", r),
                    ("g", g),
                    ("b", b),
                    ("brightness", brightness.parts[0].clone()),
                    ("alpha", alpha.parts[0].clone()),
                ],
            );
            ("color", output(&out, "color"))
        }
        Kind::Strobe => {
            let mut inputs = [
                ("rate", lowering.number(&id, "rate", 0.5)?),
                ("alpha", lowering.number(&id, "alpha", 1.)?),
            ];
            lowering.resolve(
                &mut inputs,
                Role {
                    peak: None,
                    scale: &["rate", "alpha"],
                    turn: &[],
                },
            )?;
            let [(_, rate), (_, alpha)] = inputs;
            let out = lowering.kernel(
                Kernel::StrobeOut,
                vec![
                    ("rate", rate.parts[0].clone()),
                    ("alpha", alpha.parts[0].clone()),
                ],
            );
            ("strobe", output(&out, "strobe"))
        }
        Kind::Aim => {
            let heads = lowering.heads_input(&id)?;
            if heads.mirrors.len() > AIM_MIRRORS {
                return Err(Error(format!(
                    "{id}.heads: an aim reads at most {AIM_MIRRORS} stacked mirrors; got {}",
                    heads.mirrors.len()
                )));
            }
            let base = match out.setting("base") {
                Some("point") => 1.,
                Some("away") => 2.,
                _ => 0.,
            };
            let mut inputs = [
                (
                    "direction",
                    lowering.triple(&id, "direction", DEFAULT_DIRECTION)?,
                ),
                ("point", lowering.triple(&id, "point", [0.; 3])?),
                ("yaw", lowering.number(&id, "yaw", 0.)?),
                ("pitch", lowering.number(&id, "pitch", 0.)?),
                ("alpha", lowering.number(&id, "alpha", 1.)?),
            ];
            lowering.resolve(
                &mut inputs,
                Role {
                    peak: None,
                    scale: &["alpha"],
                    turn: &["yaw", "pitch"],
                },
            )?;
            let [(_, direction), (_, point), (_, yaw), (_, pitch), (_, alpha)] = inputs;
            let positions =
                lowering.field(lowering.cells.iter().flat_map(|c| c.uvz).collect(), 3)?;
            let mut ports = vec![
                ("dir_x", direction.parts[0].clone()),
                ("dir_y", direction.parts[1].clone()),
                ("dir_z", direction.parts[2].clone()),
                ("point_x", point.parts[0].clone()),
                ("point_y", point.parts[1].clone()),
                ("point_z", point.parts[2].clone()),
                ("yaw", yaw.parts[0].clone()),
                ("pitch", pitch.parts[0].clone()),
                ("alpha", alpha.parts[0].clone()),
                ("positions", positions),
                ("base", number(base)),
                ("mirrors", number(heads.mirrors.len() as f64)),
            ];
            let names: Vec<(String, String)> = (0..AIM_MIRRORS)
                .map(|m| (format!("fold{m}"), format!("normal{m}")))
                .collect();
            for (m, (fold, normal)) in names.iter().enumerate() {
                let (folded, plane) = heads
                    .mirrors
                    .get(m)
                    .cloned()
                    .unwrap_or_else(|| (number(0.), number(0.)));
                ports.push((fold.as_str(), folded));
                ports.push((normal.as_str(), plane));
            }
            let out = lowering.kernel(Kernel::AimOut, ports);
            outputs.insert(crate::aim::TURN_OUTPUT.to_string(), output(&out, "turn"));
            types.insert(
                crate::aim::TURN_OUTPUT.to_string(),
                Output {
                    value_type: ValueType::Signal(SignalType::new(
                        Unit::Number,
                        Channels::components(crate::aim::TURN_CHANNELS)?,
                    )),
                    rate: Rate::Frame,
                },
            );
            ("aim", output(&out, "aim"))
        }
        kind => {
            return Err(Error(format!(
                "{id}: a {} node is not an output",
                kind.name()
            )))
        }
    };
    let terminal_id = lowering.library_node("output", vec![(terminal.0, terminal.1)]);
    outputs.insert(
        super::OUTPUT.to_string(),
        output(&terminal_id, super::OUTPUT),
    );
    Ok(Definition {
        name: "Clip graph".into(),
        inputs: BTreeMap::new(),
        outputs: types,
        body: Body::Graph(Graph {
            nodes: lowering.nodes,
            outputs,
        }),
    })
}
