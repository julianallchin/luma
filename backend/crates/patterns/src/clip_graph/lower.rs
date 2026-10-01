//! Lowering: a checked clip graph becomes a graph of kernel nodes over the
//! frame's cells (spec 5.1). Values become constants; the heads pipeline
//! becomes constant unit, span and position fields; every clock becomes
//! one event table; and the output node combines each clock's live events
//! into one layer (spec 5.3): light keeps the largest per channel, aim lays
//! the newest over the older by its alpha, and no live event is
//! transparent.
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
/// first), and the clock whose events it carries.
#[derive(Clone)]
struct Lowered {
    parts: [Binding; 3],
    clock: Option<String>,
}

/// A lowered heads wire.
#[derive(Clone)]
struct Heads {
    units: Units,
    /// Per cell, the unit's (folded) position: (heads, time, 3).
    positions: Binding,
    /// Per cell, the unit's position before any mirror.
    unfolded: Binding,
    /// Per stacked mirror, in order.
    mirrors: Vec<Mirror>,
    /// A shuffle's event index and its clock.
    shuffle: Option<(Binding, Option<String>)>,
}

/// One mirror of a heads wire: whether each cell was folded and the unit
/// direction it folded along.
#[derive(Clone)]
struct Mirror {
    folded: Binding,
    direction: Binding,
}

#[derive(Clone)]
struct Events {
    progress: Binding,
    present: Binding,
    index: Binding,
}

struct Lowering<'a> {
    graph: &'a ClipGraph,
    /// The clip's length in beats: the duration of a time node with
    /// neither `every` nor `duration`.
    clip_duration: f64,
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
        clock: None,
    }
}
fn constant3(value: [f64; 3]) -> Lowered {
    Lowered {
        parts: value.map(number),
        clock: None,
    }
}
/// Component `c` of a vector or color wire, as a number wire.
fn component(lowered: &Lowered, c: usize) -> Lowered {
    Lowered {
        parts: std::array::from_fn(|_| lowered.parts[c].clone()),
        clock: lowered.clock.clone(),
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

    fn kernel_named(&mut self, kernel: Kernel, inputs: Vec<(String, Binding)>) -> String {
        let id = format!("k{}", self.nodes.len());
        self.nodes.insert(
            id.clone(),
            Node {
                definition: kernel.id().into(),
                inputs: inputs.into_iter().collect(),
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

    /// A number input: its value, its wire, or `empty`.
    fn number(&mut self, id: &str, input: &str, empty: f64) -> Result<Lowered> {
        match self.node(id).inputs.get(input).cloned() {
            None => Ok(constant(empty)),
            Some(item) => self.item(id, input, &item),
        }
    }

    /// One number: a value or a curve.
    fn item(&mut self, id: &str, input: &str, item: &Input) -> Result<Lowered> {
        match item {
            Input::Number(value) => Ok(constant(*value)),
            Input::Wire(source) => self.value(source),
            _ => Err(Error(format!("{id}.{input}: expected a number"))),
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
            Kind::Math => self.math(id)?,
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

    /// The events of time node `id`, which has `every`: one event table
    /// per clock key, so time nodes with equal `every` and `duration`
    /// share their events.
    fn events(&mut self, id: &str) -> Result<(Events, String)> {
        let key = self
            .graph
            .clock_key(id)
            .ok_or_else(|| Error(format!("{id}: a time node without every has no events")))?;
        if let Some(known) = self.clocks.get(&key) {
            return Ok((known.clone(), key));
        }
        let every = self.number(id, "every", 1.)?;
        let duration = self.duration(id)?;
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
        self.clocks.insert(key.clone(), lowered.clone());
        Ok((lowered, key))
    }

    /// A time node's duration in beats: its own, else its `every`, else
    /// the clip's length.
    fn duration(&mut self, id: &str) -> Result<Lowered> {
        let node = self.node(id);
        if node.inputs.contains_key("duration") {
            self.number(id, "duration", 1.)
        } else if node.inputs.contains_key("every") {
            self.number(id, "every", 1.)
        } else {
            Ok(constant(self.clip_duration))
        }
    }

    // ---- coordinates ----

    /// Each head's clock: the event's progress less the head's delay over
    /// the duration, plus the phase, wrapped (spec section 0).
    fn time(&mut self, id: &str) -> Result<Lowered> {
        let node = self.node(id).clone();
        let (progress, clock) = if node.inputs.contains_key("every") {
            let (events, clock) = self.events(id)?;
            (events.progress, Some(clock))
        } else if node.inputs.contains_key("duration") {
            // Once, `duration` beats long from the clip start; past its end
            // the clock runs on above 1, so curves hold their last value.
            let duration = self.number(id, "duration", 1.)?;
            let table = self.kernel(
                Kernel::ClockTable,
                vec![("period", duration.parts[0].clone())],
            );
            let turns = self.kernel(Kernel::Clock, vec![("table", output(&table, "value"))]);
            (output(&turns, "value"), None)
        } else {
            (
                output(&self.kernel(Kernel::ClipProgress, vec![]), "value"),
                None,
            )
        };
        if !node.inputs.contains_key("delay") && !node.inputs.contains_key("phase") {
            return Ok(Lowered {
                parts: std::array::from_fn(|_| progress.clone()),
                clock,
            });
        }
        let delay = self.number(id, "delay", 0.)?;
        let duration = self.duration(id)?;
        let phase = self.number(id, "phase", 0.)?;
        let has_phase = node.inputs.contains_key("phase");
        let shifted = self.kernel(
            Kernel::Shift,
            vec![
                ("progress", progress),
                ("delay", delay.parts[0].clone()),
                ("duration", duration.parts[0].clone()),
                ("phase", phase.parts[0].clone()),
                ("has_phase", number(if has_phase { 1. } else { 0. })),
            ],
        );
        Ok(Lowered {
            parts: std::array::from_fn(|_| output(&shifted, "value")),
            clock: clock_of([&clock, &delay.clock, &phase.clock]),
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
        let centre = self.triple(id, "centre", [0.5; 3])?;
        let rank = self.rank(&heads)?;
        let unit = self.index_field(&heads.units.unit)?;
        let span = self.index_field(&heads.units.span)?;
        let dir = direction.clone().unwrap_or_else(|| constant(0.));
        let ports = vec![
            ("positions".to_string(), heads.positions.clone()),
            ("base".into(), heads.unfolded.clone()),
            ("unit".into(), unit),
            ("span".into(), span),
            ("rank".into(), rank),
            ("dir_x".into(), dir.parts[0].clone()),
            ("dir_y".into(), dir.parts[1].clone()),
            ("dir_z".into(), dir.parts[2].clone()),
            ("centre_x".into(), centre.parts[0].clone()),
            ("centre_y".into(), centre.parts[1].clone()),
            ("centre_z".into(), centre.parts[2].clone()),
            ("kind".into(), number(kind)),
            (
                "has_direction".into(),
                number(if direction.is_some() { 1. } else { 0. }),
            ),
            ("wrap".into(), number(if wrap { 1. } else { 0. })),
        ];
        let axis = self.kernel_named(Kernel::Axis, ports);
        let shuffled = heads.shuffle.as_ref().and_then(|(_, clock)| clock.clone());
        if !["at", "shift", "scale"]
            .iter()
            .any(|k| node.inputs.contains_key(*k))
        {
            return Ok(Lowered {
                parts: std::array::from_fn(|_| output(&axis, "value")),
                clock: shuffled,
            });
        }
        let at = self.number(id, "at", 0.)?;
        let shift = self.number(id, "shift", 0.)?;
        let scale = self.number(id, "scale", 1.)?;
        let slid = self.kernel(
            Kernel::Slide,
            vec![
                ("a", output(&axis, "value")),
                ("at", at.parts[0].clone()),
                ("shift", shift.parts[0].clone()),
                ("scale", scale.parts[0].clone()),
                ("wrap", number(if wrap { 1. } else { 0. })),
            ],
        );
        Ok(Lowered {
            parts: std::array::from_fn(|_| output(&slid, "value")),
            clock: clock_of([&shuffled, &at.clock, &shift.clock, &scale.clock]),
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
        // The band's level, 0–1 over the whole track.
        let energy = self.library_node(
            "band_energy",
            vec![("low_hz", number(low)), ("high_hz", number(high))],
        );
        Ok(Lowered {
            parts: std::array::from_fn(|_| output(&energy, "value")),
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
            clock: clock_of([&x.clock, &low.clock, &high.clock]),
        })
    }

    /// A math node: its values combined pairwise, left to right, per
    /// component; a number broadcasts over a vector or a color.
    fn math(&mut self, id: &str) -> Result<Lowered> {
        let node = self.node(id).clone();
        let op = match node.setting("op").unwrap_or("*") {
            "+" => 1.,
            "-" => 2.,
            "max" => 3.,
            "min" => 4.,
            _ => 0.,
        };
        let Some(Input::List(items)) = node.inputs.get("values") else {
            return Err(Error(format!("{id}.values: expected a list")));
        };
        let wide = self.graph.value_kind(id) != Some("number");
        let mut so_far: Option<Lowered> = None;
        for item in items {
            let item = self.item(id, "values", item)?;
            so_far = Some(match so_far {
                None => item,
                Some(left) => {
                    let parts = if wide {
                        let mut parts = Vec::with_capacity(3);
                        for c in 0..3 {
                            let k = self.kernel(
                                Kernel::Math,
                                vec![
                                    ("a", left.parts[c].clone()),
                                    ("b", item.parts[c].clone()),
                                    ("op", number(op)),
                                ],
                            );
                            parts.push(output(&k, "value"));
                        }
                        <[Binding; 3]>::try_from(parts).expect("three parts")
                    } else {
                        let k = self.kernel(
                            Kernel::Math,
                            vec![
                                ("a", left.parts[0].clone()),
                                ("b", item.parts[0].clone()),
                                ("op", number(op)),
                            ],
                        );
                        std::array::from_fn(|_| output(&k, "value"))
                    };
                    Lowered {
                        parts,
                        clock: clock_of([&left.clock, &item.clock]),
                    }
                }
            });
        }
        so_far.ok_or_else(|| Error(format!("{id}.values: expected two or more items")))
    }

    // ---- heads ----

    fn base_heads(&self) -> Result<Heads> {
        let positions = self.field(self.cells.iter().flat_map(|c| c.uvz).collect(), 3)?;
        Ok(Heads {
            units: Units::base(&self.cells),
            unfolded: positions.clone(),
            positions,
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
                let direction = self.optional_triple(id, "direction")?;
                let at = self.number(id, "at", 0.5)?;
                let plane = direction.clone().unwrap_or_else(|| constant(0.));
                let fold = self.kernel(
                    Kernel::Fold,
                    vec![
                        ("positions", heads.positions.clone()),
                        ("base", heads.unfolded.clone()),
                        ("span", self.index_field(&heads.units.span)?),
                        ("normal_x", plane.parts[0].clone()),
                        ("normal_y", plane.parts[1].clone()),
                        ("normal_z", plane.parts[2].clone()),
                        ("at", at.parts[0].clone()),
                        (
                            "has_normal",
                            number(if direction.is_some() { 1. } else { 0. }),
                        ),
                    ],
                );
                heads.positions = output(&fold, "positions");
                heads.mirrors.push(Mirror {
                    folded: output(&fold, "folded"),
                    direction: output(&fold, "normal"),
                });
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
                let unit_field = unit.clone();
                let group = self.kernel(
                    Kernel::Group,
                    vec![("positions", heads.positions.clone()), ("unit", unit)],
                );
                heads.positions = output(&group, "positions");
                if heads.mirrors.is_empty() {
                    heads.unfolded = heads.positions.clone();
                } else {
                    let base = self.kernel(
                        Kernel::Group,
                        vec![("positions", heads.unfolded.clone()), ("unit", unit_field)],
                    );
                    heads.unfolded = output(&base, "positions");
                }
            }
            Kind::Split => {
                let by = match node.setting("by") {
                    Some("group") => SplitBy::Group,
                    _ => SplitBy::Fixture,
                };
                heads.units = heads.units.split(&self.cells, by);
            }
            Kind::Shuffle => {
                heads.shuffle = Some(match self.wire(id, "time") {
                    Some(source) if self.graph.clock_key(&source).is_some() => {
                        let (events, clock) = self.events(&source)?;
                        (events.index, Some(clock))
                    }
                    _ => (number(0.), None),
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

    /// `a × b`, per part.
    fn times(&mut self, a: &Binding, b: &Binding) -> Binding {
        let k = self.kernel(
            Kernel::Math,
            vec![("a", a.clone()), ("b", b.clone()), ("op", number(0.))],
        );
        output(&k, "value")
    }

    /// The product of `factors`, or 1.
    fn product(&mut self, factors: &[Binding]) -> Binding {
        let mut out: Option<Binding> = None;
        for factor in factors {
            out = Some(match out {
                None => factor.clone(),
                Some(so_far) => self.times(&so_far, factor),
            });
        }
        out.unwrap_or_else(|| number(1.))
    }

    /// The live events of every clock the output's inputs carry, combined
    /// into one layer (spec 5.3). A light output (`over` false) multiplies
    /// each part's factors per event, premultiplied by alpha, and keeps the
    /// largest per part across the live events; an aim (`over` true) lays
    /// each newer event over the older by its alpha. Where a clock has no
    /// live event the clip is transparent: alpha 0.
    ///
    /// `parts[p]` are the factors of output part `p`; `alpha` is the
    /// output's alpha. Returns the parts, premultiplied for light, and the
    /// alpha.
    fn layer(
        &mut self,
        parts: Vec<Vec<Lowered>>,
        alpha: Lowered,
        over: bool,
    ) -> Result<(Vec<Binding>, Binding)> {
        let mut clocks: Vec<String> = parts
            .iter()
            .flatten()
            .chain([&alpha])
            .filter_map(|l| l.clock.clone())
            .collect();
        clocks.sort();
        clocks.dedup();
        let carries = |l: &Lowered, clock: &str| l.clock.as_deref() == Some(clock);
        // Factors that carry no clock, per part; light is premultiplied.
        let mut out: Vec<Vec<Binding>> = parts
            .iter()
            .map(|factors| {
                let mut kept: Vec<Binding> = factors
                    .iter()
                    .filter(|l| l.clock.is_none())
                    .map(|l| l.parts[0].clone())
                    .collect();
                if !over && alpha.clock.is_none() {
                    kept.push(alpha.parts[0].clone());
                }
                kept
            })
            .collect();
        let mut alphas: Vec<Binding> = if alpha.clock.is_none() {
            vec![alpha.parts[0].clone()]
        } else {
            vec![]
        };
        for clock in clocks {
            let events = self.clocks[&clock].clone();
            let alpha_carried = carries(&alpha, &clock);
            let weight = if alpha_carried {
                alpha.parts[0].clone()
            } else {
                number(1.)
            };
            let over_flag = number(if over { 1. } else { 0. });
            let present = events.present.clone();
            let combine = |this: &mut Self, value: Binding| {
                this.kernel(
                    Kernel::Combine,
                    vec![
                        ("value", value),
                        ("weight", weight.clone()),
                        ("present", present.clone()),
                        ("over", over_flag.clone()),
                    ],
                )
            };
            let presence = combine(self, number(1.));
            alphas.push(output(&presence, "alpha"));
            for (p, factors) in parts.iter().enumerate() {
                let mut own: Vec<Binding> = factors
                    .iter()
                    .filter(|l| carries(l, &clock))
                    .map(|l| l.parts[0].clone())
                    .collect();
                // Light is premultiplied, so a clock that carries alpha
                // scales every part.
                if !over && alpha_carried {
                    own.push(alpha.parts[0].clone());
                }
                if own.is_empty() {
                    continue;
                }
                let value = self.product(&own);
                let combined = combine(self, value);
                out[p].push(output(&combined, "value"));
            }
        }
        let parts = out.iter().map(|factors| self.product(factors)).collect();
        let alpha = self.product(&alphas);
        Ok((parts, alpha))
    }
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
            clip_duration: frame.clip_duration,
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

/// Coordinate node `id` of a checked graph alone, per head and live event,
/// as [`COORDINATE`]. An editor reads it to mark where the heads fall on
/// the coordinate.
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
    let outputs = BTreeMap::from([(COORDINATE.to_string(), value.parts[0].clone())]);
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
    // The clip's opacity, for color and strobe: the compositor mixes the
    // clip with the light below by it.
    let mut alpha_port: Option<Binding> = None;
    let terminal = match out.kind {
        Kind::Color => {
            let color = lowering.triple(&id, "color", [1.; 3])?;
            let brightness = lowering.number(&id, "brightness", 1.)?;
            let alpha = lowering.number(&id, "alpha", 1.)?;
            let parts = (0..3)
                .map(|c| vec![component(&color, c), brightness.clone()])
                .collect();
            let (parts, alpha) = lowering.layer(parts, alpha, false)?;
            let out = lowering.kernel(
                Kernel::ColorOut,
                vec![
                    ("r", parts[0].clone()),
                    ("g", parts[1].clone()),
                    ("b", parts[2].clone()),
                    ("alpha", alpha.clone()),
                ],
            );
            alpha_port = Some(alpha);
            ("color", output(&out, "color"))
        }
        Kind::Strobe => {
            let rate = lowering.number(&id, "rate", 0.5)?;
            let alpha = lowering.number(&id, "alpha", 1.)?;
            let (parts, alpha) = lowering.layer(vec![vec![rate]], alpha, false)?;
            let out = lowering.kernel(
                Kernel::StrobeOut,
                vec![("rate", parts[0].clone()), ("alpha", alpha.clone())],
            );
            alpha_port = Some(alpha);
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
            let direction = lowering.triple(&id, "direction", DEFAULT_DIRECTION)?;
            let point = lowering.triple(&id, "point", [0.; 3])?;
            let yaw = lowering.number(&id, "yaw", 0.)?;
            let pitch = lowering.number(&id, "pitch", 0.)?;
            let alpha = lowering.number(&id, "alpha", 1.)?;
            let mut parts: Vec<Vec<Lowered>> = (0..3)
                .map(|c| vec![component(&direction, c)])
                .chain((0..3).map(|c| vec![component(&point, c)]))
                .collect();
            parts.push(vec![yaw]);
            parts.push(vec![pitch]);
            let (parts, alpha) = lowering.layer(parts, alpha, true)?;
            let positions =
                lowering.field(lowering.cells.iter().flat_map(|c| c.uvz).collect(), 3)?;
            let mut ports = vec![
                ("dir_x", parts[0].clone()),
                ("dir_y", parts[1].clone()),
                ("dir_z", parts[2].clone()),
                ("point_x", parts[3].clone()),
                ("point_y", parts[4].clone()),
                ("point_z", parts[5].clone()),
                ("yaw", parts[6].clone()),
                ("pitch", parts[7].clone()),
                ("alpha", alpha),
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
                    .map(|mirror| (mirror.folded.clone(), mirror.direction.clone()))
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
    let mut ports = vec![(terminal.0, terminal.1)];
    if let Some(alpha) = alpha_port {
        ports.push(("alpha", alpha));
    }
    let terminal_id = lowering.library_node("output", ports);
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
