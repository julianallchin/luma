//! Shipped effects are inspected and copied as ordinary graphs. Interfaces are
//! exposed child inputs, so authoring a custom node uses exactly the same rules.
use crate::*;
use std::collections::BTreeMap;

fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn input(key: &str) -> Binding {
    Binding::Input { input: key.into() }
}
fn node(definition: &str, inputs: &[(&str, Binding)]) -> Node {
    Node {
        definition: definition.into(),
        inputs: inputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        position: None,
    }
}
fn graph(
    library: &mut Library,
    id: &str,
    name: &str,
    nodes: &[(&str, Node)],
    output: (&str, &str, &str),
) {
    let mut inputs = BTreeMap::<String, Input>::new();
    for (_, n) in nodes {
        for (port, binding) in &n.inputs {
            if let Binding::Input { input } = binding {
                let spec = library.definitions[&n.definition].inputs[port].clone();
                if let Some(old) = inputs.insert(input.clone(), spec.clone()) {
                    assert_eq!(old.value_type, spec.value_type, "recipe input {id}.{input}");
                }
            }
        }
    }
    let (key, output_node, port) = output;
    let target = &nodes
        .iter()
        .find(|(id, _)| *id == output_node)
        .unwrap()
        .1
        .definition;
    library.definitions.insert(
        id.into(),
        Definition {
            name: name.into(),
            inputs,
            outputs: BTreeMap::from([(
                key.into(),
                library.definitions[target].outputs[port].clone(),
            )]),
            body: Body::Graph(Graph {
                nodes: nodes
                    .iter()
                    .map(|(id, n)| (id.to_string(), n.clone()))
                    .collect(),
                outputs: BTreeMap::from([(key.into(), wire(output_node, port))]),
            }),
        },
    );
}
fn default(library: &mut Library, id: &str, key: &str, name: &str, value: Value) {
    let spec = library
        .definitions
        .get_mut(id)
        .unwrap()
        .inputs
        .get_mut(key)
        .unwrap();
    assert_eq!(spec.value_type, value.value_type());
    spec.name = name.into();
    spec.default = Some(value);
}
pub(crate) fn foundation(library: &mut Library) {
    use crate::catalog::primitive;
    for (id, op) in [
        ("clip_time", Primitive::ClipTime),
        ("band_energy", Primitive::BandEnergy),
        ("drum_time", Primitive::DrumClock),
        ("harmony", Primitive::Harmony),
        ("core/absolute", Primitive::FieldUnary(UnaryMath::Absolute)),
        ("core/floor", Primitive::FieldUnary(UnaryMath::Floor)),
        ("core/fraction", Primitive::FieldUnary(UnaryMath::Fraction)),
        ("core/sine", Primitive::FieldUnary(UnaryMath::Sine)),
        ("core/noise", Primitive::Noise),
        ("core/dimmer_output", Primitive::WriteMask),
        ("sample_gradient", Primitive::SampleGradient),
        ("sample_field_gradient", Primitive::SampleGradientField),
        ("core/color_field", Primitive::ColorField),
        ("mask_color", Primitive::MaskColor),
        ("core/color_output", Primitive::WriteColor),
        ("hsv", Primitive::Hsv),
        ("core/beats_field", Primitive::Broadcast(ScalarKind::Beats)),
        (
            "core/position_field",
            Primitive::Broadcast(ScalarKind::Position),
        ),
    ] {
        library.definitions.insert(id.into(), primitive(op));
    }
    for (id, math) in [
        ("add", FieldMath::Add),
        ("subtract", FieldMath::Subtract),
        ("multiply", FieldMath::Multiply),
        ("divide", FieldMath::Divide),
        ("minimum", FieldMath::Minimum),
        ("maximum", FieldMath::Maximum),
    ] {
        library.definitions.insert(
            format!("core/{id}_number"),
            primitive(Primitive::ScalarBinary(math)),
        );
    }
    for (id, from, to) in [
        ("beats_number", ScalarKind::Beats, ScalarKind::Number),
        (
            "coverage_number",
            ScalarKind::Proportion,
            ScalarKind::Number,
        ),
        ("position_number", ScalarKind::Position, ScalarKind::Number),
        ("number_position", ScalarKind::Number, ScalarKind::Position),
        ("number_beats", ScalarKind::Number, ScalarKind::Beats),
        (
            "number_coverage",
            ScalarKind::Number,
            ScalarKind::Proportion,
        ),
    ] {
        library.definitions.insert(
            format!("core/{id}"),
            primitive(Primitive::ScalarConvert { from, to }),
        );
    }

    graph(
        library,
        "appearance",
        "Appearance",
        &[
            (
                "color",
                node("core/color_field", &[("color", input("color"))]),
            ),
            (
                "mask",
                node(
                    "mask_color",
                    &[("color", wire("color", "color")), ("mask", input("mask"))],
                ),
            ),
            (
                "output",
                node("core/color_output", &[("color", wire("mask", "color"))]),
            ),
        ],
        ("lighting", "output", "lighting"),
    );
    graph(
        library,
        "uniform_mask",
        "Uniform mask",
        &[
            (
                "value",
                node("core/coverage_field", &[("value", input("coverage"))]),
            ),
            (
                "mask",
                node("core/clamp_coverage", &[("value", wire("value", "value"))]),
            ),
        ],
        ("mask", "mask", "mask"),
    );
    default(
        library,
        "uniform_mask",
        "coverage",
        "Coverage",
        Value::Proportion(1.0),
    );
    graph(
        library,
        "wash",
        "Wash",
        &[
            ("mask", node("uniform_mask", &[])),
            (
                "color",
                node(
                    "appearance",
                    &[("mask", wire("mask", "mask")), ("color", input("color"))],
                ),
            ),
        ],
        ("lighting", "color", "lighting"),
    );
    graph(
        library,
        "write_dimmer",
        "Dimmer output",
        &[
            (
                "mask",
                node("uniform_mask", &[("coverage", input("value"))]),
            ),
            (
                "output",
                node("core/dimmer_output", &[("mask", wire("mask", "mask"))]),
            ),
        ],
        ("lighting", "output", "lighting"),
    );
}
pub(crate) fn extend(library: &mut Library) {
    graph(
        library,
        "pulse_mask",
        "Pulse mask",
        &[
            (
                "clock",
                node(
                    "rhythm",
                    &[
                        ("repeat", input("repeat")),
                        ("grid_aligned", input("grid_aligned")),
                    ],
                ),
            ),
            (
                "motion",
                node(
                    "motion",
                    &[
                        ("elapsed", wire("clock", "elapsed")),
                        ("travel", input("travel")),
                        ("repeat", input("repeat")),
                    ],
                ),
            ),
            (
                "curve",
                node(
                    "envelope",
                    &[
                        ("progress", wire("motion", "progress")),
                        ("shape", input("shape")),
                    ],
                ),
            ),
            (
                "mask",
                node("uniform_mask", &[("coverage", wire("curve", "value"))]),
            ),
            (
                "active",
                node(
                    "scale_mask",
                    &[
                        ("mask", wire("mask", "mask")),
                        ("amount", wire("motion", "active")),
                    ],
                ),
            ),
        ],
        ("mask", "active", "mask"),
    );
    for (id, name, sink) in [
        ("pulse", "Pulse", "appearance"),
        ("pulse_dimmer", "Dimmer pulse", "core/dimmer_output"),
    ] {
        let mut nodes = vec![(
            "pulse",
            node(
                "pulse_mask",
                &[
                    ("repeat", input("repeat")),
                    ("grid_aligned", input("grid_aligned")),
                    ("travel", input("travel")),
                    ("shape", input("shape")),
                ],
            ),
        )];
        let mut bindings = vec![("mask", wire("pulse", "mask"))];
        if sink == "appearance" {
            bindings.push(("color", input("color")));
        }
        nodes.push(("output", node(sink, &bindings)));
        graph(
            library,
            id,
            name,
            &nodes,
            ("lighting", "output", "lighting"),
        );
    }

    graph(
        library,
        "gradient",
        "Color fade",
        &[
            ("time", node("clip_time", &[])),
            (
                "gradient",
                node(
                    "sample_gradient",
                    &[
                        ("position", wire("time", "progress")),
                        ("gradient", input("gradient")),
                    ],
                ),
            ),
            (
                "output",
                node("wash", &[("color", wire("gradient", "color"))]),
            ),
        ],
        ("lighting", "output", "lighting"),
    );
    graph(
        library,
        "mapped_position",
        "Mapped position",
        &[
            (
                "mapping",
                node("resolve_mapping", &[("mapping", input("mapping"))]),
            ),
            (
                "position",
                node(
                    "coordinate_offset",
                    &[
                        ("mapping", wire("mapping", "coordinates")),
                        ("boundary", Value::Boundary(Boundary::Clip).into()),
                    ],
                ),
            ),
        ],
        ("value", "position", "value"),
    );
    graph(
        library,
        "spatial_gradient",
        "Spatial gradient",
        &[
            (
                "mapping",
                node("mapped_position", &[("mapping", input("mapping"))]),
            ),
            (
                "gradient",
                node(
                    "sample_field_gradient",
                    &[
                        ("position", wire("mapping", "value")),
                        ("gradient", input("gradient")),
                    ],
                ),
            ),
            (
                "output",
                node("core/color_output", &[("color", wire("gradient", "color"))]),
            ),
        ],
        ("lighting", "output", "lighting"),
    );

    graph(
        library,
        "noise_mask",
        "Noise mask",
        &[
            (
                "x",
                node(
                    "mapped_position",
                    &[(
                        "mapping",
                        Value::Mapping(MappingSpec {
                            source: MappingSource::U,
                            per_group: false,
                            reverse: false,
                        })
                        .into(),
                    )],
                ),
            ),
            (
                "y",
                node(
                    "mapped_position",
                    &[(
                        "mapping",
                        Value::Mapping(MappingSpec {
                            source: MappingSource::V,
                            per_group: false,
                            reverse: false,
                        })
                        .into(),
                    )],
                ),
            ),
            (
                "scale",
                node("core/number_field", &[("value", input("scale"))]),
            ),
            (
                "scale_x",
                node(
                    "core/multiply",
                    &[("a", wire("x", "value")), ("b", wire("scale", "value"))],
                ),
            ),
            (
                "scale_y",
                node(
                    "core/multiply",
                    &[("a", wire("y", "value")), ("b", wire("scale", "value"))],
                ),
            ),
            ("clock", node("clip_time", &[])),
            (
                "time",
                node("core/beats_field", &[("value", wire("clock", "elapsed"))]),
            ),
            (
                "period",
                node("core/beats_field", &[("value", input("period"))]),
            ),
            (
                "speed",
                node(
                    "core/divide",
                    &[("a", wire("time", "value")), ("b", wire("period", "value"))],
                ),
            ),
            (
                "noise",
                node(
                    "core/noise",
                    &[
                        ("x", wire("scale_x", "value")),
                        ("y", wire("scale_y", "value")),
                        ("z", wire("speed", "value")),
                    ],
                ),
            ),
            (
                "shape",
                node(
                    "sample_field_envelope",
                    &[("phase", wire("noise", "value")), ("shape", input("shape"))],
                ),
            ),
        ],
        ("mask", "shape", "mask"),
    );
    default(
        library,
        "noise_mask",
        "period",
        "Evolution time (beats per noise cell)",
        Value::Beats(4.0),
    );
    default(
        library,
        "noise_mask",
        "scale",
        "Spatial scale",
        Value::Number(2.0),
    );
    default(
        library,
        "noise_mask",
        "shape",
        "Response",
        Value::Envelope(Envelope {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }),
    );
    graph(
        library,
        "noise_wash",
        "Noise wash",
        &[
            (
                "mask",
                node(
                    "noise_mask",
                    &[
                        ("period", input("period")),
                        ("scale", input("scale")),
                        ("shape", input("shape")),
                    ],
                ),
            ),
            (
                "output",
                node(
                    "appearance",
                    &[("mask", wire("mask", "mask")), ("color", input("color"))],
                ),
            ),
        ],
        ("lighting", "output", "lighting"),
    );

    graph(
        library,
        "event_mask",
        "Event envelope",
        &[
            (
                "time",
                node(
                    "motion",
                    &[
                        ("elapsed", input("elapsed")),
                        ("travel", input("duration")),
                        ("repeat", input("duration")),
                    ],
                ),
            ),
            (
                "shape",
                node(
                    "envelope",
                    &[
                        ("progress", wire("time", "progress")),
                        ("shape", input("shape")),
                    ],
                ),
            ),
            (
                "mask",
                node("uniform_mask", &[("coverage", wire("shape", "value"))]),
            ),
            (
                "active",
                node(
                    "scale_mask",
                    &[
                        ("mask", wire("mask", "mask")),
                        ("amount", wire("time", "active")),
                    ],
                ),
            ),
            (
                "present",
                node(
                    "scale_mask",
                    &[
                        ("mask", wire("active", "mask")),
                        ("amount", input("present")),
                    ],
                ),
            ),
        ],
        ("mask", "present", "mask"),
    );
    default(
        library,
        "event_mask",
        "duration",
        "Fade duration",
        Value::Beats(0.5),
    );
    graph(
        library,
        "drum_mask",
        "Drum pulse mask",
        &[
            ("events", node("drum_time", &[("drum", input("drum"))])),
            (
                "shape",
                node(
                    "event_mask",
                    &[
                        ("elapsed", wire("events", "elapsed")),
                        ("present", wire("events", "present")),
                        ("duration", input("duration")),
                        ("shape", input("shape")),
                    ],
                ),
            ),
        ],
        ("mask", "shape", "mask"),
    );
    graph(
        library,
        "drum_pulse",
        "Drum pulse",
        &[
            (
                "mask",
                node(
                    "drum_mask",
                    &[
                        ("drum", input("drum")),
                        ("duration", input("duration")),
                        ("shape", input("shape")),
                    ],
                ),
            ),
            (
                "output",
                node(
                    "appearance",
                    &[("mask", wire("mask", "mask")), ("color", input("color"))],
                ),
            ),
        ],
        ("lighting", "output", "lighting"),
    );

    graph(
        library,
        "band_mask",
        "Frequency mask",
        &[
            (
                "energy",
                node(
                    "band_energy",
                    &[
                        ("source", input("source")),
                        ("low_hz", input("low_hz")),
                        ("high_hz", input("high_hz")),
                    ],
                ),
            ),
            (
                "field",
                node("core/number_field", &[("value", wire("energy", "value"))]),
            ),
            (
                "gain",
                node("core/number_field", &[("value", input("gain"))]),
            ),
            (
                "amplify",
                node(
                    "core/multiply",
                    &[("a", wire("field", "value")), ("b", wire("gain", "value"))],
                ),
            ),
            (
                "shape",
                node(
                    "sample_field_envelope",
                    &[
                        ("phase", wire("amplify", "value")),
                        ("shape", input("shape")),
                    ],
                ),
            ),
        ],
        ("mask", "shape", "mask"),
    );
    default(
        library,
        "band_mask",
        "gain",
        "Sensitivity",
        Value::Number(10.0),
    );
    default(
        library,
        "band_mask",
        "shape",
        "Response",
        Value::Envelope(Envelope {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }),
    );
    graph(
        library,
        "band_pulse",
        "Frequency pulse",
        &[
            (
                "mask",
                node(
                    "band_mask",
                    &[
                        ("source", input("source")),
                        ("low_hz", input("low_hz")),
                        ("high_hz", input("high_hz")),
                        ("gain", input("gain")),
                        ("shape", input("shape")),
                    ],
                ),
            ),
            (
                "output",
                node(
                    "appearance",
                    &[("mask", wire("mask", "mask")), ("color", input("color"))],
                ),
            ),
        ],
        ("lighting", "output", "lighting"),
    );
}
