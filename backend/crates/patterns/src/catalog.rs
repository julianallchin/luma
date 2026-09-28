use crate::*;

fn input(
    name: &str,
    description: &str,
    value_type: ValueType,
    rate: Rate,
    default: Option<Value>,
) -> Input {
    Input {
        optional: false,
        name: name.into(),
        description: description.into(),
        value_type,
        rate,
        default,
        author: None,
        promotable: Vec::new(),
    }
}
fn field(name: &str, description: &str, value: Value, rate: Rate) -> Input {
    input(name, description, value.value_type(), rate, Some(value))
}
fn primitive_definition(p: Primitive) -> Definition {
    if let Some(definition) = crate::forms::ops::definition(p) {
        return definition;
    }
    if p == Primitive::ClipRange {
        return crate::clip_range::definition();
    }
    if p == Primitive::Output {
        return crate::output::terminal_definition();
    }
    if let Some(definition) = crate::features::definition(p) {
        return definition;
    }
    if let Some(definition) = crate::signals::definition(p).or_else(|| crate::color::definition(p))
    {
        return definition;
    }
    if let Some(definition) = crate::field_ops::definition(p) {
        return definition;
    }
    use Rate::{Fixed, Frame};
    let mapping = || {
        input(
            "Mapping",
            "Resolved coordinates of independently controllable cells",
            ValueType::Coordinates,
            Fixed,
            None,
        )
    };
    let proportion = |name, default| {
        field(
            name,
            "Fraction from 0 to 1",
            Value::Proportion(default),
            Frame,
        )
    };
    let (name, inputs, outputs) = match p {
        Primitive::ResolveMapping => (
            "Resolve Mapping",
            vec![(
                "mapping",
                field(
                    "Mapping",
                    "Coordinate source, grouping, and orientation",
                    Value::Mapping(MappingSpec {
                        span: Default::default(),
                        plane: None,
                        mirror: None,
                        source: MappingSource::Z,
                        per_group: false,
                        reverse: false,
                    }),
                    Fixed,
                ),
            )],
            vec![("coordinates", ValueType::Coordinates, Fixed)],
        ),
        Primitive::CoordinateOffset => (
            "Coordinate offset",
            vec![
                ("mapping", mapping()),
                (
                    "position",
                    input(
                        "Position",
                        "Center in mapped coordinates",
                        ValueType::Signal(SignalType {
                            unit: Some(Unit::Position),
                            channels: None,
                        }),
                        Frame,
                        Some(Value::Position(0.0)),
                    ),
                ),
                (
                    "boundary",
                    field(
                        "Boundary",
                        "Use mapping topology, clip, or wrap",
                        Value::Boundary(Boundary::Natural),
                        Fixed,
                    ),
                ),
            ],
            vec![
                (
                    "value",
                    ValueType::Signal(SignalType {
                        unit: Some(Unit::Number),
                        channels: None,
                    }),
                    Frame,
                ),
                ("wrapped", ValueType::Mask, Frame),
            ],
        ),
        Primitive::Envelope => (
            "Evaluate Envelope",
            vec![
                (
                    "shape",
                    field(
                        "Envelope",
                        "Normalized editable curve",
                        Value::Envelope(Envelope::linear(vec![[0.0, 1.0], [1.0, 0.0]])),
                        Frame,
                    ),
                ),
                ("progress", proportion("Progress", 0.0)),
            ],
            vec![("value", ValueType::Proportion, Frame)],
        ),
        _ => unreachable!("fundamental field definition handled above"),
    };
    Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: outputs
            .into_iter()
            .map(|(k, t, r)| {
                (
                    k.into(),
                    Output {
                        value_type: t,
                        rate: r,
                    },
                )
            })
            .collect(),
        body: Body::Primitive(p),
    }
}
pub(crate) fn primitive(p: Primitive) -> Definition {
    let mut definition = primitive_definition(p);
    if p == Primitive::Envelope {
        let key = "progress";
        let kind = ValueType::Signal(SignalType {
            unit: Some(Unit::Proportion),
            channels: None,
        });
        definition.inputs.get_mut(key).unwrap().value_type = kind;
        for output in definition.outputs.values_mut() {
            output.value_type = kind;
        }
    }
    for input in definition.inputs.values_mut() {
        if let Some(signal) = input.value_type.signal_type() {
            input.value_type = ValueType::Signal(signal);
        }
    }
    for output in definition.outputs.values_mut() {
        if let Some(signal) = output.value_type.signal_type() {
            output.value_type = ValueType::Signal(signal);
        }
    }
    definition
}

/// Numerical nodes and reusable graphs. Output is the only capability terminal.
pub fn standard_library() -> Library {
    static LIBRARY: std::sync::OnceLock<Library> = std::sync::OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let mut library = Library {
                definitions: serde_json::from_str(include_str!("recipes.json"))
                    .expect("canonical graph recipes"),
            };
            for (id, op) in [
                ("band_energy", Primitive::BandEnergy),
                ("clip_time", Primitive::ClipTime),
                ("clip_range", Primitive::ClipRange),
                ("coordinate_offset", Primitive::CoordinateOffset),
                ("core/add", Primitive::FieldBinary(FieldMath::Add)),
                ("core/channel_maximum", Primitive::ChannelMaximum),
                ("core/join_channels", Primitive::JoinChannels),
                ("core/choose_number", Primitive::ChooseNumber),
                ("core/clamp_coverage", Primitive::FieldClamp),
                ("core/divide", Primitive::FieldBinary(FieldMath::Divide)),
                ("core/fraction", Primitive::Fraction),
                ("core/greater", Primitive::FieldGreater),
                ("core/maximum", Primitive::FieldBinary(FieldMath::Maximum)),
                ("core/minimum", Primitive::FieldBinary(FieldMath::Minimum)),
                ("core/multiply", Primitive::FieldBinary(FieldMath::Multiply)),
                ("core/noise", Primitive::Noise),
                ("core/subtract", Primitive::FieldBinary(FieldMath::Subtract)),
                ("core/event_life", Primitive::EventLife),
                ("core/odometer", Primitive::Odometer),
                ("core/curve", Primitive::SampleCurve),
                ("core/random_share", Primitive::RandomShare),
                ("core/path_glides", Primitive::PathGlides),
                ("core/aim_base", Primitive::AimBase),
                ("core/aim_fan", Primitive::AimFan),
                ("core/aim_motion", Primitive::AimMotion),
                ("core/aim_offset", Primitive::AimOffset),
                ("core/aim_turn", Primitive::AimTurn),
                ("envelope", Primitive::Envelope),
                ("output", Primitive::Output),
                ("resolve_mapping", Primitive::ResolveMapping),
                ("sample_gradient", Primitive::SampleGradient),
            ] {
                library.definitions.insert(id.into(), primitive(op));
            }
            for (id, form) in crate::forms::definitions() {
                library.definitions.insert(id.into(), form);
            }
            library
        })
        .clone()
}
