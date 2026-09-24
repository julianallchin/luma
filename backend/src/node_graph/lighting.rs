//! Native authoring projection of the typed lighting catalogue. The graph file
//! retains canvas positions; evaluation uses the same definitions as the core.
use crate::models::node_graph::*;
use luma_patterns::{self as p, ValueType};
use serde_json::{json, Value};

pub const PREFIX: &str = "lighting/";

#[cfg(test)]
mod mapping_tests {
    use super::*;

    #[test]
    fn authoring_projection_preserves_structured_mapping_values() {
        for source in [
            p::MappingSource::MajorAxis {
                toward: [1., -2., 3.],
            },
            p::MappingSource::Vector {
                direction: [1., 0., 2.],
            },
        ] {
            let value = p::Value::Mapping(p::MappingSpec {
                span: Default::default(),
                plane: None,
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
        ValueType::Vector => PortType::Signal,
        ValueType::Choice => PortType::Choice,
        // Clip sources; they drive numerical values.
        ValueType::Time | ValueType::Hit | ValueType::Noise | ValueType::Audio => PortType::Signal,
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
        // A clip source keeps its tag, so the wire says which source it is.
        _ if value.source_kind().is_some() => {
            serde_json::to_value(value).expect("serializable source")
        }
        _ => serde_json::to_value(value).expect("serializable default")["value"].clone(),
    }
}

/// A tagged clip source on the wire, as [`wire_value`] writes it.
fn is_source(value: &Value) -> bool {
    value.get("value").is_some()
        && matches!(
            value.get("type").and_then(Value::as_str),
            Some("time" | "hit" | "noise" | "audio")
        )
}

pub fn decode(kind: ValueType, value: &Value) -> Result<p::Value, String> {
    if is_source(value) {
        let decoded: p::Value =
            serde_json::from_value(value.clone()).map_err(|e| format!("Invalid source: {e}"))?;
        decoded.validate().map_err(|e| e.to_string())?;
        return Ok(decoded);
    }
    if let ValueType::Signal(spec) = kind {
        let vector = matches!(spec.channels, Some(p::Channels::Components(n)) if n.get() == 3);
        let decoded = if value.get("values").is_some() {
            p::Value::Signal(
                serde_json::from_value(value.clone())
                    .map_err(|e| format!("Invalid signal: {e}"))?,
            )
        } else if vector && value.is_array() {
            p::Value::Vector(
                serde_json::from_value(value.clone())
                    .map_err(|e| format!("A vector needs U, V and Z: {e}"))?,
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
                        .unwrap_or_else(|| color_alpha(&stop["color"]));
                    Ok(json!({"t":stop["t"], "color":color, "alpha":alpha}))
                })
                .collect::<Result<Vec<_>, String>>()?;
            json!({"stops":stops})
        }
        ValueType::Mapping if value.is_string() => {
            let source = p::MappingSource::from_key(value.as_str().unwrap())
                .map_err(|error| error.to_string())?;
            return Ok(p::Value::Mapping(p::MappingSpec {
                span: Default::default(),
                plane: None,
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

pub fn input_node_id(key: &str) -> String {
    format!("$input/{key}")
}
/// The card a graph's named outputs are wired into. A pattern ends at its
/// Apply node instead.
pub const OUTPUTS_NODE: &str = "$outputs";

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
        ValueType::Choice => PatternArgType::Choice,
        _ => return None,
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

/// The alpha of a stop color: the `aa` byte of `"#rrggbbaa"` or the `a` field
/// (0–1) of an `{r,g,b,a}` object. Opaque when neither says otherwise.
fn color_alpha(color: &Value) -> f64 {
    if let Some(hex) = color.as_str() {
        let hex = hex.trim_start_matches('#');
        return match hex.get(6..8) {
            Some(byte) if hex.is_ascii() => {
                f64::from(u8::from_str_radix(byte, 16).unwrap_or(0)) / 255.
            }
            _ => 1.,
        };
    }
    color.get("a").and_then(Value::as_f64).unwrap_or(1.)
}
