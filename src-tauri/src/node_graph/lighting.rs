//! Native authoring projection of the typed lighting catalogue. The graph file
//! retains canvas positions; evaluation uses the same definitions as the core.
use crate::models::node_graph::*;
use luma_patterns::{self as p, ValueType};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const PREFIX: &str = "lighting/";

pub fn choices(kind: ValueType) -> Vec<ParamOption> {
    let rows: &[(&str, &str)] = match kind {
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
        ValueType::Number => PortType::Signal,
        ValueType::Color => PortType::Signal,
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
        p::Value::Color(rgb) => {
            json!({"r": rgb[0]*255., "g": rgb[1]*255., "b": rgb[2]*255., "a": 1.})
        }
        _ => serde_json::to_value(value).expect("serializable default")["value"].clone(),
    }
}

pub fn decode(kind: ValueType, value: &Value) -> Result<p::Value, String> {
    let value = match kind {
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
    p::standard_library()
        .definitions
        .into_iter()
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
            let mut inputs: Vec<_> = def
                .inputs
                .iter()
                .map(|(id, input)| PortDef {
                    id: id.clone(),
                    name: input_label(input),
                    port_type: port_type(input.value_type),
                })
                .collect();
            if def.lighting_output().is_some() {
                inputs.push(PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                });
            }
            NodeTypeDef {
                id: format!("{PREFIX}{id}"),
                name: def.name.clone(),
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
            ValueType::Mapping => PatternArgType::Mapping,
            ValueType::Boundary => PatternArgType::Boundary,
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
        PatternArgType::Boundary => ValueType::Boundary,
        PatternArgType::Boolean => ValueType::Boolean,
        _ => return Vec::new(),
    })
}
