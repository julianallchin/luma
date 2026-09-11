//! Native authoring projection of the typed lighting catalogue. The graph file
//! retains canvas positions; evaluation uses the same definitions as the core.
use crate::models::node_graph::*;
use luma_patterns::{self as p, ValueType};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const PREFIX: &str = "lighting/";

#[cfg(test)]
mod mapping_tests {
    use super::*;

    #[test]
    fn authoring_projection_preserves_structured_mapping_values() {
        for source in [
            p::MappingSource::Circle { origin: 0.375 },
            p::MappingSource::MajorAxis {
                toward: [1., -2., 3.],
            },
            p::MappingSource::Vector {
                direction: [1., 0., 2.],
            },
        ] {
            let value = p::Value::Mapping(p::MappingSpec {
                mirror: Some(p::MirrorPlane {
                    normal: [1., 0., 1.],
                    offset: 0.25,
                }),
                source,
                reverse: true,
                per_group: true,
            });
            assert_eq!(
                decode(ValueType::Mapping, &wire_value(&value)).expect("valid mapping"),
                value
            );
        }
    }
}

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
        ValueType::Mapping => &p::MappingSource::OPTIONS,
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
        ValueType::Seed => PortType::Seed,
        ValueType::Signal(_) | ValueType::Number | ValueType::Field => PortType::Signal,
        ValueType::Color | ValueType::ColorField => PortType::Signal,
        ValueType::Gradient => PortType::Stops,
        ValueType::AudioSource => PortType::Audio,
        ValueType::Drum | ValueType::Events => PortType::Events,
        ValueType::Beats => PortType::Signal,
        ValueType::Proportion => PortType::Signal,
        ValueType::Position => PortType::Signal,
        ValueType::Boolean => PortType::Boolean,
        ValueType::Mapping => PortType::Mapping,
        ValueType::Coordinates => PortType::Coordinates,
        ValueType::Boundary => PortType::Boundary,
        ValueType::Envelope => PortType::Envelope,
        ValueType::Mask => PortType::Signal,
        ValueType::Lighting => PortType::Lighting,
    }
}

pub fn wire_value(value: &p::Value) -> Value {
    match value {
        p::Value::Signal(signal)
            if signal.fixtures().is_none() && signal.values().dim() == (1, 1, 1) =>
        {
            json!(signal.values()[[0, 0, 0]])
        }
        p::Value::Signal(signal) => serde_json::to_value(signal).expect("serializable signal"),
        p::Value::Gradient(gradient) => {
            serde_json::to_value(gradient).expect("serializable gradient")
        }
        p::Value::Color(rgb) => {
            json!({"r": rgb[0]*255., "g": rgb[1]*255., "b": rgb[2]*255., "a": 1.})
        }
        _ => serde_json::to_value(value).expect("serializable default")["value"].clone(),
    }
}

pub fn decode(kind: ValueType, value: &Value) -> Result<p::Value, String> {
    if let ValueType::Signal(spec) = kind {
        let decoded = if value.get("values").is_some() {
            p::Value::Signal(
                serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid signal: {e}"))?,
            )
        } else if spec.channels == Some(p::Channels::Rgb)
            || value.get("r").is_some()
            || value.is_array()
            || value.as_str().is_some_and(|v| v.starts_with('#'))
        {
            decode(ValueType::Color, value)?
        } else {
            let number = value.as_f64().ok_or("A numerical signal needs a value")?;
            match spec.unit {
                Some(p::Unit::Beats) => p::Value::Beats(number),
                Some(p::Unit::Proportion) => p::Value::Proportion(number),
                Some(p::Unit::Position) => p::Value::Position(number),
                Some(p::Unit::Degrees) => p::Value::Degrees(number),
                Some(p::Unit::Seconds) => p::Value::Seconds(number),
                _ => p::Value::Number(number),
            }
        };
        decoded.validate().map_err(|e| e.to_string())?;
        if !kind.accepts(decoded.value_type()) {
            return Err("Signal units or channels do not match this input".into());
        }
        return Ok(decoded);
    }
    let value = match kind {
        ValueType::Gradient => {
            let stops = value
                .get("stops")
                .and_then(Value::as_array)
                .ok_or("Gradient needs stops")?;
            let stops = stops
                .iter()
                .map(|stop| {
                    // Gradient opacity is separate from RGB. Historical stops
                    // may store it in the color object or an eight-digit hex.
                    let mut rgb = stop["color"].clone();
                    if let Some(object) = rgb.as_object_mut() {
                        object.remove("a");
                    } else if let Some(hex) = rgb.as_str() {
                        if hex.starts_with('#') && hex.len() == 9 {
                            let rgba = u32::from_str_radix(&hex[1..], 16)
                                .map_err(|_| "Invalid gradient color")?;
                            rgb = json!([
                                ((rgba >> 24) & 255) as f64 / 255.,
                                ((rgba >> 16) & 255) as f64 / 255.,
                                ((rgba >> 8) & 255) as f64 / 255.
                            ]);
                        }
                    }
                    let color = decode(ValueType::Color, &rgb)?;
                    let p::Value::Color(color) = color else {
                        unreachable!()
                    };
                    let alpha = stop
                        .get("alpha")
                        .and_then(Value::as_f64)
                        .unwrap_or_else(|| {
                            super::migration::values::parse_color(&stop["color"])[3] as f64
                        });
                    Ok(json!({"t":stop["t"], "color":color, "alpha":alpha}))
                })
                .collect::<Result<Vec<_>, String>>()?;
            json!({"stops":stops})
        }
        ValueType::Mapping if value.is_string() => {
            let source = p::MappingSource::from_key(value.as_str().unwrap())
                .map_err(|error| error.to_string())?;
            return Ok(p::Value::Mapping(p::MappingSpec {
                mirror: None,
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
    // Schema-v3 standalone patterns retain their historical ports for decoding;
    // the score canvas calls types_for with its canonical library directly.
    let library = p::migration::v2_library();
    let mut types = types_for(&library);
    types.retain(|node| {
        node.id
            .strip_prefix(PREFIX)
            .is_some_and(|id| library.definitions.contains_key(id))
    });
    for node in &mut types {
        let definition = &library.definitions[node.id.strip_prefix(PREFIX).unwrap()];
        node.outputs.extend(
            definition
                .outputs
                .iter()
                .filter(|(_, output)| output.value_type == ValueType::Lighting)
                .map(|(id, _)| PortDef {
                    id: id.clone(),
                    name: id.clone(),
                    port_type: PortType::Lighting,
                }),
        );
    }
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

pub fn input_node_id(key: &str) -> String {
    format!("$input/{key}")
}
pub fn input_node_key(id: &str) -> Option<&str> {
    id.strip_prefix("$input/")
}
fn input_type_id(definition: &str, key: &str) -> String {
    format!("{PREFIX}$input/{definition}/{key}")
}
/// The card a graph's named outputs are wired into. A pattern ends at its
/// Apply node instead.
pub const OUTPUTS_NODE: &str = "$outputs";
fn outputs_type_id(definition: &str) -> String {
    format!("{PREFIX}$outputs/{definition}")
}

pub fn types_for(library: &p::Library) -> Vec<NodeTypeDef> {
    let mut types: Vec<_> = library
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
                        range: match input.author() {
                            Some(p::Author::Number {
                                min: Some(min),
                                max: Some(max),
                            }) => Some((min as f32, max as f32)),
                            _ => None,
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
                description: Some("Every input accepts a value or a connection.".into()),
                category: Some(
                    if def.lighting_output().is_some() {
                        "Output"
                    } else {
                        "Signals and controls"
                    }
                    .into(),
                ),
                inputs,
                outputs: def
                    .outputs
                    .iter()
                    .filter(|(_, output)| output.value_type != ValueType::Lighting)
                    .map(|(id, output)| PortDef {
                        id: id.clone(),
                        name: id.clone(),
                        port_type: port_type(output.value_type),
                    })
                    .collect(),
                params,
            }
        })
        .collect();
    for (id, definition) in &library.definitions {
        let p::Body::Graph(graph) = &definition.body else {
            continue;
        };
        if definition.lighting_output().is_none() && !definition.outputs.is_empty() {
            types.push(NodeTypeDef {
                id: outputs_type_id(id),
                name: "Outputs".into(),
                description: Some("What this graph provides to its callers".into()),
                category: Some("Outputs".into()),
                inputs: definition
                    .outputs
                    .iter()
                    .map(|(key, output)| PortDef {
                        id: key.clone(),
                        name: key.clone(),
                        port_type: port_type(output.value_type),
                    })
                    .collect(),
                params: Vec::new(),
                outputs: Vec::new(),
            });
        }
        let keys: std::collections::BTreeSet<_> = definition
            .inputs
            .keys()
            .chain(graph.input_nodes.keys())
            .collect();
        for key in keys {
            let spec = definition.inputs.get(key);
            let name = spec
                .map(|spec| spec.name.clone())
                .unwrap_or_else(|| graph.input_nodes[key].name.clone());
            types.push(NodeTypeDef {
                id: input_type_id(id, key),
                name,
                description: Some("Exposed Input".into()),
                category: Some("Inputs".into()),
                inputs: Vec::new(),
                params: Vec::new(),
                outputs: vec![PortDef {
                    id: "value".into(),
                    name: "value".into(),
                    port_type: spec
                        .map(|spec| port_type(spec.value_type))
                        .unwrap_or(PortType::Signal),
                }],
            });
        }
    }
    types
}

pub fn input_label(input: &p::Input) -> String {
    let suffix = match input
        .value_type
        .signal_type()
        .filter(|spec| spec.channels != Some(p::Channels::Rgb))
        .and_then(|spec| spec.unit)
    {
        Some(p::Unit::Beats) => " (beats)",
        Some(p::Unit::Proportion) => " (0–1)",
        Some(p::Unit::Degrees) => " (degrees)",
        Some(p::Unit::Seconds) => " (seconds)",
        _ => "",
    };
    if input.name.ends_with(suffix) {
        input.name.clone()
    } else {
        format!("{}{suffix}", input.name)
    }
}

/// The graph and clip inspectors share the destination's editor metadata.
pub fn arg_type(kind: ValueType) -> Option<PatternArgType> {
    if let Some(spec) = kind.signal_type() {
        return match spec.channels {
            Some(p::Channels::Rgb) => Some(PatternArgType::Color),
            Some(p::Channels::PanTilt | p::Channels::Components(_)) => Some(PatternArgType::Scalar),
            Some(p::Channels::Value) | None => Some(match spec.unit {
                Some(p::Unit::Beats) => PatternArgType::Beats,
                Some(p::Unit::Proportion) => PatternArgType::Proportion,
                Some(p::Unit::Position) => PatternArgType::Position,
                _ => PatternArgType::Scalar,
            }),
        };
    }
    Some(match kind {
        ValueType::Seed => PatternArgType::Seed,
        ValueType::Gradient => PatternArgType::Gradient,
        ValueType::AudioSource => PatternArgType::AudioSource,
        ValueType::Drum => PatternArgType::Drum,
        ValueType::Mapping => PatternArgType::Mapping,
        ValueType::Boundary => PatternArgType::Boundary,
        ValueType::Envelope => PatternArgType::Envelope,
        ValueType::Boolean => PatternArgType::Boolean,
        _ => return None,
    })
}

/// A playable one-node Pattern, with the node's inputs exposed to each clip.
pub fn pattern(effect: &str) -> Result<Graph, String> {
    let library = p::migration::v2_library();
    let def = library
        .definitions
        .get(effect)
        .ok_or("Unknown lighting node")?;
    if def.lighting_output().is_none() {
        return Err("A score requires one Lighting output".into());
    }
    let mut args = Vec::new();
    for (id, input) in &def.inputs {
        if input.value_type == ValueType::Events {
            continue;
        }
        let arg_type = arg_type(input.value_type).ok_or_else(|| {
            format!(
                "{} needs an enclosing graph to supply {}",
                def.name, input.name
            )
        })?;
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

/// Canvas projection of a definition. A node the document does not place
/// carries no position: the canvas lays those out itself, once it knows how
/// big each card is.
pub fn project_definition(library: &p::Library, id: &str) -> Option<Graph> {
    let definition = library.definitions.get(id)?;
    let p::Body::Graph(body) = &definition.body else {
        return None;
    };
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut wire =
        |to: &str, port: &str, binding: &p::Binding, params: &mut HashMap<String, Value>| {
            let (source, output) = match binding {
                p::Binding::Value { value } => {
                    params.insert(port.into(), wire_value(value));
                    return;
                }
                p::Binding::Input { input } => (input_node_id(input), "value".into()),
                p::Binding::Connection { node, output } => (node.clone(), output.clone()),
            };
            edges.push(Edge {
                id: format!("{to}/{port}"),
                from_node: source,
                from_port: output,
                to_node: to.into(),
                to_port: port.into(),
            });
        };
    for (id, node) in &body.nodes {
        let mut params = HashMap::new();
        for (port, binding) in &node.inputs {
            wire(id, port, binding, &mut params);
        }
        nodes.push(NodeInstance {
            id: id.clone(),
            type_id: format!("{PREFIX}{}", node.definition),
            params,
            position_x: node.position.map(|p| p[0]),
            position_y: node.position.map(|p| p[1]),
        });
    }
    if definition.lighting_output().is_none() && !body.outputs.is_empty() {
        let mut params = HashMap::new();
        for (key, binding) in &body.outputs {
            wire(OUTPUTS_NODE, key, binding, &mut params);
        }
        nodes.push(NodeInstance {
            id: OUTPUTS_NODE.into(),
            type_id: outputs_type_id(id),
            params,
            position_x: None,
            position_y: None,
        });
    }
    let keys: std::collections::BTreeSet<_> = definition
        .inputs
        .keys()
        .chain(body.input_nodes.keys())
        .collect();
    for key in keys {
        let position = body.input_nodes.get(key).and_then(|node| node.position);
        nodes.push(NodeInstance {
            id: input_node_id(key),
            type_id: input_type_id(id, key),
            params: HashMap::new(),
            position_x: position.map(|p| p[0]),
            position_y: position.map(|p| p[1]),
        });
    }
    Some(Graph {
        nodes,
        edges,
        args: Vec::new(),
    })
}
