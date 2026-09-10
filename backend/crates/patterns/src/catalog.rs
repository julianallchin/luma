use crate::*;
use std::collections::BTreeMap;

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
    }
}
fn field(name: &str, description: &str, value: Value, rate: Rate) -> Input {
    input(name, description, value.value_type(), rate, Some(value))
}
fn primitive_definition(p: Primitive) -> Definition {
    if let Some(definition) = crate::point_fields::definition(p) {
        return definition;
    }
    if p == Primitive::ClipRange {
        return crate::clip_range::definition();
    }
    if p == Primitive::RandomEventTargets {
        return crate::event_targets::definition();
    }
    if let Some(definition) = crate::event_timing::definition(p) {
        return definition;
    }
    if p == Primitive::Output {
        return crate::output::terminal_definition();
    }
    if let Some(definition) = crate::event_tensor::definition(p) {
        return definition;
    }
    if let Some(definition) = crate::features::definition(p) {
        return definition;
    }
    if let Some(definition) = crate::signals::definition(p).or_else(|| crate::color::definition(p))
    {
        return definition;
    }
    if let Some(definition) =
        crate::field_ops::definition(p).or_else(|| crate::metrics::definition(p))
    {
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
    let lighting = || {
        input(
            "Lighting",
            "Lighting contribution keyed by cell identity",
            ValueType::Lighting,
            Frame,
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
        Primitive::Rhythm => (
            "Rhythm",
            vec![
                (
                    "delay",
                    field(
                        "Phase delay",
                        "Shift stroke starts later on the beat grid",
                        Value::Beats(0.0),
                        Fixed,
                    ),
                ),
                (
                    "repeat",
                    field(
                        "Repeat",
                        "Time between stroke starts, in beats",
                        Value::Beats(4.0),
                        Fixed,
                    ),
                ),
                (
                    "grid_aligned",
                    field(
                        "Follow track grid",
                        "Otherwise start at the clip boundary",
                        Value::Boolean(false),
                        Fixed,
                    ),
                ),
            ],
            vec![
                ("elapsed", ValueType::Beats, Frame),
                ("cycle", ValueType::Number, Frame),
            ],
        ),
        Primitive::TravelClock => (
            "Travel time",
            vec![
                (
                    "elapsed",
                    field(
                        "Elapsed",
                        "Musical time since this stroke began",
                        Value::Beats(0.0),
                        Frame,
                    ),
                ),
                (
                    "travel",
                    field(
                        "Travel time",
                        "Complete start-to-end journey, in beats",
                        Value::Beats(2.0),
                        Fixed,
                    ),
                ),
                (
                    "repeat",
                    field(
                        "Repeat",
                        "Travel plus dark rest, in beats",
                        Value::Beats(4.0),
                        Fixed,
                    ),
                ),
            ],
            vec![
                ("progress", ValueType::Proportion, Frame),
                ("active", ValueType::Proportion, Frame),
            ],
        ),
        Primitive::CoordinateOffset => (
            "Coordinate offset",
            vec![
                ("mapping", mapping()),
                (
                    "position",
                    field(
                        "Position",
                        "Center in mapped coordinates",
                        Value::Position(0.0),
                        Frame,
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
                ("value", ValueType::Field, Frame),
                ("wrapped", ValueType::Mask, Frame),
            ],
        ),
        Primitive::FieldEnvelope => (
            "Sample envelope per head",
            vec![
                (
                    "phase",
                    input(
                        "Phase",
                        "Coordinate at which to sample the curve",
                        ValueType::Field,
                        Frame,
                        None,
                    ),
                ),
                (
                    "shape",
                    field(
                        "Envelope",
                        "Normalized editable curve",
                        Value::Envelope(Envelope::soft_edges(0.1)),
                        Frame,
                    ),
                ),
            ],
            vec![("mask", ValueType::Mask, Frame)],
        ),
        Primitive::WritePosition => (
            "Position output",
            vec![
                (
                    "pan",
                    field(
                        "Pan (degrees)",
                        "Absolute pan angle in degrees",
                        Value::Number(0.0),
                        Frame,
                    ),
                ),
                (
                    "tilt",
                    field(
                        "Tilt (degrees)",
                        "Absolute tilt angle in degrees",
                        Value::Number(0.0),
                        Frame,
                    ),
                ),
            ],
            vec![("lighting", ValueType::Lighting, Frame)],
        ),
        Primitive::WriteSpeed => (
            "Movement speed output",
            vec![("value", proportion("Value", 1.0))],
            vec![("lighting", ValueType::Lighting, Frame)],
        ),
        Primitive::AddLighting => (
            "Add Lighting",
            vec![("a", lighting()), ("b", lighting())],
            vec![("lighting", ValueType::Lighting, Frame)],
        ),
        Primitive::SoftEdges => (
            "Soft Edges",
            vec![(
                "softness",
                field(
                    "Edge softness",
                    "Symmetric edge fraction",
                    Value::Proportion(0.1),
                    Frame,
                ),
            )],
            vec![("shape", ValueType::Envelope, Frame)],
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
    if matches!(p, Primitive::Envelope | Primitive::FieldEnvelope) {
        let key = if p == Primitive::Envelope {
            "progress"
        } else {
            "phase"
        };
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
                ("audio_spectrum", Primitive::AudioSpectrum),
                ("audio_lowpass", Primitive::FilterAudio { highpass: false }),
                ("audio_highpass", Primitive::FilterAudio { highpass: true }),
                ("beat_trigger", Primitive::BeatEvents),
                ("clip_time", Primitive::ClipTime),
                ("clip_range", Primitive::ClipRange),
                ("coordinate_offset", Primitive::CoordinateOffset),
                ("core/absolute", Primitive::FieldUnary(UnaryMath::Absolute)),
                ("core/add", Primitive::FieldBinary(FieldMath::Add)),
                ("core/channel_maximum", Primitive::ChannelMaximum),
                ("core/channel_sum", Primitive::ChannelSum),
                ("core/channel_argmax", Primitive::ChannelArgmax),
                ("core/channel_index", Primitive::ChannelIndex),
                ("core/channel_count", Primitive::ChannelCount),
                ("core/channel", Primitive::Channel),
                ("core/join_channels", Primitive::JoinChannels),
                ("core/chase_events", Primitive::ChaseEvents),
                ("core/choose", Primitive::FieldSelect),
                ("core/choose_number", Primitive::ChooseNumber),
                ("core/clamp_coverage", Primitive::FieldClamp),
                ("core/dissolve_events", Primitive::DissolveEvents),
                ("core/divide", Primitive::FieldBinary(FieldMath::Divide)),
                (
                    "core/field_maximum",
                    Primitive::FieldReduce(FieldReduction::Maximum),
                ),
                (
                    "core/field_mean",
                    Primitive::FieldReduce(FieldReduction::Mean),
                ),
                (
                    "core/field_minimum",
                    Primitive::FieldReduce(FieldReduction::Minimum),
                ),
                ("core/floor", Primitive::FieldUnary(UnaryMath::Floor)),
                ("core/float32", Primitive::FieldUnary(UnaryMath::Float32)),
                ("core/fraction", Primitive::FieldUnary(UnaryMath::Fraction)),
                ("core/greater", Primitive::FieldGreater),
                (
                    "core/head_count",
                    Primitive::FieldReduce(FieldReduction::Count),
                ),
                ("core/maximum", Primitive::FieldBinary(FieldMath::Maximum)),
                ("core/minimum", Primitive::FieldBinary(FieldMath::Minimum)),
                ("core/multiply", Primitive::FieldBinary(FieldMath::Multiply)),
                ("core/noise", Primitive::Noise),
                ("core/value_noise_1d", Primitive::ValueNoise1d),
                ("core/seed_stream", Primitive::SeedStream),
                ("core/value_noise_3d", Primitive::ValueNoise3d),
                ("core/domain_index", Primitive::DomainIndex),
                ("core/align_domain", Primitive::AlignDomain),
                ("core/power", Primitive::Power),
                ("core/pulse_events", Primitive::PulseEvents),
                ("core/random", Primitive::RandomField),
                ("core/rank", Primitive::FieldRank),
                ("core/field_first", Primitive::FieldFirst),
                ("core/rank_nearby", Primitive::RankNearby),
                ("core/radial_coordinates", Primitive::RadialCoordinates),
                ("core/fit_circle", Primitive::CirclePhase),
                ("core/principal_direction", Primitive::PrincipalDirection),
                (
                    "core/distinct_count",
                    Primitive::FieldReduce(FieldReduction::DistinctCount),
                ),
                ("core/sine", Primitive::FieldUnary(UnaryMath::Sine)),
                (
                    "core/square_root",
                    Primitive::FieldUnary(UnaryMath::SquareRoot),
                ),
                ("core/subtract", Primitive::FieldBinary(FieldMath::Subtract)),
                ("drum_time", Primitive::DrumClock),
                ("drum_trigger", Primitive::DrumEvents),
                ("core/track_time", Primitive::TrackTime),
                ("core/grid_events", Primitive::GridEvents),
                ("core/event_window", Primitive::EventWindow),
                ("core/event_spacing", Primitive::EventSpacing),
                ("core/thin_events", Primitive::ThinEvents),
                ("random_subset", Primitive::RandomEventTargets),
                ("envelope", Primitive::Envelope),
                ("harmony", Primitive::Harmony),
                ("hsv", Primitive::Hsv),
                ("rotate_hue", Primitive::RotateHue),
                ("mask_color", Primitive::MaskColor),
                ("mix_palette", Primitive::MixPalette),
                ("palette_fallback", Primitive::PaletteFallback),
                ("output", Primitive::Output),
                ("resolve_mapping", Primitive::ResolveMapping),
                ("rhythm", Primitive::Rhythm),
                ("sample_field_envelope", Primitive::FieldEnvelope),
                ("sample_field_gradient", Primitive::SampleGradientField),
                ("sample_gradient", Primitive::SampleGradient),
                ("soft_edges", Primitive::SoftEdges),
                ("stage_coordinates", Primitive::StageCoordinates),
                ("wander_points", Primitive::WanderPoints),
                ("proximity_weights", Primitive::ProximityWeights),
                ("fixture_geometry", Primitive::WorldGeometry),
            ] {
                library.definitions.insert(id.into(), primitive(op));
            }
            library
        })
        .clone()
}

pub(crate) fn dissolve_inputs() -> BTreeMap<String, Input> {
    let mut inputs = BTreeMap::new();
    for (key, name, value, rate) in [
        ("coverage", "Coverage", Value::Proportion(1.0), Rate::Frame),
        (
            "softness",
            "Cell fade softness",
            Value::Proportion(0.0),
            Rate::Frame,
        ),
        ("cycle", "Stroke index", Value::Number(0.0), Rate::Frame),
        (
            "reseed",
            "New order each stroke",
            Value::Boolean(true),
            Rate::Fixed,
        ),
        ("refresh", "Flicker", Value::Boolean(false), Rate::Fixed),
        (
            "refresh_every",
            "Refresh every",
            Value::Beats(0.125),
            Rate::Fixed,
        ),
    ] {
        inputs.insert(key.into(), field(name, name, value, rate));
    }
    inputs
}

pub(crate) fn pill_inputs() -> BTreeMap<String, Input> {
    use Rate::{Fixed, Frame};
    let mapping = || {
        input(
            "Mapping",
            "Resolved coordinates",
            ValueType::Coordinates,
            Fixed,
            None,
        )
    };
    let proportion = |name, value| {
        field(
            name,
            "Fraction of the selected domain",
            Value::Proportion(value),
            Frame,
        )
    };
    let inputs = vec![
        ("mapping", mapping()),
        (
            "position",
            field(
                "Position",
                "Stroke center in mapped coordinates",
                Value::Position(0.0),
                Frame,
            ),
        ),
        ("width", proportion("Width", 0.25)),
        (
            "shape",
            field(
                "Shape",
                "Brightness across the stroke, from its negative to positive edge",
                Value::Envelope(Envelope::linear(vec![
                    [0., 0.],
                    [0.05, 1.],
                    [0.95, 1.],
                    [1., 0.],
                ])),
                Frame,
            ),
        ),
        ("active", proportion("Activity", 1.0)),
        (
            "boundary",
            field(
                "Boundary",
                "Follow mapping, clip, or wrap",
                Value::Boundary(Boundary::Natural),
                Fixed,
            ),
        ),
    ];

    inputs
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
}
