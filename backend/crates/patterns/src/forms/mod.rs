//! Clip forms: shipped graphs that a clip names together with a value for
//! every input. A form input takes a plain value of its type or, where the
//! input's `promotable` list allows it, a source. Sources become graph nodes
//! when the clip is prepared; the stored clip keeps only the values.
use crate::*;
use std::collections::BTreeMap;

pub(crate) mod ops;
mod pace;

/// Every form id. An id never changes meaning; a new meaning is a new version.
pub const FORMS: [&str; 7] = [
    "color.constant@1",
    "color.time@1",
    "color.space@1",
    "color.chase@1",
    "color.sparkle@1",
    "color.noise@1",
    "strobe.constant@1",
];

pub fn is_form(id: &str) -> bool {
    FORMS.contains(&id)
}

/// A form's inputs in the order an editor shows them. The definition keeps
/// its inputs in a map, so the order lives here.
pub fn input_order(id: &str) -> Option<&'static [&'static str]> {
    Some(match id {
        "color.constant@1" => &["color", "alpha"],
        "color.time@1" => &["colors", "curve", "every", "alpha"],
        "color.space@1" => &["colors", "axis", "alpha"],
        "color.chase@1" => &[
            "color",
            "axis",
            "every",
            "travel",
            "width",
            "width_relative",
            "shape",
            "path",
            "alpha",
            "boundary",
        ],
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
        _ => return None,
    })
}

/// Inputs that set a speed. A `time` source on one of them is summed over
/// the clip like an odometer instead of read frame by frame.
const SPEEDS: [&str; 4] = ["every", "travel", "duration", "speed"];

use SourceKind::{Audio, Events as Stamps, Hit, Noise, Time};

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
                position: None,
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
                input_nodes: BTreeMap::new(),
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

fn curve(points: &[[f64; 2]], curves: &[EnvelopeCurve]) -> Value {
    Value::Envelope(Envelope {
        points: points.to_vec(),
        curves: curves.to_vec(),
    })
}
fn bezier(control1: [f64; 2], control2: [f64; 2]) -> EnvelopeCurve {
    EnvelopeCurve::Bezier { control1, control2 }
}

/// Positions at the centers of N equal parts, without gliding.
pub fn steps_path(count: usize) -> Envelope {
    let count = count.max(1);
    let mut points: Vec<[f64; 2]> = (0..count)
        .map(|i| [i as f64 / count as f64, (i as f64 + 0.5) / count as f64])
        .collect();
    points.push([1.0, (count as f64 - 0.5) / count as f64]);
    Envelope {
        curves: vec![EnvelopeCurve::Hold; points.len() - 1],
        points,
    }
}

/// Named paths: where a stroke is over its life (0 = axis start, 1 = end).
pub fn path_presets() -> Vec<(&'static str, Value)> {
    vec![
        ("Forward", curve(&[[0., 0.], [1., 1.]], &[])),
        ("Backward", curve(&[[0., 1.], [1., 0.]], &[])),
        ("Bounce", curve(&[[0., 0.], [0.5, 1.], [1., 0.]], &[])),
        (
            "Ease in",
            curve(&[[0., 0.], [1., 1.]], &[bezier([0.42, 0.], [1., 1.])]),
        ),
        (
            "Ease out",
            curve(&[[0., 0.], [1., 1.]], &[bezier([0., 0.], [0.58, 1.])]),
        ),
        (
            "Ease in-out",
            curve(&[[0., 0.], [1., 1.]], &[bezier([0.42, 0.], [0.58, 1.])]),
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
                &[bezier([0.2, 0.], [0.3, 1.]), bezier([0.7, 1.], [0.8, 0.])],
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
                &[bezier([0.4, 0.], [0.5, 0.3]), bezier([0.5, 0.3], [0.6, 0.])],
            ),
        ),
    ]
}

/// Named curves for `color.time`: how the gradient is crossed over a pass.
fn progress_presets() -> Vec<(&'static str, Value)> {
    vec![
        ("Linear", curve(&[[0., 0.], [1., 1.]], &[])),
        (
            "Ease in-out",
            curve(&[[0., 0.], [1., 1.]], &[bezier([0.42, 0.], [0.58, 1.])]),
        ),
        (
            "There and back",
            curve(&[[0., 0.], [0.5, 1.], [1., 0.]], &[]),
        ),
    ]
}

fn axis(source: MappingSource) -> Value {
    Value::Mapping(MappingSpec {
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
    ]
}
fn axis_input(default: MappingSource) -> Input {
    choice(
        input(
            "Axis",
            "Which way the heads are ordered",
            axis(default),
            Rate::Fixed,
            &[],
        ),
        axis_presets(),
        true,
    )
}

fn gradient(stops: &[(f64, [f64; 3])]) -> Value {
    Value::Gradient(Gradient {
        stops: stops
            .iter()
            .map(|(t, color)| ColorStop {
                t: *t,
                color: *color,
                alpha: 1.0,
            })
            .collect(),
    })
}
fn colors_input() -> Input {
    input(
        "Colors",
        "The gradient",
        gradient(&[(0.0, [1.0, 0.0, 0.0]), (1.0, [0.0, 0.0, 1.0])]),
        Rate::Fixed,
        &[],
    )
}

// ---------------------------------------------------------------------------
// The forms

pub(crate) fn definitions() -> Vec<(&'static str, Definition)> {
    vec![
        ("color.constant@1", constant()),
        ("color.time@1", time()),
        ("color.space@1", space()),
        ("color.chase@1", chase()),
        ("color.sparkle@1", sparkle()),
        ("color.noise@1", noise()),
        ("strobe.constant@1", strobe()),
    ]
}

fn constant() -> Definition {
    let mut body = Body::default();
    let color = body.multiply("color", i("color"), i("alpha"));
    body.form(
        "Constant color",
        vec![
            ("color", color_input()),
            ("alpha", alpha_input(&[Time, Noise, Audio])),
        ],
        color,
    )
}

fn time() -> Definition {
    let mut body = Body::default();
    body.node("clock", "core/odometer", vec![("period", i("every"))]);
    body.node(
        "phase",
        "core/fraction",
        vec![("value", c("clock", "turns"))],
    );
    let shaped = body.envelope("shaped", c("phase", "value"), i("curve"));
    body.node(
        "sample",
        "sample_gradient",
        vec![("gradient", i("colors")), ("position", shaped)],
    );
    let color = body.multiply("color", c("sample", "color"), i("alpha"));
    body.form(
        "Color over time",
        vec![
            ("colors", colors_input()),
            (
                "curve",
                choice(
                    input(
                        "Curve",
                        "How one pass moves through the gradient",
                        progress_presets().remove(0).1,
                        Rate::Fixed,
                        &[],
                    ),
                    progress_presets(),
                    true,
                ),
            ),
            (
                "every",
                every_input(
                    0.0,
                    "Beats for one pass; 0 plays the gradient once over the clip",
                    &[Time],
                ),
            ),
            ("alpha", alpha_input(&[Time, Noise, Audio])),
        ],
        color,
    )
}

fn space() -> Definition {
    let mut body = Body::default();
    body.node("position", "mapped_position", vec![("mapping", i("axis"))]);
    body.node(
        "sample",
        "sample_gradient",
        vec![
            ("gradient", i("colors")),
            ("position", c("position", "value")),
        ],
    );
    let color = body.multiply("color", c("sample", "color"), i("alpha"));
    body.form(
        "Color across space",
        vec![
            ("colors", colors_input()),
            ("axis", axis_input(MappingSource::U)),
            ("alpha", alpha_input(&[Time, Noise, Audio])),
        ],
        color,
    )
}

fn chase() -> Definition {
    let mut body = Body::default();
    body.node(
        "life",
        "core/event_life",
        vec![("every", i("every")), ("life", i("travel"))],
    );
    let progress = c("life", "progress");
    let position = body.envelope("position", progress.clone(), i("path"));
    // Which way the stroke travels now. A still path (a step) keeps the
    // direction of the whole path.
    let ahead_at = body.add("ahead_at", progress.clone(), n(1e-3));
    let behind_at = body.subtract("behind_at", progress, n(1e-3));
    let ahead = body.envelope("ahead", ahead_at, i("path"));
    let behind = body.envelope("behind", behind_at, i("path"));
    let velocity = body.subtract("velocity", ahead, behind);
    let path_end = body.envelope("path_end", Value::Proportion(1.0).into(), i("path"));
    let path_start = body.envelope("path_start", Value::Proportion(0.0).into(), i("path"));
    let overall = body.subtract("overall", path_end, path_start);
    let moving_back = body.greater("moving_back", n(0.0), velocity.clone());
    let moving_on = body.greater("moving_on", velocity, n(0.0));
    let net_back = body.greater("net_back", n(0.0), overall);
    let moving = body.add("moving", moving_back.clone(), moving_on);
    let still = body.subtract("still", n(1.0), moving);
    let still_back = body.multiply("still_back", still, net_back);
    let backward = body.add("backward", moving_back, still_back);
    // Width: a share of the axis, or of the gap to the next stroke.
    body.node(
        "relative",
        "core/choose_number",
        vec![
            ("condition", i("width_relative")),
            ("yes", n(1.0)),
            ("no", n(0.0)),
        ],
    );
    let absolute = body.subtract("absolute", n(1.0), c("relative", "value"));
    let gap = body.multiply("gap_width", i("width"), c("life", "spacing"));
    let gap_part = body.multiply("gap_part", gap, c("relative", "value"));
    let axis_part = body.multiply("axis_part", i("width"), absolute);
    let width = body.add("width", gap_part, axis_part);
    body.node(
        "safe_width",
        "core/maximum",
        vec![("a", width.clone()), ("b", n(1e-9))],
    );
    // Phase across the stroke: 0 at the tail, 1 at the head.
    body.node("axis", "resolve_mapping", vec![("mapping", i("axis"))]);
    body.node(
        "offset",
        "coordinate_offset",
        vec![
            ("mapping", c("axis", "coordinates")),
            ("position", position),
            ("boundary", i("boundary")),
        ],
    );
    body.node(
        "across",
        "core/divide",
        vec![("a", c("offset", "value")), ("b", c("safe_width", "value"))],
    );
    let phase = body.add("phase", c("across", "value"), n(0.5));
    let mirrored = body.subtract("mirrored", n(1.0), phase.clone());
    let flip = body.subtract("flip", mirrored, phase.clone());
    let flip_back = body.multiply("flip_back", flip, backward);
    let directed = body.add("directed", phase.clone(), flip_back);
    // The stroke covers its tail and head edges, so strokes that fill the
    // axis leave no head dark at a boundary.
    let after_tail = body.greater("after_tail", phase.clone(), n(-2e-9));
    let before_head = body.greater("before_head", n(1.0 + 2e-9), phase);
    let visible = body.greater("visible", width, n(0.0));
    let stroke = body.envelope("stroke", directed, i("shape"));
    let lit = body.multiply("lit", stroke, after_tail);
    let lit = body.multiply("lit_head", lit, before_head);
    let lit = body.multiply("lit_visible", lit, visible);
    let lit = body.multiply("lit_live", lit, c("life", "present"));
    let lit = body.multiply("lit_alpha", lit, i("alpha"));
    body.node("mask", "core/channel_maximum", vec![("value", lit)]);
    let color = body.multiply("color", i("color"), c("mask", "value"));
    body.form(
        "Chase",
        vec![
            ("color", color_input()),
            ("axis", axis_input(MappingSource::Order)),
            (
                "every",
                every_input(2.0, "Beats between strokes", &[Time, Stamps]),
            ),
            (
                "travel",
                input(
                    "Travel",
                    "Beats for one stroke to cross the whole axis",
                    Value::Beats(2.0),
                    Rate::Fixed,
                    &[Time],
                ),
            ),
            (
                "width",
                input(
                    "Width",
                    "Stroke size",
                    Value::Proportion(0.5),
                    Rate::Frame,
                    &[Time, Hit],
                ),
            ),
            (
                "width_relative",
                input(
                    "Relative width",
                    "Width is a share of the gap between strokes; otherwise of the axis",
                    Value::Boolean(true),
                    Rate::Fixed,
                    &[],
                ),
            ),
            (
                "shape",
                choice(
                    input(
                        "Shape",
                        "Brightness across the stroke",
                        shape_presets().remove(0).1,
                        Rate::Fixed,
                        &[],
                    ),
                    shape_presets(),
                    true,
                ),
            ),
            (
                "path",
                choice(
                    input(
                        "Path",
                        "Where the stroke is over its life",
                        path_presets().remove(0).1,
                        Rate::Fixed,
                        &[],
                    ),
                    path_presets(),
                    true,
                ),
            ),
            ("alpha", alpha_input(&[Time, Hit, Noise, Audio])),
            (
                "boundary",
                choice(
                    input(
                        "Boundary",
                        "What happens at the ends of the axis",
                        Value::Boundary(Boundary::Clip),
                        Rate::Fixed,
                        &[],
                    ),
                    vec![
                        ("Clip", Value::Boundary(Boundary::Clip)),
                        ("Wrap", Value::Boundary(Boundary::Wrap)),
                    ],
                    false,
                ),
            ),
        ],
        color,
    )
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
            (
                "every",
                every_input(1.0, "Beats between events", &[Time, Stamps]),
            ),
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
    body.node("strobe", "write_strobe", vec![("value", i("rate"))]);
    let color = body.multiply("color", Value::Color([1.0; 3]).into(), i("alpha"));
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
        vec![("color", color), ("strobe", c("strobe", "strobe"))],
    )
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
    for name in definition.inputs.keys() {
        if !inputs.contains_key(name) {
            return Err(Error(format!("{id}: missing input {name}")));
        }
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
    Ok(())
}

fn check_value(name: &str, spec: &Input, value: &Value) -> Result<()> {
    let Some(kind) = value.source_kind() else {
        if !spec.value_type.accepts(value.value_type()) {
            return Err(Error(format!(
                "expected {}, got {:?}",
                spec.value_type,
                value.value_type()
            )));
        }
        if let Value::Mapping(mapping) = value {
            if mapping.reverse {
                return Err(Error(
                    "an axis has no reverse; choose a backward path".into(),
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
    let speed = SPEEDS.contains(&name);
    let within = |mut values: Box<dyn Iterator<Item = f64> + '_>| {
        values.all(|v| {
            if speed {
                v > 0.0
            } else {
                (0.0..=1.0).contains(&v)
            }
        })
    };
    let fits = match value {
        Value::Time(curve) | Value::Hit(curve) => {
            curve.is_color() == color && within(Box::new(curve.values()))
        }
        Value::Noise(NoiseSource { range, .. }) | Value::Audio(AudioLevel { range, .. }) => {
            !color && !speed && within(Box::new(range.iter().copied()))
        }
        Value::Events(Events::Beats { times }) => times.as_slice().iter().all(|t| *t >= 0.0),
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
    for (name, value) in inputs {
        if value.source_kind().is_none() {
            plain.insert(name.clone(), value.clone());
            continue;
        }
        lowered.inputs.remove(name);
        let mut body = Body::default();
        let key = |part: &str| format!("source/{name}/{part}");
        let replacement = match value {
            Value::Events(_) => {
                clocks(graph, name, |_| "stamps".into(), value)?;
                None
            }
            Value::Time(curve) if SPEEDS.contains(&name.as_str()) => {
                let curve = Value::Time(curve.clone());
                clocks(graph, name, |key| format!("{key}_curve"), &curve)?;
                Some(time_curve(&mut body, &key, curve))
            }
            Value::Time(curve) => Some(time_curve(&mut body, &key, Value::Time(curve.clone()))),
            Value::Hit(curve) => {
                if !graph.nodes.contains_key("life") {
                    return Err(Error(format!("{id} has no events for a hit source")));
                }
                body.node(
                    &key("curve"),
                    "core/curve",
                    vec![
                        ("curve", Value::Time(curve.clone()).into()),
                        ("progress", c("life", "progress")),
                    ],
                );
                Some(c(&key("curve"), "value"))
            }
            Value::Noise(noise) => {
                body.node(
                    &key("clock"),
                    "core/odometer",
                    vec![("period", Value::Beats(noise.speed).into())],
                );
                body.node(
                    &key("noise"),
                    "core/noise",
                    vec![
                        ("x", c(&key("clock"), "turns")),
                        ("y", n(salt(name))),
                        ("z", n(0.0)),
                    ],
                );
                Some(scale(
                    &mut body,
                    &key,
                    c(&key("noise"), "value"),
                    noise.range,
                ))
            }
            Value::Audio(audio) => {
                let (low, high) = audio.band.hz();
                body.node(
                    &key("energy"),
                    "band_energy",
                    vec![
                        ("source", Value::AudioSource(AudioSource::Mix.into()).into()),
                        ("low_hz", n(low)),
                        ("high_hz", n(high)),
                    ],
                );
                body.node(
                    &key("level"),
                    "normalize",
                    vec![("value", c(&key("energy"), "value"))],
                );
                Some(scale(
                    &mut body,
                    &key,
                    c(&key("level"), "value"),
                    audio.range,
                ))
            }
            _ => unreachable!("checked source"),
        };
        graph.nodes.extend(body.0);
        let mut unbound = false;
        for binding in graph
            .nodes
            .values_mut()
            .flat_map(|node| node.inputs.values_mut())
            .chain(graph.outputs.values_mut())
        {
            if matches!(binding, Binding::Input { input } if input == name) {
                match &replacement {
                    Some(replacement) => *binding = replacement.clone(),
                    None => unbound = true,
                }
            }
        }
        if unbound {
            return Err(Error(format!(
                "{id}.{name}: this source only drives the event clock"
            )));
        }
    }
    Ok(Some((lowered, plain)))
}

/// Give a speed source to the clocks that read the input, in place of it.
fn clocks(
    graph: &mut Graph,
    name: &str,
    rename: impl Fn(&str) -> String,
    value: &Value,
) -> Result<()> {
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
            node.inputs.insert(rename(&key), value.clone().into());
        }
    }
    Ok(())
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
