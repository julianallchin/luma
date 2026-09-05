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
    let mask = || {
        input(
            "Mask",
            "Coverage keyed by cell identity",
            ValueType::Mask,
            Frame,
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
        Primitive::Motion => (
            "Motion",
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
                (
                    "start",
                    field(
                        "Start position",
                        "Position in the mapped domain; outside values are allowed",
                        Value::Position(0.0),
                        Frame,
                    ),
                ),
                (
                    "end",
                    field(
                        "End position",
                        "Position in the mapped domain; outside values are allowed",
                        Value::Position(1.0),
                        Frame,
                    ),
                ),
            ],
            vec![
                ("position", ValueType::Position, Frame),
                ("progress", ValueType::Proportion, Frame),
                ("active", ValueType::Proportion, Frame),
            ],
        ),
        Primitive::Pill => (
            "Pill",
            vec![
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
            ],
            vec![("mask", ValueType::Mask, Frame)],
        ),
        Primitive::Dissolve => (
            "Dissolve Mask",
            vec![
                ("mapping", mapping()),
                ("progress", proportion("Progress", 0.0)),
                ("softness", proportion("Cell fade softness", 0.0)),
                (
                    "cycle",
                    field(
                        "Cycle",
                        "Stable stroke identity from Rhythm",
                        Value::Number(0.0),
                        Frame,
                    ),
                ),
                (
                    "reseed",
                    field(
                        "New order each stroke",
                        "Keep thresholds fixed within each stroke",
                        Value::Boolean(true),
                        Fixed,
                    ),
                ),
            ],
            vec![("mask", ValueType::Mask, Frame)],
        ),
        Primitive::Appearance => (
            "Appearance",
            vec![
                ("mask", mask()),
                (
                    "color",
                    field(
                        "Color",
                        "Linear RGB light color",
                        Value::Color([1.0; 3]),
                        Frame,
                    ),
                ),
                ("brightness", proportion("Brightness", 1.0)),
            ],
            vec![("lighting", ValueType::Lighting, Frame)],
        ),
        Primitive::MultiplyMask => (
            "Multiply Masks",
            vec![("a", mask()), ("b", mask())],
            vec![("mask", ValueType::Mask, Frame)],
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
    for (id, p) in [
        ("resolve_mapping", Primitive::ResolveMapping),
        ("rhythm", Primitive::Rhythm),
        ("motion", Primitive::Motion),
        ("pill", Primitive::Pill),
        ("dissolve_mask", Primitive::Dissolve),
        ("appearance", Primitive::Appearance),
        ("multiply_mask", Primitive::MultiplyMask),
        ("add_lighting", Primitive::AddLighting),
        ("envelope", Primitive::Envelope),
        ("soft_edges", Primitive::SoftEdges),
    ] {
        library.definitions.insert(id.into(), primitive(p));
    }
    let mut inputs = BTreeMap::new();
    for (id, names) in [
        ("rhythm", vec!["repeat", "grid_aligned"]),
        ("motion", vec!["travel", "start", "end"]),
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
        definition: "chase_mask".into(),
        inputs: bindings.into_iter().collect(),
    };
    for name in ["color", "brightness"] {
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
                            &[
                                ("mask", wire("mask", "mask")),
                                ("color", exposed("color")),
                                ("brightness", exposed("brightness")),
                            ],
                        ),
                    ),
                ]),
                outputs: BTreeMap::from([("lighting".into(), wire("appearance", "lighting"))]),
            }),
        },
    );
    let mut inputs = BTreeMap::new();
    for (id, names) in [
        ("rhythm", vec!["repeat", "grid_aligned"]),
        ("motion", vec!["travel"]),
        ("dissolve_mask", vec!["mapping", "softness", "reseed"]),
        ("appearance", vec!["color", "brightness"]),
    ] {
        for name in names {
            inputs.insert(name.into(), library.definitions[id].inputs[name].clone());
        }
    }
    inputs.insert(
        "mapping".into(),
        library.definitions["resolve_mapping"].inputs["mapping"].clone(),
    );
    let mut nodes = timing;
    nodes.get_mut("motion").unwrap().inputs.remove("start");
    nodes.get_mut("motion").unwrap().inputs.remove("end");
    nodes.insert(
        "dissolve".into(),
        node(
            "dissolve_mask",
            &[
                ("mapping", wire("mapping", "coordinates")),
                ("softness", exposed("softness")),
                ("reseed", exposed("reseed")),
                ("progress", wire("motion", "progress")),
                ("cycle", wire("rhythm", "cycle")),
            ],
        ),
    );
    nodes.insert(
        "appearance".into(),
        node(
            "appearance",
            &[
                ("mask", wire("dissolve", "mask")),
                ("color", exposed("color")),
                ("brightness", exposed("brightness")),
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
    library
}
