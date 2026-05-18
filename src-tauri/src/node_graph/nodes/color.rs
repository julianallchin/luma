use super::*;
use crate::node_graph::oklab::{oklab_to_srgb, srgb_to_oklab};

/// Default rainbow used by `chroma_palette` when no Stops are wired. K=12.
const DEFAULT_CHROMA_RAINBOW_HEX: [&str; 12] = [
    "#ff0000", // C: Red
    "#ff8000", // C#
    "#ffcc00", // D
    "#ffff00", // D#
    "#80ff00", // E
    "#00ff00", // F
    "#00ff80", // F#
    "#00ffff", // G
    "#0080ff", // G#
    "#0000ff", // A
    "#8000ff", // A#
    "#ff0080", // B
];

const DEFAULT_PALETTE_JSON: &str =
    r##"{"colors":["#ff0080","#00ffc8","#ffbe28","#9d4dff","#3aff8a"]}"##;

const DEFAULT_GRADIENT_JSON: &str =
    r##"{"stops":[{"color":"#000000","t":0},{"color":"#ffffff","t":1}]}"##;

const DEFAULT_CHROMA_PALETTE_JSON: &str = r##"{"colors":["#ff0000","#ff8000","#ffcc00","#ffff00","#80ff00","#00ff00","#00ff80","#00ffff","#0080ff","#0000ff","#8000ff","#ff0080"]}"##;

/// Build a Stops value from a JSON arg/param. Handles both shapes:
///   - `{"colors": ["#hex", ...]}` → uniform-spaced stops (palette author)
///   - `{"stops": [{"color": "#hex", "t": 0.5}, ...]}` → positioned stops (gradient author)
/// If both are present, `stops` wins (gradient authoring takes priority).
pub(crate) fn stops_from_value(value: &serde_json::Value) -> Stops {
    if let Some(arr) = value.get("stops").and_then(|v| v.as_array()) {
        let mut parsed: Vec<(f32, [f32; 4])> = Vec::with_capacity(arr.len());
        for entry in arr {
            let t = entry
                .get("t")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let (r, g, b, a) = match entry.get("color") {
                Some(serde_json::Value::String(hex)) => {
                    crate::node_graph::context::parse_hex_color(hex)
                }
                Some(v) => crate::node_graph::context::parse_color_value(v),
                None => (0.0, 0.0, 0.0, 1.0),
            };
            parsed.push((t, [r, g, b, a]));
        }
        parsed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        return Stops { stops: parsed };
    }
    if let Some(arr) = value.get("colors").and_then(|v| v.as_array()) {
        let n = arr.len();
        let stops = arr
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let (r, g, b, a) = match entry {
                    serde_json::Value::String(hex) => {
                        crate::node_graph::context::parse_hex_color(hex)
                    }
                    v => crate::node_graph::context::parse_color_value(v),
                };
                let t = if n <= 1 {
                    0.0
                } else {
                    i as f32 / (n - 1) as f32
                };
                (t, [r, g, b, a])
            })
            .collect();
        return Stops { stops };
    }
    Stops::default()
}

fn read_text_param_as_json(value: Option<&serde_json::Value>) -> serde_json::Value {
    match value {
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(s).unwrap_or(serde_json::json!({}))
        }
        Some(other) => other.clone(),
        None => serde_json::json!({}),
    }
}

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    match node.type_id.as_str() {
        "palette" => {
            let value = read_text_param_as_json(node.params.get("value"));
            let stops = stops_from_value(&value);
            let stops = if stops.is_empty() {
                stops_from_value(&serde_json::from_str(DEFAULT_PALETTE_JSON).unwrap())
            } else {
                stops
            };
            state
                .stops_outputs
                .insert((node.id.clone(), "out".into()), stops);
            Ok(true)
        }
        "gradient" => {
            let value = read_text_param_as_json(node.params.get("value"));
            let stops = stops_from_value(&value);
            let stops = if stops.is_empty() {
                stops_from_value(&serde_json::from_str(DEFAULT_GRADIENT_JSON).unwrap())
            } else {
                stops
            };
            state
                .stops_outputs
                .insert((node.id.clone(), "out".into()), stops);
            Ok(true)
        }
        "sample_palette" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let stops_edge = input_edges.iter().find(|e| e.to_port == "stops");
            let u_edge = input_edges.iter().find(|e| e.to_port == "u");

            let Some(stops_edge) = stops_edge else {
                return Ok(true);
            };
            let Some(stops) = state
                .stops_outputs
                .get(&(stops_edge.from_node.clone(), stops_edge.from_port.clone()))
            else {
                return Ok(true);
            };

            // Default u: scalar 0 when unconnected.
            let default_u = Signal {
                n: 1,
                t: 1,
                c: 1,
                data: vec![0.0],
            };
            let u_signal = u_edge
                .and_then(|e| {
                    state
                        .signal_outputs
                        .get(&(e.from_node.clone(), e.from_port.clone()))
                })
                .unwrap_or(&default_u);

            let n = u_signal.n;
            let t = u_signal.t;
            let mut data = Vec::with_capacity(n * t * 4);
            for ni in 0..n {
                for ti in 0..t {
                    let idx = ni * (u_signal.t * u_signal.c) + ti * u_signal.c;
                    let u = u_signal.data.get(idx).copied().unwrap_or(0.0);
                    let rgba = stops.sample(u);
                    data.extend_from_slice(&rgba);
                }
            }
            state
                .signal_outputs
                .insert((node.id.clone(), "out".into()), Signal { n, t, c: 4, data });
            Ok(true)
        }
        "chroma_palette" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let chroma_edge = input_edges
                .iter()
                .find(|e| e.to_port == "chroma")
                .ok_or_else(|| format!("Chroma Palette node '{}' missing chroma input", node.id))?;
            let Some(chroma_sig) = state
                .signal_outputs
                .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()))
            else {
                return Ok(true);
            };
            if chroma_sig.c != 12 {
                eprintln!("[chroma_palette] Input signal is not 12-channel chroma");
                return Ok(true);
            }

            // Resolve the 12 palette colors: from a connected Stops port,
            // sampled at 12 uniform positions; or the default rainbow.
            let stops_edge = input_edges.iter().find(|e| e.to_port == "stops");
            let colors: [[f32; 4]; 12] = match stops_edge.and_then(|e| {
                state
                    .stops_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            }) {
                Some(s) if !s.is_empty() => {
                    let v = s.sample_uniform(12);
                    let mut out = [[0.0; 4]; 12];
                    for (i, c) in v.iter().enumerate() {
                        out[i] = *c;
                    }
                    out
                }
                _ => default_rainbow_colors(),
            };

            let mut out_data = vec![0.0; chroma_sig.t * 4];
            for t in 0..chroma_sig.t {
                let mut r_sum = 0.0;
                let mut g_sum = 0.0;
                let mut b_sum = 0.0;
                for c in 0..12 {
                    let prob = chroma_sig.data[t * 12 + c];
                    r_sum += prob * colors[c][0];
                    g_sum += prob * colors[c][1];
                    b_sum += prob * colors[c][2];
                }
                let max_val = r_sum.max(g_sum).max(b_sum).max(0.001);
                let scale = 1.0 / max_val;
                out_data[t * 4] = (r_sum * scale).clamp(0.0, 1.0);
                out_data[t * 4 + 1] = (g_sum * scale).clamp(0.0, 1.0);
                out_data[t * 4 + 2] = (b_sum * scale).clamp(0.0, 1.0);
                out_data[t * 4 + 3] = 1.0;
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: 1,
                    t: chroma_sig.t,
                    c: 4,
                    data: out_data,
                },
            );
            Ok(true)
        }
        "spectral_shift" => {
            let in_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|edge| edge.to_port == "in"))
                .ok_or_else(|| format!("Spectral Shift node '{}' missing 'in' input", node.id))?;
            let chroma_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|edge| edge.to_port == "chroma"))
                .ok_or_else(|| format!("Spectral Shift node '{}' missing chroma input", node.id))?;

            let in_sig_opt = state
                .signal_outputs
                .get(&(in_edge.from_node.clone(), in_edge.from_port.clone()));
            let chroma_sig_opt = state
                .signal_outputs
                .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()));

            if let (Some(in_sig), Some(chroma_sig)) = (in_sig_opt, chroma_sig_opt) {
                let len = in_sig.t.min(chroma_sig.t);
                let mut out_data = vec![0.0; len * 3];

                for t in 0..len {
                    let r = in_sig.data.get(t * in_sig.c).copied().unwrap_or(0.0);
                    let g = in_sig.data.get(t * in_sig.c + 1).copied().unwrap_or(0.0);
                    let b = in_sig.data.get(t * in_sig.c + 2).copied().unwrap_or(0.0);

                    let mut max_p = -1.0;
                    let mut dominant_idx = 0;
                    for c in 0..12 {
                        let p = chroma_sig.data[t * 12 + c];
                        if p > max_p {
                            max_p = p;
                            dominant_idx = c;
                        }
                    }
                    let hue_shift_deg = (dominant_idx as f32 / 12.0) * 360.0;

                    let max_c = r.max(g).max(b);
                    let min_c = r.min(g).min(b);
                    let delta = max_c - min_c;
                    let l = (max_c + min_c) / 2.0;
                    let mut s = 0.0;
                    let mut h = 0.0;
                    if delta > 0.00001 {
                        s = if l > 0.5 {
                            delta / (2.0 - max_c - min_c)
                        } else {
                            delta / (max_c + min_c)
                        };
                        if max_c == r {
                            h = (g - b) / delta + (if g < b { 6.0 } else { 0.0 });
                        } else if max_c == g {
                            h = (b - r) / delta + 2.0;
                        } else {
                            h = (r - g) / delta + 4.0;
                        }
                        h /= 6.0;
                    }
                    h = (h + hue_shift_deg / 360.0).fract();
                    if h < 0.0 {
                        h += 1.0;
                    }

                    let q = if l < 0.5 {
                        l * (1.0 + s)
                    } else {
                        l + s - l * s
                    };
                    let p = 2.0 * l - q;

                    fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
                        if t < 0.0 {
                            t += 1.0;
                        }
                        if t > 1.0 {
                            t -= 1.0;
                        }
                        if t < 1.0 / 6.0 {
                            return p + (q - p) * 6.0 * t;
                        }
                        if t < 1.0 / 2.0 {
                            return q;
                        }
                        if t < 2.0 / 3.0 {
                            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
                        }
                        p
                    }

                    out_data[t * 3] = hue_to_rgb(p, q, h + 1.0 / 3.0);
                    out_data[t * 3 + 1] = hue_to_rgb(p, q, h);
                    out_data[t * 3 + 2] = hue_to_rgb(p, q, h - 1.0 / 3.0);
                }

                state.signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: 1,
                        t: len,
                        c: 3,
                        data: out_data,
                    },
                );
            }
            Ok(true)
        }
        "rainbow" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let signal_edge = input_edges.iter().find(|e| e.to_port == "in");

            let offset = node
                .params
                .get("offset")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let saturation = node
                .params
                .get("saturation")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let spread = node
                .params
                .get("spread")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;

            fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
                if t < 0.0 {
                    t += 1.0;
                }
                if t > 1.0 {
                    t -= 1.0;
                }
                if t < 1.0 / 6.0 {
                    return p + (q - p) * 6.0 * t;
                }
                if t < 1.0 / 2.0 {
                    return q;
                }
                if t < 2.0 / 3.0 {
                    return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
                }
                p
            }

            fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
                if s == 0.0 {
                    return (l, l, l);
                }
                let q = if l < 0.5 {
                    l * (1.0 + s)
                } else {
                    l + s - l * s
                };
                let p = 2.0 * l - q;
                let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
                let g = hue_to_rgb(p, q, h);
                let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
                (r, g, b)
            }

            if let Some(signal_edge) = signal_edge {
                let signal = state
                    .signal_outputs
                    .get(&(signal_edge.from_node.clone(), signal_edge.from_port.clone()));
                if let Some(signal) = signal {
                    let mut data = Vec::with_capacity(signal.n * signal.t * 4);
                    for chunk in signal.data.chunks(signal.c) {
                        let v = chunk.first().copied().unwrap_or(0.0);
                        let hue = (v * spread + offset).fract();
                        let hue = if hue < 0.0 { hue + 1.0 } else { hue };
                        let (r, g, b) = hsl_to_rgb(hue, saturation.clamp(0.0, 1.0), 0.5);
                        data.push(r);
                        data.push(g);
                        data.push(b);
                        data.push(1.0);
                    }
                    state.signal_outputs.insert(
                        (node.id.clone(), "out".into()),
                        Signal {
                            n: signal.n,
                            t: signal.t,
                            c: 4,
                            data,
                        },
                    );
                }
            } else {
                let steps = PREVIEW_LENGTH;
                let mut data = Vec::with_capacity(steps * 4);
                for i in 0..steps {
                    let v = i as f32 / steps as f32;
                    let hue = (v * spread + offset).fract();
                    let hue = if hue < 0.0 { hue + 1.0 } else { hue };
                    let (r, g, b) = hsl_to_rgb(hue, saturation.clamp(0.0, 1.0), 0.5);
                    data.push(r);
                    data.push(g);
                    data.push(b);
                    data.push(1.0);
                }
                state.signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: 1,
                        t: steps,
                        c: 4,
                        data,
                    },
                );
            }
            Ok(true)
        }
        "color" => {
            let color_json = node
                .params
                .get("color")
                .and_then(|v| v.as_str())
                .unwrap_or(r#"{"r":255,"g":0,"b":0}"#);
            let parsed: serde_json::Value =
                serde_json::from_str(color_json).unwrap_or(serde_json::json!({}));
            let r = parsed.get("r").and_then(|v| v.as_f64()).unwrap_or(255.0) as f32 / 255.0;
            let g = parsed.get("g").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 / 255.0;
            let b = parsed.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 / 255.0;
            let a = parsed.get("a").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: 1,
                    t: 1,
                    c: 4,
                    data: vec![r, g, b, a],
                },
            );
            state
                .color_outputs
                .insert((node.id.clone(), "out".into()), color_json.to_string());
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn default_rainbow_colors() -> [[f32; 4]; 12] {
    let mut out = [[0.0; 4]; 12];
    for (i, hex) in DEFAULT_CHROMA_RAINBOW_HEX.iter().enumerate() {
        let (r, g, b, a) = crate::node_graph::context::parse_hex_color(hex);
        out[i] = [r, g, b, a];
    }
    out
}

#[allow(dead_code)]
fn _force_oklab_used() {
    let _ = srgb_to_oklab;
    let _ = oklab_to_srgb;
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "palette".into(),
            name: "Palette".into(),
            description: Some(
                "Ordered set of K colors emitted as uniformly-spaced color Stops. Use for discrete-feeling color sets (one color per seed, per pitch class, etc.)."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Stops".into(),
                port_type: PortType::Stops,
            }],
            params: vec![ParamDef {
                id: "value".into(),
                name: "Colors".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(DEFAULT_PALETTE_JSON.into()),
            }],
        },
        NodeTypeDef {
            id: "gradient".into(),
            name: "Gradient".into(),
            description: Some(
                "Continuous color function defined by stops at user-positioned t values. Sampled exactly at consumer time — no fixed-resolution bake."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Stops".into(),
                port_type: PortType::Stops,
            }],
            params: vec![ParamDef {
                id: "value".into(),
                name: "Stops".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(DEFAULT_GRADIENT_JSON.into()),
            }],
        },
        NodeTypeDef {
            id: "sample_palette".into(),
            name: "Sample Palette".into(),
            description: Some(
                "Samples a Stops function at scalar position u ∈ [0,1]. OKLab interpolation between bracketing stops."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "stops".into(),
                    name: "Stops".into(),
                    port_type: PortType::Stops,
                },
                PortDef {
                    id: "u".into(),
                    name: "u".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "chroma_palette".into(),
            name: "Harmonic Palette".into(),
            description: Some(
                "Maps the 12 chroma pitches to colors sampled from a Stops input (12 uniform samples). Falls back to a default rainbow."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "chroma".into(),
                    name: "Chroma".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "stops".into(),
                    name: "Stops".into(),
                    port_type: PortType::Stops,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "fallback_palette".into(),
                name: "Fallback Palette".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(DEFAULT_CHROMA_PALETTE_JSON.into()),
            }],
        },
        NodeTypeDef {
            id: "spectral_shift".into(),
            name: "Spectral Shift".into(),
            description: Some("Rotates color hue based on the dominant musical key.".into()),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Base Color".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "chroma".into(),
                    name: "Chroma".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "strength".into(),
                name: "Strength".into(),
                param_type: ParamType::Number,
                default_number: Some(1.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "rainbow".into(),
            name: "Rainbow".into(),
            description: Some(
                "Maps a signal through a full rainbow hue cycle. Without input, generates a 256-sample ramp.".into(),
            ),
            category: Some("Color".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "offset".into(),
                    name: "Offset".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "spread".into(),
                    name: "Spread".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "saturation".into(),
                    name: "Saturation".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "color".into(),
            name: "Color".into(),
            description: Some("Outputs a constant RGB signal.".into()),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "color".into(),
                name: "Color".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(r#"{"r":255,"g":0,"b":0,"a":1}"#.into()),
            }],
        },
    ]
}
