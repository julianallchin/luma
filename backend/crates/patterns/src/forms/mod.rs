//! Clip forms: shipped graphs that a clip names together with a value for
//! every input. A form input takes a plain value of its type or, where the
//! input's `promotable` list allows it, a source. Sources become graph nodes
//! when the clip is prepared; the stored clip keeps only the values.
use crate::*;
use std::collections::BTreeMap;

pub(crate) mod ops;
mod pace;
mod upgrade;

pub use upgrade::upgrade;

/// Every form id. An id never changes meaning; a new meaning is a new version.
pub const FORMS: [&str; 5] = [
    "color@1",
    "color.sparkle@1",
    "color.noise@1",
    "strobe.constant@1",
    "aim@1",
];

pub fn is_form(id: &str) -> bool {
    FORMS.contains(&id)
}

/// The blend modes a clip of form `id` takes, in the order a picker lists
/// them. An aim either replaces the aim under it or turns it; light takes
/// the light modes.
pub fn blend_modes(id: &str) -> &'static [BlendMode] {
    if id == "aim@1" {
        &[BlendMode::Replace, BlendMode::Offset]
    } else {
        &BlendMode::LIGHT
    }
}

/// A form's inputs in the order an editor shows them. The definition keeps
/// its inputs in a map, so the order lives here.
pub fn input_order(id: &str) -> Option<&'static [&'static str]> {
    Some(match id {
        "color@1" => &["color", "brightness", "every", "alpha"],
        "color.sparkle@1" => &[
            "color",
            "every",
            "duration",
            "coverage",
            "brightness",
            "grain",
            "alpha",
        ],
        "color.noise@1" => &["color", "speed", "scale", "contrast", "alpha"],
        "strobe.constant@1" => &["rate", "alpha"],
        "aim@1" => &[
            "base",
            "direction",
            "point",
            "fan",
            "axis",
            "motion",
            "shape",
            "size",
            "every",
            "spread",
            "speed",
            "alpha",
        ],
        _ => return None,
    })
}

/// Inputs that set a speed. A `time` source on one of them is summed over
/// the clip like an odometer instead of read frame by frame.
const SPEEDS: [&str; 4] = ["every", "travel", "duration", "speed"];

use SourceKind::{Audio, Hit, Noise, Space, Time};

// ---------------------------------------------------------------------------
// Building blocks

fn signal_type(value_type: ValueType) -> ValueType {
    value_type
        .signal_type()
        .map_or(value_type, ValueType::Signal)
}

fn input(
    name: &str,
    description: &str,
    default: Value,
    rate: Rate,
    promotable: &[SourceKind],
) -> Input {
    Input {
        optional: false,
        name: name.into(),
        description: description.into(),
        value_type: signal_type(default.value_type()),
        rate,
        default: Some(default),
        author: None,
        promotable: promotable.to_vec(),
    }
}
/// The most degrees of phase an aim's spread puts across the axis: four
/// cycles, either way.
pub const MAX_SPREAD: f64 = 1440.0;
/// The widest stroke of a moving space source, in axis lengths.
pub const MAX_WIDTH: f64 = 4.0;
fn number(mut input: Input, min: f64, max: f64) -> Input {
    input.author = Some(Author::Number {
        min: Some(min),
        max: Some(max),
    });
    input
}
fn choice(mut input: Input, options: Vec<(&str, Value)>, custom: bool) -> Input {
    input.author = Some(Author::Choice {
        options: options
            .into_iter()
            .map(|(label, value)| Preset {
                label: label.into(),
                value,
            })
            .collect(),
        custom,
    });
    input
}

fn i(name: &str) -> Binding {
    Binding::Input { input: name.into() }
}
fn c(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn n(value: f64) -> Binding {
    Value::Number(value).into()
}

#[derive(Default)]
struct Body(BTreeMap<String, Node>);
impl Body {
    fn node(&mut self, id: &str, definition: &str, inputs: Vec<(&str, Binding)>) -> &mut Self {
        self.0.insert(
            id.into(),
            Node {
                definition: definition.into(),
                inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            },
        );
        self
    }
    fn multiply(&mut self, id: &str, a: Binding, b: Binding) -> Binding {
        self.node(id, "core/multiply", vec![("a", a), ("b", b)]);
        c(id, "value")
    }
    fn add(&mut self, id: &str, a: Binding, b: Binding) -> Binding {
        self.node(id, "core/add", vec![("a", a), ("b", b)]);
        c(id, "value")
    }
    fn subtract(&mut self, id: &str, a: Binding, b: Binding) -> Binding {
        self.node(id, "core/subtract", vec![("a", a), ("b", b)]);
        c(id, "value")
    }
    fn greater(&mut self, id: &str, a: Binding, b: Binding) -> Binding {
        self.node(
            id,
            "core/greater",
            vec![("a", a), ("b", b), ("tolerance", n(1e-9))],
        );
        c(id, "mask")
    }
    fn envelope(&mut self, id: &str, progress: Binding, shape: Binding) -> Binding {
        self.node(
            id,
            "envelope",
            vec![("progress", progress), ("shape", shape)],
        );
        c(id, "value")
    }
    /// Apply a color and optional strobe to the selected heads.
    fn form(self, name: &str, inputs: Vec<(&str, Input)>, color: Binding) -> Definition {
        self.form_with(name, inputs, vec![("color", color)])
    }
    fn form_with(
        mut self,
        name: &str,
        inputs: Vec<(&str, Input)>,
        writes: Vec<(&str, Binding)>,
    ) -> Definition {
        self.node("output", "output", writes);
        Definition {
            name: name.into(),
            inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            outputs: BTreeMap::from([(
                "lighting".into(),
                Output {
                    value_type: ValueType::Lighting,
                    rate: Rate::Frame,
                },
            )]),
            body: crate::Body::Graph(Graph {
                nodes: self.0,
                outputs: BTreeMap::from([("lighting".into(), c("output", "lighting"))]),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared inputs and named choices

fn color_input() -> Input {
    input(
        "Color",
        "Color of the light",
        Value::Color([1.0; 3]),
        Rate::Frame,
        &[Time],
    )
}
fn alpha_input(promotable: &[SourceKind]) -> Input {
    input(
        "Alpha",
        "How much the clip counts: its brightness and its opacity",
        Value::Proportion(1.0),
        Rate::Frame,
        promotable,
    )
}
fn every_input(default: f64, description: &str, promotable: &[SourceKind]) -> Input {
    input(
        "Every",
        description,
        Value::Beats(default),
        Rate::Fixed,
        promotable,
    )
}

fn curve(points: &[[f64; 2]], eases: &[Ease]) -> Value {
    Value::Envelope(Envelope::eased(points.to_vec(), eases))
}

/// Positions at the centers of N equal parts, without gliding.
pub fn steps_path(count: usize) -> Envelope {
    let count = count.max(1);
    let mut points: Vec<[f64; 2]> = (0..count)
        .map(|i| [i as f64 / count as f64, (i as f64 + 0.5) / count as f64])
        .collect();
    points.push([1.0, (count as f64 - 0.5) / count as f64]);
    Envelope::eased(points, &vec![Ease::Hold; count])
}

/// Named paths: where a stroke is over its life (0 = axis start, 1 = end).
pub fn path_presets() -> Vec<(&'static str, Value)> {
    vec![
        ("Forward", curve(&[[0., 0.], [1., 1.]], &[])),
        ("Backward", curve(&[[0., 1.], [1., 0.]], &[])),
        ("Bounce", curve(&[[0., 0.], [0.5, 1.], [1., 0.]], &[])),
        ("Ease in", curve(&[[0., 0.], [1., 1.]], &[Ease::EaseIn])),
        ("Ease out", curve(&[[0., 0.], [1., 1.]], &[Ease::EaseOut])),
        (
            "Ease in-out",
            curve(&[[0., 0.], [1., 1.]], &[Ease::EaseInOut]),
        ),
        ("Steps (2)", Value::Envelope(steps_path(2))),
        ("Steps (3)", Value::Envelope(steps_path(3))),
        ("Steps (4)", Value::Envelope(steps_path(4))),
        ("Steps (8)", Value::Envelope(steps_path(8))),
    ]
}

/// Named shapes: brightness across a stroke, from its tail (0) to its head
/// (1) in the direction of travel.
pub fn shape_presets() -> Vec<(&'static str, Value)> {
    vec![
        ("Hard", curve(&[[0., 1.], [1., 1.]], &[])),
        (
            "Soft",
            curve(
                &[[0., 0.], [0.5, 1.], [1., 0.]],
                &[Ease::Bezier([0.4, 0., 0.6, 1.]); 2],
            ),
        ),
        ("Comet", curve(&[[0., 0.], [0.95, 1.], [1., 0.]], &[])),
        (
            "Reverse comet",
            curve(&[[0., 0.], [0.05, 1.], [1., 0.]], &[]),
        ),
        (
            "Spike",
            curve(
                &[[0., 0.], [0.5, 1.], [1., 0.]],
                &[
                    Ease::Bezier([0.8, 0., 1., 0.3]),
                    Ease::Bezier([0., 0.7, 0.2, 1.]),
                ],
            ),
        ),
    ]
}

/// Named curves for a gradient read over time or per hit: how one pass
/// crosses the gradient.
pub fn progress_presets() -> Vec<(&'static str, Value)> {
    vec![
        ("Linear", curve(&[[0., 0.], [1., 1.]], &[])),
        (
            "Ease in-out",
            curve(&[[0., 0.], [1., 1.]], &[Ease::EaseInOut]),
        ),
        (
            "There and back",
            curve(&[[0., 0.], [0.5, 1.], [1., 0.]], &[]),
        ),
        ("Steps (2)", Value::Envelope(palette_steps(2))),
        ("Steps (3)", Value::Envelope(palette_steps(3))),
        ("Steps (4)", Value::Envelope(palette_steps(4))),
        ("Steps (6)", Value::Envelope(palette_steps(6))),
        ("Steps (8)", Value::Envelope(palette_steps(8))),
    ]
}

/// N gradient positions i / (N − 1), evenly spaced from 0 to 1, each held
/// for 1/N of a pass. A gradient used as a palette then shows each of its N
/// stops with no blending. Chase `steps_path` uses part centers instead.
pub fn palette_steps(count: usize) -> Envelope {
    let count = count.max(2);
    let last = (count - 1) as f64;
    let mut points: Vec<[f64; 2]> = (0..count)
        .map(|i| [i as f64 / count as f64, i as f64 / last])
        .collect();
    points.push([1.0, 1.0]);
    Envelope::eased(points, &vec![Ease::Hold; count])
}

/// An axis over the whole selection. Radial and angle read in the best-fit
/// plane around the centroid.
fn axis(source: MappingSource) -> Value {
    let round = matches!(source, MappingSource::Radial | MappingSource::Angle);
    Value::Mapping(MappingSpec {
        span: Default::default(),
        plane: round.then_some(crate::AxisPlane::Auto),
        source,
        per_group: false,
        reverse: false,
        mirror: None,
    })
}
pub fn axis_presets() -> Vec<(&'static str, Value)> {
    vec![
        ("Order", axis(MappingSource::Order)),
        ("X", axis(MappingSource::U)),
        ("Y", axis(MappingSource::V)),
        ("Z", axis(MappingSource::Z)),
        ("Radial", axis(MappingSource::Radial)),
        ("Angle", axis(MappingSource::Angle)),
        ("Random", axis(MappingSource::Random)),
    ]
}
// ---------------------------------------------------------------------------
// The forms

pub(crate) fn definitions() -> Vec<(&'static str, Definition)> {
    vec![
        ("color@1", color()),
        ("color.sparkle@1", sparkle()),
        ("color.noise@1", noise()),
        ("strobe.constant@1", strobe()),
        ("aim@1", aim()),
    ]
}

/// One color: each input is fixed, or follows a source over time, per hit,
/// across space or across space and time. A space source with `move` is a
/// chase: a stroke that travels along its axis once per hit.
fn color() -> Definition {
    let mut body = Body::default();
    // One hit every `every` beats; zero is one hit over the clip. A hit
    // source reads the share of its hit that has passed. A moving space
    // source sends one stroke per hit, and hit sources then follow each
    // stroke's life instead.
    body.node("clock", "core/odometer", vec![("period", i("every"))]);
    body.node(
        "life",
        "core/fraction",
        vec![("value", c("clock", "turns"))],
    );
    // A moving brightness has one channel per live stroke; the brightest
    // stroke wins at each head.
    let level = body.multiply("level", i("brightness"), i("alpha"));
    body.node("mask", "core/channel_maximum", vec![("value", level)]);
    let color = body.multiply("color", i("color"), c("mask", "value"));
    body.form(
        "Color",
        vec![
            (
                "color",
                input(
                    "Color",
                    "Color of the light",
                    Value::Color([1.0; 3]),
                    Rate::Frame,
                    &[Time, Hit, Space],
                ),
            ),
            (
                "brightness",
                input(
                    "Brightness",
                    "Brightness of the light. Alpha is how much the clip covers the layers under it",
                    Value::Proportion(1.0),
                    Rate::Frame,
                    &[Time, Hit, Noise, Audio, Space],
                ),
            ),
            (
                "every",
                every_input(
                    0.0,
                    "Beats between hits; 0 is one hit over the clip. A hit source plays once per hit, and a moving space source sends one stroke per hit",
                    &[Time],
                ),
            ),
            ("alpha", alpha_input(&[Time, Hit, Noise, Audio])),
        ],
        color,
    )
}

/// A still space source: each head's position on the axis reads the
/// gradient or the curve.
fn still_space(body: &mut Body, key: &impl Fn(&str) -> String, space: &SpaceSource) -> Binding {
    body.node(
        &key("position"),
        "mapped_position",
        vec![("mapping", Value::Mapping(space.axis.clone()).into())],
    );
    let position = c(&key("position"), "value");
    match (&space.gradient, &space.curve) {
        (Some(gradient), _) => {
            body.node(
                &key("sample"),
                "sample_gradient",
                vec![
                    ("gradient", Value::Gradient(gradient.clone()).into()),
                    ("position", position),
                ],
            );
            c(&key("sample"), "color")
        }
        (None, Some(curve)) => body.envelope(
            &key("value"),
            position,
            Value::Envelope(curve.clone()).into(),
        ),
        (None, None) => unreachable!("validated space source"),
    }
}

/// A moving space source: one stroke per hit of `every`, each `travel`
/// beats long, with the source's curve across it. Returns the stroke value,
/// one channel per live stroke, and the strokes' life for hit sources.
fn stroke(
    body: &mut Body,
    key: &impl Fn(&str) -> String,
    space: &SpaceSource,
    movement: &Movement,
) -> (Binding, Binding) {
    let mut life = vec![("every", i("every"))];
    match &movement.travel {
        Value::Time(curve) => life.push(("life_curve", Value::Time(curve.clone()).into())),
        travel => life.push(("life", travel.clone().into())),
    }
    body.node(&key("life"), "core/event_life", life);
    let progress = c(&key("life"), "progress");
    let path: Binding = Value::Envelope(movement.path.clone()).into();
    let shape: Binding = Value::Envelope(space.curve.clone().expect("validated curve")).into();
    let axis: Binding = Value::Mapping(space.axis.clone()).into();
    let boundary: Binding = Value::Boundary(movement.boundary).into();
    let width = match &movement.width {
        Value::Time(curve) => time_curve(
            body,
            &|part| key(&format!("width_{part}")),
            Value::Time(curve.clone()),
        ),
        Value::Hit(curve) => {
            body.node(
                &key("width_curve"),
                "core/curve",
                vec![
                    ("curve", Value::Time(curve.clone()).into()),
                    ("progress", progress.clone()),
                ],
            );
            c(&key("width_curve"), "value")
        }
        width => width.clone().into(),
    };
    let position = body.envelope(&key("position"), progress.clone(), path.clone());
    // Which way the stroke travels now. A still path (a step) keeps the
    // direction of the whole path.
    let ahead_at = body.add(&key("ahead_at"), progress.clone(), n(1e-3));
    let behind_at = body.subtract(&key("behind_at"), progress, n(1e-3));
    let ahead = body.envelope(&key("ahead"), ahead_at, path.clone());
    let behind = body.envelope(&key("behind"), behind_at, path.clone());
    let velocity = body.subtract(&key("velocity"), ahead, behind);
    let path_end = body.envelope(
        &key("path_end"),
        Value::Proportion(1.0).into(),
        path.clone(),
    );
    let path_start = body.envelope(
        &key("path_start"),
        Value::Proportion(0.0).into(),
        path.clone(),
    );
    let overall = body.subtract(&key("overall"), path_end, path_start);
    let moving_back = body.greater(&key("moving_back"), n(0.0), velocity.clone());
    let moving_on = body.greater(&key("moving_on"), velocity, n(0.0));
    let net_back = body.greater(&key("net_back"), n(0.0), overall);
    let moving = body.add(&key("moving"), moving_back.clone(), moving_on);
    let still = body.subtract(&key("still"), n(1.0), moving);
    let still_back = body.multiply(&key("still_back"), still, net_back);
    let backward = body.add(&key("backward"), moving_back, still_back);
    // Overrun: with a clip boundary and a gliding path, the stroke enters
    // fully from outside and leaves fully. Path 0–1 maps onto centers from
    // -w/2 to 1 + w/2. Stepped paths and wrap keep exact positions.
    body.node(&key("axis"), "resolve_mapping", vec![("mapping", axis)]);
    body.node(
        &key("edge"),
        "coordinate_offset",
        vec![
            ("mapping", c(&key("axis"), "coordinates")),
            ("position", Value::Position(0.0).into()),
            ("boundary", boundary.clone()),
        ],
    );
    body.node(&key("glides"), "core/path_glides", vec![("path", path)]);
    let open = body.subtract(&key("open"), n(1.0), c(&key("edge"), "wrapped"));
    let overrun = body.multiply(&key("overrun"), open, c(&key("glides"), "value"));
    // Width: a share of the axis, or relative to the gap between strokes.
    // g is the axis share between two stroke paths (every / travel). With
    // overrun, centers are (1 + w) × g apart, so strokes touch at rel 1 when
    // w = g / (1 - g); width = r g / (1 - r g), with r g capped at 4/5 (a
    // stroke four axes wide). Without overrun, width = r g, capped at 4.
    body.node(
        &key("relative"),
        "core/choose_number",
        vec![
            ("condition", Value::Boolean(movement.width_relative).into()),
            ("yes", n(1.0)),
            ("no", n(0.0)),
        ],
    );
    let absolute = body.subtract(&key("absolute"), n(1.0), c(&key("relative"), "value"));
    let gap = body.multiply(&key("gap_share"), width.clone(), c(&key("life"), "spacing"));
    let cap_drop = body.multiply(&key("cap_drop"), overrun.clone(), n(MAX_WIDTH - 0.8));
    let cap = body.subtract(&key("gap_cap"), n(MAX_WIDTH), cap_drop);
    body.node(
        &key("gap_capped"),
        "core/minimum",
        vec![("a", gap), ("b", cap)],
    );
    let grown = body.multiply(
        &key("gap_grown"),
        c(&key("gap_capped"), "value"),
        overrun.clone(),
    );
    let rest = body.subtract(&key("gap_rest"), n(1.0), grown);
    body.node(
        &key("gap_width"),
        "core/divide",
        vec![("a", c(&key("gap_capped"), "value")), ("b", rest)],
    );
    let gap_part = body.multiply(
        &key("gap_part"),
        c(&key("gap_width"), "value"),
        c(&key("relative"), "value"),
    );
    let axis_part = body.multiply(&key("axis_part"), width, absolute);
    let width = body.add(&key("width"), gap_part, axis_part);
    body.node(
        &key("safe_width"),
        "core/maximum",
        vec![("a", width.clone()), ("b", n(1e-9))],
    );
    let from_middle = body.subtract(&key("from_middle"), position.clone(), n(0.5));
    let stretch = body.multiply(&key("stretch"), from_middle, width.clone());
    let stretch = body.multiply(&key("stretch_on"), stretch, overrun.clone());
    let center = body.add(&key("center"), position, stretch);
    // Phase across the stroke: 0 at the tail, 1 at the head.
    body.node(
        &key("offset"),
        "coordinate_offset",
        vec![
            ("mapping", c(&key("axis"), "coordinates")),
            ("position", center),
            ("boundary", boundary),
        ],
    );
    body.node(
        &key("across"),
        "core/divide",
        vec![
            ("a", c(&key("offset"), "value")),
            ("b", c(&key("safe_width"), "value")),
        ],
    );
    let phase = body.add(&key("phase"), c(&key("across"), "value"), n(0.5));
    let mirrored = body.subtract(&key("mirrored"), n(1.0), phase.clone());
    let flip = body.subtract(&key("flip"), mirrored, phase.clone());
    let flip_back = body.multiply(&key("flip_back"), flip, backward);
    let directed = body.add(&key("directed"), phase.clone(), flip_back);
    // Without overrun the stroke covers its edges, so exact positions
    // (steps) leave no head dark at a boundary. With overrun the edges are
    // open, so a stroke is dark as it starts and as it ends.
    let closed = body.subtract(&key("closed"), n(1.0), overrun);
    let edge = body.multiply(&key("edge_width"), closed, n(2e-6));
    let tail = body.add(&key("tail"), phase.clone(), edge.clone());
    let head = body.add(&key("head"), n(1.0), edge);
    let after_tail = body.greater(&key("after_tail"), tail, n(0.0));
    let before_head = body.greater(&key("before_head"), head, phase);
    let visible = body.greater(&key("visible"), width, n(0.0));
    let lit = body.envelope(&key("stroke"), directed, shape);
    let lit = body.multiply(&key("lit"), lit, after_tail);
    let lit = body.multiply(&key("lit_head"), lit, before_head);
    let lit = body.multiply(&key("lit_visible"), lit, visible);
    let lit = body.multiply(&key("lit_live"), lit, c(&key("life"), "present"));
    (lit, c(&key("life"), "progress"))
}

fn sparkle() -> Definition {
    let mut body = Body::default();
    body.node(
        "life",
        "core/event_life",
        vec![("every", i("every")), ("life", i("duration"))],
    );
    body.node(
        "share",
        "core/random_share",
        vec![
            ("index", c("life", "index")),
            ("coverage", i("coverage")),
            ("grain", i("grain")),
        ],
    );
    let lit = body.multiply("lit", c("share", "selected"), i("brightness"));
    let lit = body.multiply("lit_live", lit, c("life", "present"));
    body.node("mask", "core/channel_maximum", vec![("value", lit)]);
    let bright = body.multiply("bright", c("mask", "value"), i("alpha"));
    let color = body.multiply("color", i("color"), bright);
    let hit = || -> &[SourceKind] { &[Time, Hit, Noise, Audio] };
    body.form(
        "Sparkle",
        vec![
            ("color", color_input()),
            ("every", every_input(1.0, "Beats between events", &[Time])),
            (
                "duration",
                input(
                    "Duration",
                    "Beats one event lasts",
                    Value::Beats(1.0),
                    Rate::Fixed,
                    &[Time],
                ),
            ),
            (
                "coverage",
                input(
                    "Coverage",
                    "Share of heads lit",
                    Value::Proportion(0.5),
                    Rate::Frame,
                    hit(),
                ),
            ),
            (
                "brightness",
                input(
                    "Brightness",
                    "Brightness of the lit heads",
                    Value::Proportion(1.0),
                    Rate::Frame,
                    hit(),
                ),
            ),
            (
                "grain",
                choice(
                    input(
                        "Grain",
                        "What one head is: a head, a fixture, or a clump of N heads",
                        Value::Number(1.0),
                        Rate::Fixed,
                        &[],
                    ),
                    vec![
                        ("Head", Value::Number(1.0)),
                        ("Fixture", Value::Number(0.0)),
                        ("Clump of 2", Value::Number(2.0)),
                        ("Clump of 4", Value::Number(4.0)),
                        ("Clump of 8", Value::Number(8.0)),
                    ],
                    true,
                ),
            ),
            ("alpha", alpha_input(&[Time, Noise, Audio])),
        ],
        color,
    )
}

fn noise() -> Definition {
    let mut body = Body::default();
    body.node("clock", "core/odometer", vec![("period", i("speed"))]);
    for (id, source) in [("x", MappingSource::U), ("y", MappingSource::V)] {
        body.node(
            id,
            "mapped_position",
            vec![("mapping", axis(source).into())],
        );
    }
    body.node(
        "size",
        "core/maximum",
        vec![("a", i("scale")), ("b", n(0.01))],
    );
    body.node(
        "nx",
        "core/divide",
        vec![("a", c("x", "value")), ("b", c("size", "value"))],
    );
    body.node(
        "ny",
        "core/divide",
        vec![("a", c("y", "value")), ("b", c("size", "value"))],
    );
    body.node(
        "noise",
        "core/noise",
        vec![
            ("x", c("nx", "value")),
            ("y", c("ny", "value")),
            ("z", c("clock", "turns")),
        ],
    );
    // Contrast stretches the wandering around its middle.
    let centered = body.subtract("centered", c("noise", "value"), n(0.5));
    let gain = body.multiply("gain", i("contrast"), n(3.0));
    let gain = body.add("gain_one", gain, n(1.0));
    let stretched = body.multiply("stretched", centered, gain);
    let level = body.add("level", stretched, n(0.5));
    body.node("clamped", "core/clamp_coverage", vec![("value", level)]);
    let bright = body.multiply("bright", c("clamped", "mask"), i("alpha"));
    let color = body.multiply("color", i("color"), bright);
    body.form(
        "Noise",
        vec![
            ("color", color_input()),
            (
                "speed",
                input(
                    "Speed",
                    "Beats for the light to wander one step",
                    Value::Beats(8.0),
                    Rate::Fixed,
                    &[Time],
                ),
            ),
            (
                "scale",
                input(
                    "Scale",
                    "Blob size, as a share of the rig",
                    Value::Proportion(0.5),
                    Rate::Frame,
                    &[Time],
                ),
            ),
            (
                "contrast",
                input(
                    "Contrast",
                    "Soft wash at 0, hard blobs at 1",
                    Value::Proportion(0.3),
                    Rate::Frame,
                    &[Time, Noise, Audio],
                ),
            ),
            ("alpha", alpha_input(&[Time, Noise, Audio])),
        ],
        color,
    )
}

fn strobe() -> Definition {
    let mut body = Body::default();
    // Only the shutter: the strobe flashes whatever color the layers under
    // it give. Alpha scales the rate, so a gate stops the strobe.
    let rate = body.multiply("gated_rate", i("rate"), i("alpha"));
    body.node("strobe", "write_strobe", vec![("value", rate)]);
    body.form_with(
        "Strobe",
        vec![
            (
                "rate",
                input(
                    "Rate",
                    "Shutter strobe rate",
                    Value::Proportion(0.9),
                    Rate::Frame,
                    &[Time, Noise, Audio],
                ),
            ),
            ("alpha", alpha_input(&[Time, Noise, Audio])),
        ],
        vec![("strobe", c("strobe", "strobe"))],
    )
}

/// The resting aim of `aim@1` and its presets: 40° down toward downstage.
pub const REST: [f64; 3] = [0.0, 0.766, -0.643];

/// A choice of named options: the stored name and its label, in menu order.
fn named_choice(mut input: Input, names: &[(&str, &str)]) -> Input {
    input.author = Some(Author::Choice {
        options: names
            .iter()
            .map(|(name, label)| Preset {
                label: (*label).into(),
                value: Value::Choice((*name).into()),
            })
            .collect(),
        custom: false,
    });
    input
}

/// Where the heads of a clip point: a base (one direction, or a point every
/// head points at), a fan across the axis, and a motion around it. The
/// lighting output is a direction per head, never pan or tilt; its length is
/// alpha. A Replace clip plays it. An Offset clip plays the `turn` output
/// instead: the fan and motion alone, which the compositor applies to the
/// aim under the clip.
fn aim() -> Definition {
    let mut body = Body::default();
    // Shape cycles and hits for a fan per hit count `every`.
    body.node("clock", "core/odometer", vec![("period", i("every"))]);
    body.node(
        "life",
        "core/fraction",
        vec![("value", c("clock", "turns"))],
    );
    body.node("wander", "core/odometer", vec![("period", i("speed"))]);
    body.node(
        "base",
        "core/aim_base",
        vec![
            ("base", i("base")),
            ("direction", i("direction")),
            ("point", i("point")),
        ],
    );
    body.node(
        "fan",
        "core/aim_fan",
        vec![
            ("direction", c("base", "direction")),
            ("fan", i("fan")),
            ("axis", i("axis")),
        ],
    );
    body.node(
        "motion",
        "core/aim_motion",
        vec![
            ("motion", i("motion")),
            ("shape", i("shape")),
            ("cycles", c("clock", "turns")),
            ("spread", i("spread")),
            ("size", i("size")),
            ("wander", c("wander", "turns")),
            ("axis", i("axis")),
        ],
    );
    body.node(
        "moved",
        "core/aim_offset",
        vec![
            ("direction", c("fan", "direction")),
            ("yaw", c("motion", "yaw")),
            ("pitch", c("motion", "pitch")),
            ("axis", i("axis")),
        ],
    );
    body.node(
        "turn",
        "core/aim_turn",
        vec![
            ("fan", i("fan")),
            ("yaw", c("motion", "yaw")),
            ("pitch", c("motion", "pitch")),
            ("alpha", i("alpha")),
            ("axis", i("axis")),
        ],
    );
    let aim = body.multiply("aim", c("moved", "direction"), i("alpha"));
    let degrees = |name: &str, description: &str, promotable: &[SourceKind], low: f64| {
        number(
            input(
                name,
                description,
                Value::Number(0.0),
                Rate::Frame,
                promotable,
            ),
            low,
            90.0,
        )
    };
    let mut definition = body.form_with(
        "Aim",
        vec![
            (
                "base",
                named_choice(
                    input(
                        "Base",
                        "What the heads rest on: one direction, or a point they all point at",
                        Value::Choice("direction".into()),
                        Rate::Fixed,
                        &[],
                    ),
                    &[("direction", "Direction"), ("point", "Point")],
                ),
            ),
            (
                "direction",
                input(
                    "Direction",
                    "The aim in U (stage right), V (downstage), Z (up), when base is direction",
                    Value::Vector(REST),
                    Rate::Frame,
                    &[Time, Noise],
                ),
            ),
            (
                "point",
                input(
                    "Point",
                    "U, V, Z in metres that every head points at, when base is point",
                    Value::Vector([0.0; 3]),
                    Rate::Frame,
                    &[Time],
                ),
            ),
            (
                "fan",
                degrees(
                    "Fan",
                    "Degrees the heads spread apart across the axis; 0 = all alike",
                    &[Time, Hit, Noise, Audio],
                    -90.0,
                ),
            ),
            (
                "axis",
                choice(
                    input(
                        "Axis",
                        "How the heads are laid out, for fan and spread",
                        axis(MappingSource::Order),
                        Rate::Fixed,
                        &[],
                    ),
                    axis_presets(),
                    true,
                ),
            ),
            (
                "motion",
                named_choice(
                    input(
                        "Motion",
                        "How the heads move around the base",
                        Value::Choice("none".into()),
                        Rate::Fixed,
                        &[],
                    ),
                    &[("none", "None"), ("shape", "Shape"), ("noise", "Noise")],
                ),
            ),
            (
                "shape",
                named_choice(
                    input(
                        "Shape",
                        "The wobble, when motion is shape",
                        Value::Choice("swing_left_right".into()),
                        Rate::Fixed,
                        &[],
                    ),
                    &[
                        ("swing_left_right", "Swing left–right"),
                        ("swing_up_down", "Swing up–down"),
                        ("circle", "Circle"),
                        ("figure_8", "Figure-8"),
                    ],
                ),
            ),
            (
                "size",
                degrees("Size", "Degrees of the wobble", &[Time, Audio], 0.0),
            ),
            (
                "every",
                every_input(4.0, "Beats for one wobble cycle, and between hits", &[Time]),
            ),
            (
                "spread",
                number(
                    input(
                        "Spread",
                        "Degrees of phase the wobble travels across the heads; 360 = one cycle, 0 = all together",
                        Value::Number(0.0),
                        Rate::Frame,
                        &[Time],
                    ),
                    -MAX_SPREAD,
                    MAX_SPREAD,
                ),
            ),
            (
                "speed",
                input(
                    "Speed",
                    "Beats for the noise to wander one step",
                    Value::Beats(4.0),
                    Rate::Fixed,
                    &[Time],
                ),
            ),
            (
                "alpha",
                input(
                    "Alpha",
                    "How much the clip counts: in Replace, the blend toward its aim; in Offset, the share of its fan and motion",
                    Value::Proportion(1.0),
                    Rate::Frame,
                    &[Time, Hit, Noise, Audio],
                ),
            ),
        ],
        vec![("aim", aim)],
    );
    definition.outputs.insert(
        crate::aim::TURN_OUTPUT.into(),
        Output {
            value_type: ValueType::Signal(SignalType::new(
                Unit::Number,
                Channels::components(crate::aim::TURN_CHANNELS).expect("nine channels"),
            )),
            rate: Rate::Frame,
        },
    );
    let crate::Body::Graph(graph) = &mut definition.body else {
        unreachable!("a form is a graph")
    };
    graph
        .outputs
        .insert(crate::aim::TURN_OUTPUT.into(), c("turn", "turn"));
    definition
}

// ---------------------------------------------------------------------------
// Clip inputs

/// A form clip carries every input of its form and nothing else. Each value
/// is a plain value of the input's type or a source the input allows.
pub(crate) fn check_inputs(
    id: &str,
    definition: &Definition,
    inputs: &BTreeMap<String, Value>,
) -> Result<()> {
    let missing: Vec<&str> = definition
        .inputs
        .keys()
        .filter(|name| !inputs.contains_key(*name))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(Error(format!("{id}: missing input {}", missing.join(", "))));
    }
    for (name, value) in inputs {
        let spec = definition
            .inputs
            .get(name)
            .ok_or_else(|| Error(format!("{id}: unknown input {name}")))?;
        value
            .validate()
            .map_err(|error| Error(format!("{id}.{name}: {error}")))?;
        check_value(name, spec, value).map_err(|error| Error(format!("{id}.{name}: {error}")))?;
    }
    if id == "aim@1" {
        check_aim(id, inputs)?;
    }
    // A sparkle lights a random share. A fixed 100% is a Wash with a
    // brightness per hit.
    if id == "color.sparkle@1"
        && matches!(inputs.get("coverage"), Some(Value::Proportion(v)) if *v >= 1.0)
    {
        return Err(Error(format!(
            "{id}.coverage: a fixed 100% lights every head; use a Wash (color@1)"
        )));
    }
    check_strokes(id, inputs)
}

/// Hit sources follow the strokes of a moving space source, one channel per
/// live stroke. So a clip has one moving space source at most, and a color
/// cannot follow its strokes.
fn check_strokes(id: &str, inputs: &BTreeMap<String, Value>) -> Result<()> {
    let moving = inputs
        .values()
        .filter(|value| matches!(value, Value::Space(space) if space.movement.is_some()))
        .count();
    if moving > 1 {
        return Err(Error(format!(
            "{id}: a clip has one moving space source at most"
        )));
    }
    if moving == 1 {
        if let Some((name, _)) = inputs
            .iter()
            .find(|(_, value)| matches!(value, Value::Hit(curve) if curve.is_color()))
        {
            return Err(Error(format!(
                "{id}.{name}: a color hit source cannot follow the strokes of a moving space source; use a time source"
            )));
        }
    }
    Ok(())
}

/// The `move` fields of a space source that take a value or a source, as
/// inputs: an editor and the checks read them like form inputs.
pub fn movement_inputs() -> [(&'static str, Input); 2] {
    [
        (
            "travel",
            input(
                "Travel",
                "Beats for one stroke to cross the whole axis; 0 is the whole clip",
                Value::Beats(2.0),
                Rate::Fixed,
                &[Time],
            ),
        ),
        (
            "width",
            number(
                input(
                    "Width",
                    "Stroke size: a share of the axis, or of the gap between strokes. \
                     Above 1 a stroke is wider than the axis",
                    Value::Number(0.2),
                    Rate::Frame,
                    &[Time, Hit],
                ),
                0.0,
                MAX_WIDTH,
            ),
        ),
    ]
}

fn check_movement(movement: &Movement) -> Result<()> {
    for (name, spec) in movement_inputs() {
        let value = match name {
            "travel" => &movement.travel,
            _ => &movement.width,
        };
        check_value(name, &spec, value).map_err(|error| Error(format!("move.{name}: {error}")))?;
    }
    Ok(())
}

/// A plain direction has a direction, a shape or a fan per hit needs
/// hits (`every` above 0), and spread is degrees, not a share of a cycle.
fn check_aim(id: &str, inputs: &BTreeMap<String, Value>) -> Result<()> {
    if let Some(Value::Proportion(share)) = inputs.get("spread") {
        return Err(Error(format!(
            "{id}.spread: spread is now degrees of phase (360° = one cycle across the axis), \
             not a share of a cycle; store {{\"type\": \"number\", \"value\": {}}}",
            share * 360.0
        )));
    }
    if let Some(Value::Vector(direction)) = inputs.get("direction") {
        if direction.iter().all(|v| v.abs() < 1e-9) {
            return Err(Error(format!(
                "{id}.direction: a direction must not be zero"
            )));
        }
    }
    let shape = matches!(inputs.get("motion"), Some(Value::Choice(name)) if name == "shape");
    let hits = matches!(inputs.get("fan"), Some(Value::Hit(_)));
    if (shape || hits) && matches!(inputs.get("every"), Some(Value::Beats(v)) if *v <= 0.0) {
        return Err(Error(format!(
            "{id}.every: a shape or a fan per hit needs every above 0 beats"
        )));
    }
    Ok(())
}

fn check_value(name: &str, spec: &Input, value: &Value) -> Result<()> {
    // A number field's bounds; any other plain input is a share, 0 to 1.
    let (low, high) = match spec.author {
        Some(Author::Number { min, max }) => (
            min.unwrap_or(f64::NEG_INFINITY),
            max.unwrap_or(f64::INFINITY),
        ),
        _ => (0.0, 1.0),
    };
    let Some(kind) = value.source_kind() else {
        if let (Value::Number(v) | Value::Proportion(v), Some(Author::Number { .. })) =
            (value, &spec.author)
        {
            if !(low..=high).contains(v) {
                return Err(Error(format!("{v} is outside {low} to {high}")));
            }
        }
        if !spec.value_type.accepts(value.value_type())
            || (is_vector(spec) && !matches!(value, Value::Vector(_)))
        {
            return Err(Error(format!(
                "expected {}, got {:?}",
                spec.value_type,
                value.value_type()
            )));
        }
        if let (
            Value::Choice(name),
            Some(Author::Choice {
                options,
                custom: false,
            }),
        ) = (value, &spec.author)
        {
            if !options.iter().any(|option| option.value == *value) {
                return Err(Error(format!("{name} is not one of the options")));
            }
        }
        if let Value::Mapping(mapping) = value {
            if mapping.reverse {
                return Err(Error(
                    "an axis has no reverse; choose a backward path".into(),
                ));
            }
            if mapping.per_group {
                return Err(Error(
                    "an axis has no per_group; choose the group span".into(),
                ));
            }
        }
        return Ok(());
    };
    if !spec.promotable.contains(&kind) {
        return Err(Error(format!(
            "this input does not accept a {kind:?} source"
        )));
    }
    let target = spec.value_type.signal_type().unwrap_or(SignalType::ANY);
    let color = target.channels == Some(Channels::Rgb);
    // A vector has no range: a direction or a point in metres.
    let vector = is_vector(spec);
    let speed = SPEEDS.contains(&name);
    let within = |mut values: Box<dyn Iterator<Item = f64> + '_>| {
        values.all(|v| {
            if speed {
                v > 0.0
            } else {
                vector || (low..=high).contains(&v)
            }
        })
    };
    let fits = match value {
        Value::Time(SourceCurve::Keys(curve)) | Value::Hit(SourceCurve::Keys(curve)) => {
            curve.is_color() == (color || vector) && within(Box::new(curve.values()))
        }
        Value::Time(SourceCurve::Gradient(_)) | Value::Hit(SourceCurve::Gradient(_)) => color,
        // A color reads a still gradient; a number reads a curve, still or
        // moving.
        Value::Space(space) => {
            if color {
                space.gradient.is_some() && space.movement.is_none()
            } else {
                let fits = !vector
                    && !speed
                    && space.curve.as_ref().is_some_and(|curve| {
                        within(Box::new(curve.points.iter().map(|point| point.value)))
                    });
                if let (true, Some(movement)) = (fits, &space.movement) {
                    check_movement(movement)?;
                }
                fits
            }
        }
        Value::Noise(NoiseSource { range, .. }) => {
            !color && !speed && within(Box::new(range.iter().copied()))
        }
        Value::Audio(_) => !color && !vector && !speed,
        _ => false,
    };
    if fits {
        Ok(())
    } else {
        Err(Error(format!(
            "the {kind:?} source does not fit a {} input",
            spec.value_type
        )))
    }
}

/// A U, V, Z input: an aim direction or a point.
fn is_vector(spec: &Input) -> bool {
    spec.value_type.signal_type().and_then(|t| t.channels) == Some(crate::tensor::VECTOR)
}

/// The form with every source turned into graph nodes, and the plain values
/// that remain. `None` when the clip is not a form clip or has no sources.
pub(crate) fn lower(
    library: &Library,
    id: &str,
    inputs: &BTreeMap<String, Value>,
) -> Result<Option<(Definition, BTreeMap<String, Value>)>> {
    if !is_form(id) {
        return Ok(None);
    }
    let definition = library
        .definitions
        .get(id)
        .ok_or_else(|| Error(format!("unknown definition {id}")))?;
    check_inputs(id, definition, inputs)?;
    if inputs.values().all(|value| value.source_kind().is_none()) {
        return Ok(None);
    }
    let mut lowered = definition.clone();
    let crate::Body::Graph(graph) = &mut lowered.body else {
        unreachable!("forms are graphs")
    };
    let mut plain = BTreeMap::new();
    let mut hits = hit_progress(graph);
    // Space sources first: a moving one adds the strokes that hit sources
    // then follow, and its clock reads `every` before a speed curve on
    // `every` rewires the clocks.
    let mut order: Vec<(&String, &Value)> = inputs.iter().collect();
    order.sort_by_key(|(_, value)| !matches!(value, Value::Space(_)));
    for (name, value) in order {
        if value.source_kind().is_none() {
            plain.insert(name.clone(), value.clone());
            continue;
        }
        let spec = lowered.inputs.remove(name).expect("checked input");
        let mut body = Body::default();
        let key = |part: &str| format!("source/{name}/{part}");
        let replacement = match value {
            Value::Time(curve) if SPEEDS.contains(&name.as_str()) => {
                let curve = Value::Time(curve.clone());
                clocks(graph, name, &curve);
                time_curve(&mut body, &key, curve)
            }
            Value::Space(space) => match &space.movement {
                None => still_space(&mut body, &key, space),
                Some(movement) => {
                    let (lit, life) = stroke(&mut body, &key, space, movement);
                    hits = Some(life);
                    lit
                }
            },
            Value::Time(SourceCurve::Gradient(read)) => {
                body.node(&key("clock"), "clip_time", vec![]);
                gradient_curve(&mut body, &key, read, c(&key("clock"), "progress"))
            }
            Value::Time(curve) => time_curve(&mut body, &key, Value::Time(curve.clone())),
            Value::Hit(curve) => {
                let progress = hits
                    .clone()
                    .ok_or_else(|| Error(format!("{id} has no events for a hit source")))?;
                match curve {
                    SourceCurve::Gradient(read) => gradient_curve(&mut body, &key, read, progress),
                    SourceCurve::Keys(_) => {
                        body.node(
                            &key("curve"),
                            "core/curve",
                            vec![
                                ("curve", Value::Time(curve.clone()).into()),
                                ("progress", progress),
                            ],
                        );
                        c(&key("curve"), "value")
                    }
                }
            }
            Value::Noise(noise) => {
                body.node(
                    &key("clock"),
                    "core/odometer",
                    vec![("period", Value::Beats(noise.speed).into())],
                );
                // A vector wanders on each of U, V and Z, apart.
                let parts = if is_vector(&spec) { 3 } else { 1 };
                let mut joined: Option<Binding> = None;
                for part in 0..parts {
                    let key = |step: &str| format!("source/{name}/{step}{part}");
                    body.node(
                        &key("noise"),
                        "core/noise",
                        vec![
                            ("x", c(&format!("source/{name}/clock"), "turns")),
                            ("y", n(salt(name) + 101.0 * part as f64)),
                            ("z", n(0.0)),
                        ],
                    );
                    let wander = scale(&mut body, &key, c(&key("noise"), "value"), noise.range);
                    joined = Some(match joined {
                        None => wander,
                        Some(before) => {
                            body.node(
                                &key("join"),
                                "core/join_channels",
                                vec![("a", before), ("b", wander)],
                            );
                            c(&key("join"), "value")
                        }
                    });
                }
                joined.expect("one part at least")
            }
            Value::Audio(audio) => {
                let (low, high) = (audio.from_hz, audio.to_hz);
                body.node(
                    &key("energy"),
                    "band_energy",
                    vec![("low_hz", n(low)), ("high_hz", n(high))],
                );
                body.node(
                    &key("level"),
                    "normalize",
                    vec![("value", c(&key("energy"), "value"))],
                );
                // Quiet gives the floor; loud gives 1.
                let level = scale(
                    &mut body,
                    &key,
                    c(&key("level"), "value"),
                    [audio.floor, 1.0],
                );
                // Below the threshold the level is 0, under the floor too.
                let level = if audio.threshold > 0.0 {
                    body.node(
                        &key("gate"),
                        "core/greater",
                        vec![
                            ("a", c(&key("level"), "value")),
                            ("b", n(audio.threshold - 1e-9)),
                            ("tolerance", n(0.0)),
                        ],
                    );
                    body.multiply(&key("gated"), level, c(&key("gate"), "mask"))
                } else {
                    level
                };
                // A number input with a top (fan and size, in degrees) reads
                // 0–1 as 0 to that top.
                match spec.author {
                    Some(Author::Number { max: Some(max), .. }) => {
                        body.multiply(&key("range"), level, n(max))
                    }
                    _ => level,
                }
            }
            _ => unreachable!("checked source"),
        };
        graph.nodes.extend(body.0);
        for binding in graph
            .nodes
            .values_mut()
            .flat_map(|node| node.inputs.values_mut())
            .chain(graph.outputs.values_mut())
        {
            if matches!(binding, Binding::Input { input } if input == name) {
                *binding = replacement.clone();
            }
        }
    }
    Ok(Some((lowered, plain)))
}

/// A gradient read at the positions a curve gives over `progress`.
fn gradient_curve(
    body: &mut Body,
    key: &impl Fn(&str) -> String,
    read: &GradientCurve,
    progress: Binding,
) -> Binding {
    let position = body.envelope(
        &key("position"),
        progress,
        Value::Envelope(read.curve.clone()).into(),
    );
    body.node(
        &key("sample"),
        "sample_gradient",
        vec![
            ("gradient", Value::Gradient(read.gradient.clone()).into()),
            ("position", position),
        ],
    );
    c(&key("sample"), "color")
}

/// Where a hit source reads the life of its event: the `life` node's
/// progress. Sparkle has an event life there; `color@1` has the fraction of
/// its hit clock, unless a moving space source gives its strokes' life.
fn hit_progress(graph: &Graph) -> Option<Binding> {
    match graph.nodes.get("life")?.definition.as_str() {
        "core/event_life" => Some(c("life", "progress")),
        "core/fraction" => Some(c("life", "value")),
        _ => None,
    }
}

/// Give a speed curve to the clocks that read the input, on their `_curve`
/// port in place of the input.
fn clocks(graph: &mut Graph, name: &str, curve: &Value) {
    for node in graph.nodes.values_mut() {
        if !matches!(
            node.definition.as_str(),
            "core/event_life" | "core/odometer"
        ) {
            continue;
        }
        let keys: Vec<_> = node
            .inputs
            .iter()
            .filter(|(_, binding)| matches!(binding, Binding::Input { input } if input == name))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            node.inputs.remove(&key);
            node.inputs
                .insert(format!("{key}_curve"), curve.clone().into());
        }
    }
}

fn time_curve(body: &mut Body, key: &impl Fn(&str) -> String, curve: Value) -> Binding {
    body.node(&key("clock"), "clip_time", vec![]);
    body.node(
        &key("curve"),
        "core/curve",
        vec![
            ("curve", curve.into()),
            ("progress", c(&key("clock"), "progress")),
        ],
    );
    c(&key("curve"), "value")
}

/// Map 0–1 onto `range`.
fn scale(
    body: &mut Body,
    key: &impl Fn(&str) -> String,
    level: Binding,
    range: [f64; 2],
) -> Binding {
    let scaled = body.multiply(&key("scaled"), level, n(range[1] - range[0]));
    body.add(&key("shifted"), scaled, n(range[0]))
}

/// A fixed offset per input name, so two noise sources wander apart.
fn salt(name: &str) -> f64 {
    let hash = name.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    (hash % 10_000) as f64 + 0.5
}
