//! Native authoring projection of the typed lighting catalogue. The graph file
//! retains canvas positions; evaluation uses the same definitions as the core.
use crate::models::node_graph::*;
use luma_patterns::{self as p, ValueType};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub const PREFIX: &str = "lighting/";

pub fn choices(kind: ValueType) -> Vec<ParamOption> {
    let rows: &[(&str, &str)] = match kind {
        ValueType::AudioSource => &[
            ("mix", "Full mix"),
            ("bass", "Bass"),
            ("drums", "Drums"),
            ("vocals", "Vocals"),
            ("other", "Other instruments"),
        ],
        ValueType::Drum => &[
            ("kick", "Kick"),
            ("snare", "Snare"),
            ("hihat", "Hi-hat"),
            ("cymbal", "Cymbal"),
        ],
        ValueType::Mapping => &[
            ("z", "Up (Z+)"),
            ("u", "Stage right (U+)"),
            ("v", "Downstage (V+)"),
            ("major_axis", "Major axis"),
            ("circle", "Solved circle"),
            ("order", "Selection order"),
        ],
        ValueType::Boundary => &[("natural", "Natural"), ("clip", "Clip"), ("wrap", "Wrap")],
        ValueType::Boolean => &[("true", "Yes"), ("false", "No")],
        _ => &[],
    };
    rows.iter()
        .map(|(id, label)| ParamOption {
            id: (*id).into(),
            label: (*label).into(),
        })
        .collect()
}

pub fn port_type(kind: ValueType) -> PortType {
    match kind {
        ValueType::Number | ValueType::Field => PortType::Signal,
        ValueType::Color | ValueType::ColorField => PortType::Signal,
        ValueType::Gradient => PortType::Stops,
        ValueType::AudioSource => PortType::Audio,
        ValueType::Drum => PortType::Events,
        ValueType::Beats => PortType::Beats,
        ValueType::Proportion => PortType::Proportion,
        ValueType::Position => PortType::Position,
        ValueType::Boolean => PortType::Boolean,
        ValueType::Mapping => PortType::Mapping,
        ValueType::Coordinates => PortType::Coordinates,
        ValueType::Boundary => PortType::Boundary,
        ValueType::Envelope => PortType::Envelope,
        ValueType::Mask => PortType::Mask,
        ValueType::Lighting => PortType::Lighting,
    }
}

pub fn wire_value(value: &p::Value) -> Value {
    match value {
        p::Value::Mapping(m) => json!(match m.source {
            p::MappingSource::U => "u",
            p::MappingSource::V => "v",
            p::MappingSource::Z => "z",
            p::MappingSource::Order => "order",
            p::MappingSource::MajorAxis { .. } => "major_axis",
            p::MappingSource::Circle { .. } => "circle",
        }),
        p::Value::Gradient(gradient) => {
            json!({"stops": gradient.stops.iter().map(|stop| json!({"t":stop.t, "color":stop.color})).collect::<Vec<_>>()})
        }
        p::Value::Color(rgb) => {
            json!({"r": rgb[0]*255., "g": rgb[1]*255., "b": rgb[2]*255., "a": 1.})
        }
        _ => serde_json::to_value(value).expect("serializable default")["value"].clone(),
    }
}

pub fn decode(kind: ValueType, value: &Value) -> Result<p::Value, String> {
    let value = match kind {
        ValueType::Gradient => {
            let stops = value
                .get("stops")
                .and_then(Value::as_array)
                .ok_or("Gradient needs stops")?;
            let stops = stops
                .iter()
                .map(|stop| {
                    let color = decode(ValueType::Color, &stop["color"])?;
                    let p::Value::Color(color) = color else {
                        unreachable!()
                    };
                    Ok(json!({"t":stop["t"], "color":color}))
                })
                .collect::<Result<Vec<_>, String>>()?;
            json!({"stops":stops})
        }
        ValueType::Mapping if value.is_string() => {
            let source = match value.as_str().unwrap() {
                "u" => p::MappingSource::U,
                "v" => p::MappingSource::V,
                "z" => p::MappingSource::Z,
                "order" => p::MappingSource::Order,
                "major_axis" => p::MappingSource::MajorAxis {
                    toward: [0., 0., 1.],
                },
                "circle" => p::MappingSource::Circle { origin: 0. },
                other => return Err(format!("Unknown mapping {other}")),
            };
            return Ok(p::Value::Mapping(p::MappingSpec {
                source,
                per_group: false,
                reverse: false,
            }));
        }
        ValueType::Color
            if value
                .get("a")
                .is_some_and(|alpha| alpha.as_f64() != Some(1.0)) =>
        {
            return Err(
                "A Color input is RGB; use Lighting composition for inheritance or mixing".into(),
            );
        }
        ValueType::Color if value.get("r").is_some() => json!([
            value["r"].as_f64().unwrap_or(0.) / 255.,
            value["g"].as_f64().unwrap_or(0.) / 255.,
            value["b"].as_f64().unwrap_or(0.) / 255.
        ]),
        ValueType::Color if value.as_str().is_some_and(|s| s.starts_with('#')) => {
            let hex = value.as_str().unwrap().trim_start_matches('#');
            let n = u32::from_str_radix(hex, 16).map_err(|_| "Invalid color")?;
            if hex.len() != 6 {
                return Err("Color needs six hex digits".into());
            }
            json!([
                ((n >> 16) & 255) as f64 / 255.,
                ((n >> 8) & 255) as f64 / 255.,
                (n & 255) as f64 / 255.
            ])
        }
        ValueType::Boolean if value.is_string() => match value.as_str() {
            Some("true") => json!(true),
            Some("false") => json!(false),
            _ => return Err("Expected yes or no".into()),
        },
        ValueType::Envelope if value.is_string() => {
            serde_json::from_str(value.as_str().unwrap())
                .map_err(|e| format!("Invalid envelope: {e}"))?
        }
        _ => value.clone(),
    };
    let decoded: p::Value =
        serde_json::from_value(json!({"type":kind,"value":value})).map_err(|e| e.to_string())?;
    decoded.validate().map_err(|e| e.to_string())?;
    Ok(decoded)
}

pub fn node_types() -> Vec<NodeTypeDef> {
    let mut types = types_for(&p::standard_library());
    // Only legacy graphs bind their execution domain through a Selection port.
    for node in &mut types {
        if node
            .outputs
            .iter()
            .any(|output| output.port_type == PortType::Lighting)
        {
            node.inputs.push(PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            });
        }
    }
    types
}

pub fn types_for(library: &p::Library) -> Vec<NodeTypeDef> {
    library
        .definitions
        .iter()
        .map(|(id, def)| {
            let params = def
                .inputs
                .iter()
                .filter_map(|(key, input)| {
                    let default = input.default.as_ref()?;
                    let wire = wire_value(default);
                    let options = choices(input.value_type);
                    let param_type = if !options.is_empty() {
                        ParamType::Enum { options }
                    } else if wire.is_number() {
                        ParamType::Number
                    } else {
                        ParamType::Text
                    };
                    let text = match default {
                        p::Value::Color(_) => "#ffffff".into(),
                        _ => wire
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| wire.to_string()),
                    };
                    Some(ParamDef {
                        id: key.clone(),
                        name: input_label(input),
                        param_type,
                        default_number: wire.as_f64().map(|v| v as f32),
                        default_text: Some(text),
                        range: if input.value_type == ValueType::Proportion {
                            Some((0., 1.))
                        } else {
                            None
                        },
                    })
                })
                .collect();
            let inputs: Vec<_> = def
                .inputs
                .iter()
                .map(|(id, input)| PortDef {
                    id: id.clone(),
                    name: input_label(input),
                    port_type: port_type(input.value_type),
                })
                .collect();
            NodeTypeDef {
                id: format!("{PREFIX}{id}"),
                name: library.display_name(id),
                description: Some(
                    "Typed lighting node. Every input accepts a value or a connection.".into(),
                ),
                category: Some(
                    if def.lighting_output().is_some() {
                        "Lighting"
                    } else {
                        "Lighting components"
                    }
                    .into(),
                ),
                inputs,
                outputs: def
                    .outputs
                    .iter()
                    .map(|(id, output)| PortDef {
                        id: id.clone(),
                        name: id.clone(),
                        port_type: port_type(output.value_type),
                    })
                    .collect(),
                params,
            }
        })
        .collect()
}

fn input_label(input: &p::Input) -> String {
    if input.value_type == ValueType::Beats {
        format!("{} (beats)", input.name)
    } else if input.value_type == ValueType::Proportion {
        format!("{} (0–1)", input.name)
    } else {
        input.name.clone()
    }
}

/// A playable one-node Pattern, with the node's inputs exposed to each clip.
pub fn pattern(effect: &str) -> Result<Graph, String> {
    let library = p::standard_library();
    let def = library
        .definitions
        .get(effect)
        .ok_or("Unknown lighting node")?;
    if def.lighting_output().is_none() {
        return Err("A score requires one Lighting output".into());
    }
    let mut args = Vec::new();
    for (id, input) in &def.inputs {
        let arg_type = match input.value_type {
            ValueType::Number => PatternArgType::Scalar,
            ValueType::Beats => PatternArgType::Beats,
            ValueType::Proportion => PatternArgType::Proportion,
            ValueType::Position => PatternArgType::Position,
            ValueType::Color => PatternArgType::Color,
            ValueType::Gradient => PatternArgType::Gradient,
            ValueType::AudioSource => PatternArgType::AudioSource,
            ValueType::Drum => PatternArgType::Drum,
            ValueType::Mapping => PatternArgType::Mapping,
            ValueType::Boundary => PatternArgType::Boundary,
            ValueType::Envelope => PatternArgType::Envelope,
            ValueType::Boolean => PatternArgType::Boolean,
            _ => {
                return Err(format!(
                    "{} needs an enclosing graph to supply {}",
                    def.name, input.name
                ))
            }
        };
        let default = input.default.as_ref().ok_or_else(|| {
            format!(
                "{} needs an enclosing graph to supply {}",
                def.name, input.name
            )
        })?;
        args.push(PatternArgDef {
            id: id.clone(),
            name: input_label(input),
            arg_type,
            default_value: wire_value(default),
        });
    }
    args.push(PatternArgDef {
        id: "selection".into(),
        name: "Selection".into(),
        arg_type: PatternArgType::Selection,
        default_value: crate::models::selection::Selection::all().to_value(),
    });
    let edges = args
        .iter()
        .map(|arg| Edge {
            id: format!("input-{}", arg.id),
            from_node: "pattern_args".into(),
            from_port: arg.id.clone(),
            to_node: "effect".into(),
            to_port: arg.id.clone(),
        })
        .collect();
    Ok(Graph {
        nodes: vec![
            NodeInstance {
                id: "pattern_args".into(),
                type_id: "pattern_args".into(),
                params: HashMap::new(),
                position_x: Some(0.),
                position_y: Some(0.),
            },
            NodeInstance {
                id: "effect".into(),
                type_id: format!("{PREFIX}{effect}"),
                params: HashMap::new(),
                position_x: Some(320.),
                position_y: Some(0.),
            },
        ],
        edges,
        args,
    })
}

pub fn arg_choices(kind: &PatternArgType) -> Vec<ParamOption> {
    choices(match kind {
        PatternArgType::Mapping => ValueType::Mapping,
        PatternArgType::AudioSource => ValueType::AudioSource,
        PatternArgType::Drum => ValueType::Drum,
        PatternArgType::Boundary => ValueType::Boundary,
        PatternArgType::Boolean => ValueType::Boolean,
        _ => return Vec::new(),
    })
}

/// Preserve old authored softness controls by making their envelope construction
/// explicit. Clip overrides and graph connections retain their original IDs.
pub fn upgrade_shape_inputs(graph: &mut Graph) -> bool {
    let mut changed = false;
    let targets: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.type_id.as_str(),
                "lighting/chase" | "lighting/chase_mask" | "lighting/pill"
            )
        })
        .map(|n| n.id.clone())
        .collect();
    for target in targets {
        let old_edge = graph
            .edges
            .iter()
            .position(|e| e.to_node == target && e.to_port == "softness");
        let old_value = graph
            .nodes
            .iter_mut()
            .find(|n| n.id == target)
            .unwrap()
            .params
            .remove("softness");
        if old_edge.is_none() && old_value.is_none() {
            continue;
        }
        let mut id = format!("{target}_shape");
        while graph.nodes.iter().any(|n| n.id == id) {
            id.push('_');
        }
        let mut params = HashMap::new();
        if let Some(value) = old_value {
            params.insert("softness".into(), value);
        }
        if let Some(index) = old_edge {
            graph.edges[index].to_node = id.clone();
        }
        graph.nodes.push(NodeInstance {
            id: id.clone(),
            type_id: "lighting/soft_edges".into(),
            params,
            position_x: None,
            position_y: None,
        });
        let mut edge_id = format!("{id}_output");
        while graph.edges.iter().any(|e| e.id == edge_id) {
            edge_id.push('_');
        }
        graph.edges.push(Edge {
            id: edge_id,
            from_node: id,
            from_port: "shape".into(),
            to_node: target,
            to_port: "shape".into(),
        });
        changed = true;
    }
    changed
}

/// Read-only canvas projection of a built-in definition. This is never saved
/// or compiled: the canonical graph remains the typed library definition.
pub fn inspect_definition(id: &str) -> Option<Graph> {
    project_definition(&p::standard_library(), id)
}

pub fn project_definition(library: &p::Library, id: &str) -> Option<Graph> {
    let definition = library.definitions.get(id)?;
    let p::Body::Graph(body) = &definition.body else {
        return None;
    };
    let mut graph = Graph {
        nodes: Vec::new(),
        edges: Vec::new(),
        args: Vec::new(),
    };
    let mut depths = BTreeMap::new();
    fn depth(id: &str, body: &p::Graph, depths: &mut BTreeMap<String, usize>) -> usize {
        if let Some(value) = depths.get(id) {
            return *value;
        }
        let value = body.nodes[id]
            .inputs
            .values()
            .filter_map(|binding| match binding {
                p::Binding::Connection { node, .. } => Some(depth(node, body, depths) + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        depths.insert(id.into(), value);
        value
    }
    let mut rows = BTreeMap::<usize, usize>::new();
    for (id, node) in &body.nodes {
        let column = depth(id, body, &mut depths);
        let row = rows.entry(column).or_default();
        let mut params = HashMap::new();
        for (port, binding) in &node.inputs {
            let (source, output) = match binding {
                p::Binding::Value { value } => {
                    params.insert(port.clone(), wire_value(value));
                    continue;
                }
                p::Binding::Input { input } => ("__inputs".to_owned(), input.clone()),
                p::Binding::Connection { node, output } => (node.clone(), output.clone()),
            };
            graph.edges.push(Edge {
                id: format!("{id}/{port}"),
                from_node: source,
                from_port: output,
                to_node: id.clone(),
                to_port: port.clone(),
            });
        }
        graph.nodes.push(NodeInstance {
            id: id.clone(),
            type_id: format!("{PREFIX}{}", node.definition),
            params,
            position_x: Some(node.position.map_or(column as f64 * 520.0, |p| p[0])),
            position_y: Some(node.position.map_or(*row as f64 * 360.0, |p| p[1])),
        });
        *row += 1;
    }
    graph.nodes.push(NodeInstance {
        id: "__inputs".into(),
        type_id: "pattern_args".into(),
        params: HashMap::new(),
        position_x: None,
        position_y: None,
    });
    Some(graph)
}
