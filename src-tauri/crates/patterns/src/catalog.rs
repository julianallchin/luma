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
pub(crate) fn primitive(p: Primitive) -> Definition {
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
                        Value::Envelope(Envelope {
                            points: vec![[0.0, 1.0], [1.0, 0.0]],
                        }),
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
fn exposed(name: &str) -> Binding {
    Binding::Input { input: name.into() }
}
fn wire(node: &str, output: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: output.into(),
    }
}
fn node(definition: &str, inputs: &[(&str, Binding)]) -> Node {
    Node {
        position: None,
        definition: definition.into(),
        inputs: inputs
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect(),
    }
}

/// Built-in complete effects are ordinary graphs. Their interfaces are derived
/// by exposing primitive inputs, preserving type/default/editor semantics.
pub fn standard_library() -> Library {
    let mut library = Library::default();
    for (id, op) in [
        ("core/add", Primitive::FieldBinary(FieldMath::Add)),
        ("core/subtract", Primitive::FieldBinary(FieldMath::Subtract)),
        ("core/multiply", Primitive::FieldBinary(FieldMath::Multiply)),
        ("core/divide", Primitive::FieldBinary(FieldMath::Divide)),
        ("core/minimum", Primitive::FieldBinary(FieldMath::Minimum)),
        ("core/maximum", Primitive::FieldBinary(FieldMath::Maximum)),
        (
            "core/number_field",
            Primitive::Broadcast(ScalarKind::Number),
        ),
        (
            "core/coverage_field",
            Primitive::Broadcast(ScalarKind::Proportion),
        ),
        ("core/mask_values", Primitive::MaskToField),
        ("core/clamp_coverage", Primitive::FieldClamp),
        ("core/greater", Primitive::FieldGreater),
        ("core/choose", Primitive::FieldSelect),
        ("core/random", Primitive::RandomField),
        ("core/choose_number", Primitive::ChooseNumber),
    ] {
        library.definitions.insert(id.into(), primitive(op));
    }

    for (id, p) in [
        ("resolve_mapping", Primitive::ResolveMapping),
        ("rhythm", Primitive::Rhythm),
        ("core/travel_time", Primitive::TravelClock),
        ("coordinate_offset", Primitive::CoordinateOffset),
        ("sample_field_envelope", Primitive::FieldEnvelope),
        ("write_position", Primitive::WritePosition),
        ("write_speed", Primitive::WriteSpeed),
        ("add_lighting", Primitive::AddLighting),
        ("envelope", Primitive::Envelope),
        ("soft_edges", Primitive::SoftEdges),
    ] {
        library.definitions.insert(id.into(), primitive(p));
    }
    crate::recipes::foundation(&mut library);
    library.definitions.insert(
        "multiply_mask".into(),
        Definition {
            name: "Multiply Masks".into(),
            inputs: BTreeMap::from([
                (
                    "a".into(),
                    library.definitions["appearance"].inputs["mask"].clone(),
                ),
                (
                    "b".into(),
                    library.definitions["appearance"].inputs["mask"].clone(),
                ),
            ]),
            outputs: library.definitions["core/clamp_coverage"].outputs.clone(),
            body: Body::Graph(Graph {
                nodes: BTreeMap::from([
                    (
                        "a".into(),
                        node("core/mask_values", &[("mask", exposed("a"))]),
                    ),
                    (
                        "b".into(),
                        node("core/mask_values", &[("mask", exposed("b"))]),
                    ),
                    (
                        "product".into(),
                        node(
                            "core/multiply",
                            &[("a", wire("a", "value")), ("b", wire("b", "value"))],
                        ),
                    ),
                    (
                        "coverage".into(),
                        node(
                            "core/clamp_coverage",
                            &[("value", wire("product", "value"))],
                        ),
                    ),
                ]),
                outputs: BTreeMap::from([("mask".into(), wire("coverage", "mask"))]),
            }),
        },
    );
    library
        .definitions
        .insert("scale_mask".into(), scale_mask_graph(&library));
    library.definitions.insert("pill".into(), pill_graph());
    library
        .definitions
        .insert("dissolve_mask".into(), dissolve_graph());
    let mut inputs = BTreeMap::new();
    for (id, names) in [
        ("rhythm", vec!["repeat", "grid_aligned", "delay"]),
        ("motion", vec!["travel", "start", "end", "path"]),
        ("pill", vec!["mapping", "width", "shape", "boundary"]),
    ] {
        for name in names {
            inputs.insert(name.into(), library.definitions[id].inputs[name].clone());
        }
    }
    inputs.insert(
        "mapping".into(),
        library.definitions["resolve_mapping"].inputs["mapping"].clone(),
    );
    let timing = BTreeMap::from([
        (
            "mapping".into(),
            node("resolve_mapping", &[("mapping", exposed("mapping"))]),
        ),
        (
            "rhythm".into(),
            node(
                "rhythm",
                &[
                    ("repeat", exposed("repeat")),
                    ("grid_aligned", exposed("grid_aligned")),
                    ("delay", exposed("delay")),
                ],
            ),
        ),
        (
            "motion".into(),
            node(
                "motion",
                &[
                    ("elapsed", wire("rhythm", "elapsed")),
                    ("travel", exposed("travel")),
                    ("repeat", exposed("repeat")),
                    ("start", exposed("start")),
                    ("end", exposed("end")),
                    ("path", exposed("path")),
                ],
            ),
        ),
    ]);
    let mut graph = Graph {
        nodes: timing.clone(),
        outputs: BTreeMap::from([("mask".into(), wire("pill", "mask"))]),
    };
    graph.nodes.insert(
        "pill".into(),
        node(
            "pill",
            &[
                ("mapping", wire("mapping", "coordinates")),
                ("width", exposed("width")),
                ("shape", exposed("shape")),
                ("boundary", exposed("boundary")),
                ("position", wire("motion", "position")),
                ("active", wire("motion", "active")),
            ],
        ),
    );
    library.definitions.insert(
        "chase_mask".into(),
        Definition {
            name: "Chase Mask".into(),
            inputs,
            outputs: library.definitions["pill"].outputs.clone(),
            body: Body::Graph(graph),
        },
    );
    // The same mask graph is usable alone, nested, or exposed by a pattern.
    let mut inputs = library.definitions["chase_mask"].inputs.clone();
    let bindings: Vec<_> = inputs.keys().map(|k| (k.clone(), exposed(k))).collect();
    let mask_node = Node {
        position: None,
        definition: "chase_mask".into(),
        inputs: bindings.into_iter().collect(),
    };
    for name in ["color"] {
        inputs.insert(
            name.into(),
            library.definitions["appearance"].inputs[name].clone(),
        );
    }
    library.definitions.insert(
        "chase".into(),
        Definition {
            name: "Chase".into(),
            inputs,
            outputs: library.definitions["appearance"].outputs.clone(),
            body: Body::Graph(Graph {
                nodes: BTreeMap::from([
                    ("mask".into(), mask_node),
                    (
                        "appearance".into(),
                        node(
                            "appearance",
                            &[("mask", wire("mask", "mask")), ("color", exposed("color"))],
                        ),
                    ),
                ]),
                outputs: BTreeMap::from([("lighting".into(), wire("appearance", "lighting"))]),
            }),
        },
    );
    let mut inputs = BTreeMap::new();
    for (id, names) in [
        ("rhythm", vec!["repeat", "grid_aligned", "delay"]),
        ("motion", vec!["travel"]),
        (
            "dissolve_mask",
            vec!["softness", "reseed", "refresh", "refresh_every"],
        ),
        ("appearance", vec!["color"]),
    ] {
        for name in names {
            inputs.insert(name.into(), library.definitions[id].inputs[name].clone());
        }
    }
    inputs.insert(
        "shape".into(),
        library.definitions["envelope"].inputs["shape"].clone(),
    );
    inputs.get_mut("shape").unwrap().name = "Fade shape".into();
    let mut nodes = timing;
    nodes.insert(
        "coverage".into(),
        node(
            "envelope",
            &[
                ("shape", exposed("shape")),
                ("progress", wire("motion", "progress")),
            ],
        ),
    );
    nodes.remove("mapping");
    nodes.get_mut("motion").unwrap().inputs.remove("start");
    nodes.get_mut("motion").unwrap().inputs.remove("end");
    nodes.get_mut("motion").unwrap().inputs.remove("path");
    nodes.insert(
        "dissolve".into(),
        node(
            "dissolve_mask",
            &[
                ("softness", exposed("softness")),
                ("refresh", exposed("refresh")),
                ("refresh_every", exposed("refresh_every")),
                ("reseed", exposed("reseed")),
                ("coverage", wire("coverage", "value")),
                ("cycle", wire("rhythm", "cycle")),
            ],
        ),
    );
    nodes.insert(
        "active".into(),
        node(
            "scale_mask",
            &[
                ("mask", wire("dissolve", "mask")),
                ("amount", wire("motion", "active")),
            ],
        ),
    );
    nodes.insert(
        "appearance".into(),
        node(
            "appearance",
            &[
                ("mask", wire("active", "mask")),
                ("color", exposed("color")),
            ],
        ),
    );
    library.definitions.insert(
        "dissolve_flash".into(),
        Definition {
            name: "Dissolve Flash".into(),
            inputs,
            outputs: library.definitions["appearance"].outputs.clone(),
            body: Body::Graph(Graph {
                nodes,
                outputs: BTreeMap::from([("lighting".into(), wire("appearance", "lighting"))]),
            }),
        },
    );
    crate::recipes::extend(&mut library);
    library
}

/// Dissolve is a recipe: random thresholds, arithmetic, comparison and choice.
/// No effect-specific runtime operation is needed for either dissolve or flicker.
fn dissolve_graph() -> Definition {
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
    let mut nodes = BTreeMap::new();
    let constant = |value| Binding::Value {
        value: Value::Number(value),
    };
    nodes.insert(
        "one".into(),
        node("core/number_field", &[("value", constant(1.0))]),
    );
    nodes.insert(
        "zero".into(),
        node("core/number_field", &[("value", constant(0.0))]),
    );
    nodes.insert(
        "coverage_value".into(),
        node("core/coverage_field", &[("value", exposed("coverage"))]),
    );
    nodes.insert(
        "progress".into(),
        node(
            "core/subtract",
            &[
                ("a", wire("one", "value")),
                ("b", wire("coverage_value", "value")),
            ],
        ),
    );
    nodes.insert(
        "softness".into(),
        node("core/coverage_field", &[("value", exposed("softness"))]),
    );
    nodes.insert(
        "refresh_clock".into(),
        node("rhythm", &[("repeat", exposed("refresh_every"))]),
    );
    nodes.insert(
        "stroke".into(),
        node(
            "core/choose_number",
            &[
                ("condition", exposed("reseed")),
                ("yes", exposed("cycle")),
                ("no", constant(0.0)),
            ],
        ),
    );
    nodes.insert(
        "epoch".into(),
        node(
            "core/choose_number",
            &[
                ("condition", exposed("refresh")),
                ("yes", wire("refresh_clock", "cycle")),
                ("no", wire("stroke", "value")),
            ],
        ),
    );
    nodes.insert(
        "random".into(),
        node("core/random", &[("epoch", wire("epoch", "value"))]),
    );
    for (id, operation, a, b) in [
        ("spread", "core/subtract", "one", "softness"),
        ("fade_start", "core/multiply", "random", "spread"),
        ("elapsed", "core/subtract", "progress", "fade_start"),
        ("fraction", "core/divide", "elapsed", "softness"),
        ("fade", "core/subtract", "one", "fraction"),
    ] {
        nodes.insert(
            id.into(),
            node(
                operation,
                &[("a", wire(a, "value")), ("b", wire(b, "value"))],
            ),
        );
    }
    nodes.insert(
        "hard".into(),
        node(
            "core/greater",
            &[
                ("a", wire("random", "value")),
                ("b", wire("progress", "value")),
            ],
        ),
    );
    nodes.insert(
        "hard_values".into(),
        node("core/mask_values", &[("mask", wire("hard", "mask"))]),
    );
    nodes.insert(
        "soft".into(),
        node(
            "core/greater",
            &[
                ("a", wire("softness", "value")),
                ("b", wire("zero", "value")),
            ],
        ),
    );
    nodes.insert(
        "choose".into(),
        node(
            "core/choose",
            &[
                ("condition", wire("soft", "mask")),
                ("yes", wire("fade", "value")),
                ("no", wire("hard_values", "value")),
            ],
        ),
    );
    nodes.insert(
        "coverage".into(),
        node("core/clamp_coverage", &[("value", wire("choose", "value"))]),
    );
    Definition {
        name: "Dissolve Mask".into(),
        inputs,
        outputs: BTreeMap::from([(
            "mask".into(),
            Output {
                value_type: ValueType::Mask,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Graph(Graph {
            nodes,
            outputs: BTreeMap::from([("mask".into(), wire("coverage", "mask"))]),
        }),
    }
}

fn pill_graph() -> Definition {
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
                Value::Envelope(Envelope {
                    points: vec![[0., 0.], [0.05, 1.], [0.95, 1.], [1., 0.]],
                }),
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

    let mut nodes = BTreeMap::new();
    nodes.insert(
        "offset".into(),
        node(
            "coordinate_offset",
            &[
                ("mapping", exposed("mapping")),
                ("position", exposed("position")),
                ("boundary", exposed("boundary")),
            ],
        ),
    );
    for (id, value) in [("zero", 0.0), ("half", 0.5), ("one", 1.0)] {
        nodes.insert(
            id.into(),
            node(
                "core/number_field",
                &[("value", Value::Number(value).into())],
            ),
        );
    }
    for id in ["width", "active"] {
        nodes.insert(
            id.into(),
            node("core/coverage_field", &[("value", exposed(id))]),
        );
    }
    for (id, op, a, b) in [
        ("relative", "core/divide", "offset", "width"),
        ("phase", "core/add", "relative", "half"),
    ] {
        nodes.insert(
            id.into(),
            node(op, &[("a", wire(a, "value")), ("b", wire(b, "value"))]),
        );
    }
    nodes.insert(
        "shape".into(),
        node(
            "sample_field_envelope",
            &[
                ("phase", wire("phase", "value")),
                ("shape", exposed("shape")),
            ],
        ),
    );
    for (id, a, b) in [
        ("past_start", "phase", "zero"),
        ("before_end", "one", "phase"),
        ("nonzero", "width", "zero"),
        ("partial", "one", "width"),
    ] {
        nodes.insert(
            id.into(),
            node(
                "core/greater",
                &[("a", wire(a, "value")), ("b", wire(b, "value"))],
            ),
        );
        nodes.insert(
            format!("{id}_value"),
            node("core/mask_values", &[("mask", wire(id, "mask"))]),
        );
    }
    for id in ["past_start", "before_end"] {
        nodes
            .get_mut(id)
            .unwrap()
            .inputs
            .insert("tolerance".into(), Value::Number(1e-12).into());
    }
    nodes.insert(
        "wrapped_value".into(),
        node("core/mask_values", &[("mask", wire("offset", "wrapped"))]),
    );
    nodes.insert(
        "shape_value".into(),
        node("core/mask_values", &[("mask", wire("shape", "mask"))]),
    );
    for (id, op, a, b) in [
        (
            "inside",
            "core/multiply",
            "past_start_value",
            "before_end_value",
        ),
        ("full_width", "core/subtract", "one", "partial_value"),
        (
            "full_circle",
            "core/multiply",
            "full_width",
            "wrapped_value",
        ),
        ("included", "core/maximum", "inside", "full_circle"),
        ("visible", "core/multiply", "included", "nonzero_value"),
        ("shaped", "core/multiply", "shape_value", "visible"),
        ("gated", "core/multiply", "shaped", "active"),
    ] {
        nodes.insert(
            id.into(),
            node(op, &[("a", wire(a, "value")), ("b", wire(b, "value"))]),
        );
    }
    nodes.insert(
        "coverage".into(),
        node("core/clamp_coverage", &[("value", wire("gated", "value"))]),
    );
    Definition {
        name: "Pill".into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: primitive(Primitive::FieldClamp).outputs,
        body: Body::Graph(Graph {
            nodes,
            outputs: BTreeMap::from([("mask".into(), wire("coverage", "mask"))]),
        }),
    }
}

fn scale_mask_graph(library: &Library) -> Definition {
    Definition {
        name: "Scale mask".into(),
        inputs: BTreeMap::from([
            (
                "mask".into(),
                library.definitions["appearance"].inputs["mask"].clone(),
            ),
            (
                "amount".into(),
                field(
                    "Amount",
                    "Mask strength",
                    Value::Proportion(1.0),
                    Rate::Frame,
                ),
            ),
        ]),
        outputs: library.definitions["core/clamp_coverage"].outputs.clone(),
        body: Body::Graph(Graph {
            nodes: BTreeMap::from([
                (
                    "mask".into(),
                    node("core/mask_values", &[("mask", exposed("mask"))]),
                ),
                (
                    "amount".into(),
                    node("core/coverage_field", &[("value", exposed("amount"))]),
                ),
                (
                    "product".into(),
                    node(
                        "core/multiply",
                        &[("a", wire("mask", "value")), ("b", wire("amount", "value"))],
                    ),
                ),
                (
                    "coverage".into(),
                    node(
                        "core/clamp_coverage",
                        &[("value", wire("product", "value"))],
                    ),
                ),
            ]),
            outputs: BTreeMap::from([("mask".into(), wire("coverage", "mask"))]),
        }),
    }
}
