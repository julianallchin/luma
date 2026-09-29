//! Forms describe intent and lower sources into the shared tensor graph.
use crate::*;
use std::collections::BTreeMap;
pub(crate) mod clock_table;
mod geometry;
mod lower;
mod noise;
pub(crate) mod ops;
mod pace;
pub(crate) mod source_ops;
mod validate;
pub(crate) use lower::lower;
pub use noise::{noise_value, NoiseSettings};
pub use source_ops::SourceOp;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormKind {
    Color,
    Aim,
    Strobe,
}
pub const FORMS: [&str; 3] = ["color@1", "strobe.constant@1", "aim@1"];
pub const MAX_WIDTH: f64 = 4.;
pub fn is_form(id: &str) -> bool {
    FORMS.contains(&id)
}
pub fn blend_modes(id: &str) -> &'static [BlendMode] {
    if id == "aim@1" {
        &[BlendMode::Replace, BlendMode::Offset]
    } else {
        &BlendMode::LIGHT
    }
}
pub fn input_order(id: &str) -> Option<&'static [&'static str]> {
    Some(match id {
        "color@1" => &["color", "brightness", "fade"],
        "aim@1" => &[
            "base",
            "direction",
            "point",
            "lean",
            "horizontal",
            "vertical",
            "axis",
            "fade",
        ],
        "strobe.constant@1" => &["rate", "fade"],
        _ => return None,
    })
}
const SOURCES: &[SourceKind] = &[
    SourceKind::Time,
    SourceKind::Space,
    SourceKind::Random,
    SourceKind::Noise,
    SourceKind::Audio,
];
fn input(name: &str, description: &str, value: Value, sources: &[SourceKind]) -> Input {
    Input {
        name: name.into(),
        description: description.into(),
        value_type: value
            .value_type()
            .signal_type()
            .map_or(value.value_type(), ValueType::Signal),
        default: Some(value),
        optional: false,
        rate: Rate::Frame,
        author: None,
        promotable: sources.to_vec(),
    }
}
fn number(name: &str, value: f64, low: f64, high: f64) -> Input {
    let mut spec = input(name, name, Value::Number(value), SOURCES);
    spec.author = Some(Author::Number {
        min: Some(low),
        max: Some(high),
    });
    spec
}
fn fade() -> Input {
    input(
        "Clip fade",
        "Fade the whole clip over its duration",
        Value::Proportion(1.),
        &[SourceKind::Time],
    )
}
fn definition(kind: FormKind, name: &str, inputs: Vec<(&str, Input)>) -> Definition {
    let mut outputs = BTreeMap::from([(
        "lighting".into(),
        Output {
            value_type: ValueType::Lighting,
            rate: Rate::Frame,
        },
    )]);
    if kind == FormKind::Aim {
        outputs.insert(
            crate::aim::TURN_OUTPUT.into(),
            Output {
                value_type: ValueType::Signal(SignalType::new(
                    Unit::Number,
                    Channels::components(crate::aim::TURN_CHANNELS).expect("turn channels"),
                )),
                rate: Rate::Frame,
            },
        );
    }
    Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs,
        body: crate::Body::Form(kind),
    }
}
pub(crate) fn definitions() -> Vec<(&'static str, Definition)> {
    let mut base = input(
        "Position",
        "A direction or a target point",
        Value::Choice("direction".into()),
        &[],
    );
    base.author = Some(Author::Choice {
        options: vec![
            Preset {
                label: "Direction".into(),
                value: Value::Choice("direction".into()),
            },
            Preset {
                label: "Point".into(),
                value: Value::Choice("point".into()),
            },
        ],
        custom: false,
    });
    let mut frame = input(
        "Axis",
        "The spatial frame and mirror of the offsets",
        axis(MappingSource::Order),
        &[],
    );
    frame.author = Some(Author::Choice {
        options: axis_presets()
            .into_iter()
            .map(|(label, value)| Preset {
                label: label.into(),
                value,
            })
            .collect(),
        custom: true,
    });
    vec![
        (
            "color@1",
            definition(
                FormKind::Color,
                "Color",
                vec![
                    (
                        "color",
                        input(
                            "Color",
                            "Color of the light",
                            Value::Color([1.; 3]),
                            &[SourceKind::Time, SourceKind::Space],
                        ),
                    ),
                    (
                        "brightness",
                        input(
                            "Brightness",
                            "Brightness of the light",
                            Value::Proportion(1.),
                            SOURCES,
                        ),
                    ),
                    ("fade", fade()),
                ],
            ),
        ),
        (
            "aim@1",
            definition(
                FormKind::Aim,
                "Aim",
                vec![
                    ("base", base),
                    (
                        "direction",
                        input(
                            "Direction",
                            "Stage right, downstage, up",
                            Value::Vector([0., 0.766, -0.643]),
                            SOURCES,
                        ),
                    ),
                    (
                        "point",
                        input(
                            "Point",
                            "Target in stage metres",
                            Value::Vector([0.; 3]),
                            SOURCES,
                        ),
                    ),
                    (
                        "lean",
                        input(
                            "Spatial offset",
                            "A lean in stage coordinates; Space spreads the heads along its axis",
                            Value::Vector([0.; 3]),
                            SOURCES,
                        ),
                    ),
                    ("horizontal", number("Horizontal offset", 0., -180., 180.)),
                    ("vertical", number("Vertical offset", 0., -180., 180.)),
                    ("axis", frame),
                    ("fade", fade()),
                ],
            ),
        ),
        (
            "strobe.constant@1",
            definition(
                FormKind::Strobe,
                "Strobe",
                vec![
                    (
                        "rate",
                        input(
                            "Rate",
                            "Shutter strobe rate",
                            Value::Proportion(0.5),
                            SOURCES,
                        ),
                    ),
                    ("fade", fade()),
                ],
            ),
        ),
    ]
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

pub(crate) fn check_inputs(
    id: &str,
    definition: &Definition,
    inputs: &BTreeMap<String, Value>,
) -> Result<()> {
    for name in inputs.keys() {
        if !definition.inputs.contains_key(name) {
            return Err(Error(format!("{id}: unknown input {name}")));
        }
    }
    let missing: Vec<_> = definition
        .inputs
        .keys()
        .filter(|name| !inputs.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        return Err(Error(format!(
            "{id}: missing inputs {}",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    for (name, spec) in &definition.inputs {
        let value = inputs
            .get(name)
            .ok_or_else(|| Error(format!("{id}: missing input {name}")))?;
        validate::validate(value, inputs, name, 0)
            .map_err(|e| Error(format!("{id}.{name}: {e}")))?;
        if let Some(kind) = value.source_kind() {
            if !spec.promotable.contains(&kind) {
                return Err(Error(format!(
                    "{id}.{name}: this input does not accept {kind:?}"
                )));
            }
        } else if !spec.value_type.accepts(value.value_type()) {
            return Err(Error(format!(
                "{id}.{name}: expected {}, got {:?}",
                spec.value_type,
                value.value_type()
            )));
        }
        let vector = matches!(spec.default, Some(Value::Vector(_)));
        let color = matches!(spec.default, Some(Value::Color(_)));
        if vector && value.source_kind().is_none() && !matches!(value, Value::Vector(_)) {
            return Err(Error(format!("{name}: expected a vector")));
        }
        if let Value::Time(time) = value {
            if time.curve.is_color() != (vector || color) {
                return Err(Error(format!(
                    "{name}: curve components do not fit the input"
                )));
            }
        }
        if color {
            if let Value::Space(space) = value {
                if space.gradient.is_none() {
                    return Err(Error("Color needs a gradient".into()));
                }
            }
        }
        if !vector && !color && spec.value_type.signal_type().is_some() {
            let (low, high) = validate::bounds(value)?;
            let (min, max) = match spec.author {
                Some(Author::Number { min, max }) => (
                    min.unwrap_or(f64::NEG_INFINITY),
                    max.unwrap_or(f64::INFINITY),
                ),
                _ => (0., 1.),
            };
            if low < min || high > max {
                return Err(Error(format!("{name}: values must stay in {min}..{max}")));
            }
        }
        if let (
            Value::Choice(name),
            Some(Author::Choice {
                options,
                custom: false,
            }),
        ) = (value, &spec.author)
        {
            if !options.iter().any(|p| p.value == *value) {
                return Err(Error(format!("unknown choice {name}")));
            }
        }
        if name == "direction" && matches!(value,Value::Vector(v) if v.iter().all(|v|v.abs()<1e-9))
        {
            return Err(Error("a direction must not be zero".into()));
        }
        if name == "fade" {
            if let Value::Time(time) = value {
                if time.events.as_ref().is_some_and(|e| !matches!(e, Events::Own(clock) if clock.every.scalar_value() == Some(0.) && clock.life.is_none())) || time.phase.scalar_value() != Some(0.) || time.gain.scalar_value().is_none() {
                    return Err(Error("Clip fade runs once over the whole clip".into()));
                }
            }
        }
    }
    validate::validate_clock_dependencies(inputs)
}
