use super::*;

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    let compute_visualizations = ctx.compute_visualizations;
    match node.type_id.as_str() {
        "gradient" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();

            let signal_edge = input_edges.iter().find(|e| e.to_port == "in");
            let start_color_edge = input_edges.iter().find(|e| e.to_port == "start_color");
            let end_color_edge = input_edges.iter().find(|e| e.to_port == "end_color");

            let Some(signal_edge) = signal_edge else {
                return Ok(true);
            };
            let signal = state
                .signal_outputs
                .get(&(signal_edge.from_node.clone(), signal_edge.from_port.clone()));

            // If input signal is missing, skip
            let Some(signal) = signal else {
                return Ok(true);
            };

            // Get start color from connected edge or params
            let start_color = if let Some(edge) = start_color_edge {
                state
                    .signal_outputs
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .map(|s| {
                        // Extract RGBA from signal (expects c=4)
                        let r = s.data.first().copied().unwrap_or(0.0);
                        let g = s.data.get(1).copied().unwrap_or(0.0);
                        let b = s.data.get(2).copied().unwrap_or(0.0);
                        let a = s.data.get(3).copied().unwrap_or(1.0);
                        (r, g, b, a)
                    })
                    .unwrap_or((0.0, 0.0, 0.0, 1.0))
            } else {
                // Parse from param (hex color string)
                let hex = node
                    .params
                    .get("start_color")
                    .and_then(|v| v.as_str())
                    .unwrap_or("#000000");
                crate::node_graph::context::parse_hex_color(hex)
            };

            // Get end color from connected edge or params
            let end_color = if let Some(edge) = end_color_edge {
                state
                    .signal_outputs
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .map(|s| {
                        // Extract RGBA from signal (expects c=4)
                        let r = s.data.first().copied().unwrap_or(1.0);
                        let g = s.data.get(1).copied().unwrap_or(1.0);
                        let b = s.data.get(2).copied().unwrap_or(1.0);
                        let a = s.data.get(3).copied().unwrap_or(1.0);
                        (r, g, b, a)
                    })
                    .unwrap_or((1.0, 1.0, 1.0, 1.0))
            } else {
                // Parse from param (hex color string)
                let hex = node
                    .params
                    .get("end_color")
                    .and_then(|v| v.as_str())
                    .unwrap_or("#ffffff");
                crate::node_graph::context::parse_hex_color(hex)
            };

            let mut data = Vec::with_capacity(signal.n * signal.t * 4);

            // Process each sample - interpolate between start and end color
            // Input signal might have c > 1, take 1st channel as the mix factor
            for chunk in signal.data.chunks(signal.c) {
                let mix = chunk.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);

                // Linear interpolation between start and end colors
                let r = start_color.0 + (end_color.0 - start_color.0) * mix;
                let g = start_color.1 + (end_color.1 - start_color.1) * mix;
                let b = start_color.2 + (end_color.2 - start_color.2) * mix;
                let a = start_color.3 + (end_color.3 - start_color.3) * mix;

                data.push(r);
                data.push(g);
                data.push(b);
                data.push(a);
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
            Ok(true)
        }
        "chroma_palette" => {
            let chroma_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|edge| edge.to_port == "chroma"))
                .ok_or_else(|| format!("Chroma Palette node '{}' missing chroma input", node.id))?;

            if let Some(chroma_sig) = state
                .signal_outputs
                .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()))
            {
                if chroma_sig.c != 12 {
                    eprintln!("[chroma_palette] Input signal is not 12-channel chroma");
                    return Ok(true);
                }

                // Define palettes (Simple Rainbow for now)
                // C, C#, D, D#, E, F, F#, G, G#, A, A#, B
                let rainbow: [[f32; 3]; 12] = [
                    [1.0, 0.0, 0.0], // C: Red
                    [1.0, 0.5, 0.0], // C#: Orange-Red
                    [1.0, 0.8, 0.0], // D: Orange
                    [1.0, 1.0, 0.0], // D#: Yellow
                    [0.5, 1.0, 0.0], // E: Lime
                    [0.0, 1.0, 0.0], // F: Green
                    [0.0, 1.0, 0.5], // F#: Mint
                    [0.0, 1.0, 1.0], // G: Cyan
                    [0.0, 0.5, 1.0], // G#: Azure
                    [0.0, 0.0, 1.0], // A: Blue
                    [0.5, 0.0, 1.0], // A#: Purple
                    [1.0, 0.0, 0.5], // B: Magenta
                ];

                let mut out_data = vec![0.0; chroma_sig.t * 3];

                for t in 0..chroma_sig.t {
                    let mut r_sum = 0.0;
                    let mut g_sum = 0.0;
                    let mut b_sum = 0.0;

                    for c in 0..12 {
                        let prob = chroma_sig.data[t * 12 + c];
                        r_sum += prob * rainbow[c][0];
                        g_sum += prob * rainbow[c][1];
                        b_sum += prob * rainbow[c][2];
                    }

                    // Boost saturation slightly since averaging desaturates
                    let max_val = r_sum.max(g_sum).max(b_sum).max(0.001);
                    let scale = 1.0 / max_val; // Auto-gain

                    out_data[t * 3 + 0] = (r_sum * scale).clamp(0.0, 1.0);
                    out_data[t * 3 + 1] = (g_sum * scale).clamp(0.0, 1.0);
                    out_data[t * 3 + 2] = (b_sum * scale).clamp(0.0, 1.0);
                }

                state.signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: 1,
                        t: chroma_sig.t,
                        c: 3,
                        data: out_data,
                    },
                );
            }
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

            // Need both signals
            let in_sig_opt = state
                .signal_outputs
                .get(&(in_edge.from_node.clone(), in_edge.from_port.clone()));
            let chroma_sig_opt = state
                .signal_outputs
                .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()));

            if let (Some(in_sig), Some(chroma_sig)) = (in_sig_opt, chroma_sig_opt) {
                // Match lengths (simple resampling/clamping to min length)
                let len = in_sig.t.min(chroma_sig.t);
                let mut out_data = vec![0.0; len * 3];

                for t in 0..len {
                    // 1. Get input RGB
                    let r = in_sig.data.get(t * in_sig.c + 0).copied().unwrap_or(0.0);
                    let g = in_sig.data.get(t * in_sig.c + 1).copied().unwrap_or(0.0);
                    let b = in_sig.data.get(t * in_sig.c + 2).copied().unwrap_or(0.0);

                    // 2. Determine shift amount from dominant chroma
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

                    // 3. RGB -> HSL
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
                        h /= 6.0; // 0..1
                    }

                    // 4. Apply Shift
                    h = (h + hue_shift_deg / 360.0).fract();
                    if h < 0.0 {
                        h += 1.0;
                    }

                    // 5. HSL -> RGB
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
                        return p;
                    }

                    let r_out = hue_to_rgb(p, q, h + 1.0 / 3.0);
                    let g_out = hue_to_rgb(p, q, h);
                    let b_out = hue_to_rgb(p, q, h - 1.0 / 3.0);

                    out_data[t * 3 + 0] = r_out;
                    out_data[t * 3 + 1] = g_out;
                    out_data[t * 3 + 2] = b_out;
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
        "color" => {
            let color_json = node
                .params
                .get("color")
                .and_then(|v| v.as_str())
                .unwrap_or(r#"{"r":255,"g":0,"b":0}"#);
            // Simple parsing, assuming r,g,b keys exist and are 0-255.
            // We normalize to 0.0-1.0 floats.

            // Extremely naive JSON parse for the specific struct structure,
            // or use serde_json value if we want robustness.
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

            // Keep string output for legacy view if needed, but port type is Signal now.
            state
                .color_outputs
                .insert((node.id.clone(), "out".into()), color_json.to_string());
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "gradient".into(),
            name: "Gradient".into(),
            description: Some(
                "Interpolates between start and end colors based on a signal (0..1).".into(),
            ),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Signal".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "start_color".into(),
                    name: "Start Color".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "end_color".into(),
                    name: "End Color".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "start_color".into(),
                    name: "Start Color".into(),
                    param_type: ParamType::Text,
                    default_number: None,
                    default_text: Some("#000000".into()),
                },
                ParamDef {
                    id: "end_color".into(),
                    name: "End Color".into(),
                    param_type: ParamType::Text,
                    default_number: None,
                    default_text: Some("#ffffff".into()),
                },
            ],
        },
        NodeTypeDef {
            id: "chroma_palette".into(),
            name: "Harmonic Palette".into(),
            description: Some("Maps the 12 chroma pitches to colors.".into()),
            category: Some("Color".into()),
            inputs: vec![PortDef {
                id: "chroma".into(),
                name: "Chroma".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "palette".into(),
                name: "Palette JSON".into(),
                param_type: ParamType::Text,
                default_text: Some("Rainbow".into()),
                default_number: None,
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
            id: "color".into(),
            name: "Color".into(),
            description: Some("Outputs a constant RGB signal.".into()),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(), // Changed from Color to Signal
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
