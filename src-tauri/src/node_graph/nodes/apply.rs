use super::*;

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    let context = ctx.graph_context;
    match node.type_id.as_str() {
        "apply_dimmer" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let signal_edge = input_edges.iter().find(|e| e.to_port == "signal");

            if let (Some(sel_e), Some(sig_e)) = (selection_edge, signal_edge) {
                if let (Some(selections), Some(signal)) = (
                    state
                        .selections
                        .get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                    state
                        .signal_outputs
                        .get(&(sig_e.from_node.clone(), sig_e.from_port.clone())),
                ) {
                    let mut primitives = Vec::new();
                    let mut global_idx = 0;

                    for selection in selections {
                        for item in &selection.items {
                            // Broadcast N: get corresponding row from signal
                            let sig_idx = if signal.n <= 1 {
                                0
                            } else {
                                global_idx % signal.n
                            };

                            let mut samples = Vec::new();

                            // Broadcast T:
                            if signal.t == 1 {
                                // Constant over time -> create 2 points at start/end
                                let flat_idx = sig_idx * (signal.t * signal.c) + 0; // t=0, c=0
                                let val = signal.data.get(flat_idx).copied().unwrap_or(0.0);

                                samples.push(SeriesSample {
                                    time: context.start_time,
                                    values: vec![val],
                                    label: None,
                                });
                                samples.push(SeriesSample {
                                    time: context.end_time,
                                    values: vec![val],
                                    label: None,
                                });
                            } else {
                                // Animated -> Map T samples to duration
                                let duration = (context.end_time - context.start_time).max(0.001);
                                for t in 0..signal.t {
                                    let flat_idx =
                                        sig_idx * (signal.t * signal.c) + t * signal.c + 0;
                                    let val = signal.data.get(flat_idx).copied().unwrap_or(0.0);

                                    let time = context.start_time
                                        + (t as f32 / (signal.t - 1).max(1) as f32) * duration;
                                    samples.push(SeriesSample {
                                        time,
                                        values: vec![val],
                                        label: None,
                                    });
                                }
                            }

                            primitives.push(PrimitiveTimeSeries {
                                primitive_id: item.id.clone(),
                                color: None,
                                dimmer: Some(Series {
                                    dim: 1,
                                    labels: None,
                                    samples,
                                }),
                                position: None,
                                strobe: None,
                                speed: None,
                            });

                            global_idx += 1;
                        }
                    }

                    state.apply_outputs.push(LayerTimeSeries { primitives });
                }
            }
            Ok(true)
        }
        "apply_color" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let signal_edge = input_edges.iter().find(|e| e.to_port == "signal");

            if let (Some(sel_e), Some(sig_e)) = (selection_edge, signal_edge) {
                if let (Some(selections), Some(signal)) = (
                    state
                        .selections
                        .get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                    state
                        .signal_outputs
                        .get(&(sig_e.from_node.clone(), sig_e.from_port.clone())),
                ) {
                    let mut primitives = Vec::new();
                    let mut global_idx = 0;

                    for selection in selections {
                        for item in &selection.items {
                            // Broadcast N
                            let sig_idx = if signal.n <= 1 {
                                0
                            } else {
                                global_idx % signal.n
                            };

                            let mut color_samples = Vec::new();
                            let mut dimmer_samples = Vec::new();

                            // Broadcast T
                            if signal.t == 1 {
                                // Constant color -> two points spanning the window
                                let base = sig_idx * (signal.t * signal.c);
                                let r = signal
                                    .data
                                    .get(base)
                                    .copied()
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1.0);
                                let g = signal
                                    .data
                                    .get(base + 1)
                                    .copied()
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1.0);
                                let b = signal
                                    .data
                                    .get(base + 2)
                                    .copied()
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1.0);
                                let a = if signal.c >= 4 {
                                    signal
                                        .data
                                        .get(base + 3)
                                        .copied()
                                        .unwrap_or(1.0)
                                        .clamp(0.0, 1.0)
                                } else {
                                    1.0
                                };

                                // Derive dimmer from HSV Value (brightness)
                                let v = r.max(g.max(b));
                                let (nr, ng, nb, dim) = if v > 1e-5 {
                                    (r / v, g / v, b / v, v)
                                } else {
                                    (0.0, 0.0, 0.0, 0.0)
                                };

                                for time in [context.start_time, context.end_time] {
                                    color_samples.push(SeriesSample {
                                        time,
                                        values: vec![nr, ng, nb, a],
                                        label: None,
                                    });
                                    dimmer_samples.push(SeriesSample {
                                        time,
                                        values: vec![dim],
                                        label: None,
                                    });
                                }
                            } else {
                                // Animated color -> map samples across duration
                                let duration = (context.end_time - context.start_time).max(0.001);
                                for t in 0..signal.t {
                                    let base = sig_idx * (signal.t * signal.c) + t * signal.c;
                                    let r = signal
                                        .data
                                        .get(base)
                                        .copied()
                                        .unwrap_or(0.0)
                                        .clamp(0.0, 1.0);
                                    let g = signal
                                        .data
                                        .get(base + 1)
                                        .copied()
                                        .unwrap_or(0.0)
                                        .clamp(0.0, 1.0);
                                    let b = signal
                                        .data
                                        .get(base + 2)
                                        .copied()
                                        .unwrap_or(0.0)
                                        .clamp(0.0, 1.0);
                                    let a = if signal.c >= 4 {
                                        signal
                                            .data
                                            .get(base + 3)
                                            .copied()
                                            .unwrap_or(1.0)
                                            .clamp(0.0, 1.0)
                                    } else {
                                        1.0
                                    };

                                    // Derive dimmer from HSV Value (brightness)
                                    let v = r.max(g.max(b));
                                    let (nr, ng, nb, dim) = if v > 1e-5 {
                                        (r / v, g / v, b / v, v)
                                    } else {
                                        (0.0, 0.0, 0.0, 0.0)
                                    };

                                    let time = context.start_time
                                        + (t as f32 / (signal.t - 1).max(1) as f32) * duration;
                                    color_samples.push(SeriesSample {
                                        time,
                                        values: vec![nr, ng, nb, a],
                                        label: None,
                                    });
                                    dimmer_samples.push(SeriesSample {
                                        time,
                                        values: vec![dim],
                                        label: None,
                                    });
                                }
                            }

                            primitives.push(PrimitiveTimeSeries {
                                primitive_id: item.id.clone(),
                                color: Some(Series {
                                    dim: 4,
                                    labels: None,
                                    samples: color_samples,
                                }),
                                dimmer: Some(Series {
                                    dim: 1,
                                    labels: None,
                                    samples: dimmer_samples,
                                }),
                                position: None,
                                strobe: None,
                                speed: None,
                            });

                            global_idx += 1;
                        }
                    }

                    state.apply_outputs.push(LayerTimeSeries { primitives });
                }
            }
            Ok(true)
        }
        "apply_strobe" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let signal_edge = input_edges.iter().find(|e| e.to_port == "signal");

            if let (Some(sel_e), Some(sig_e)) = (selection_edge, signal_edge) {
                if let (Some(selections), Some(signal)) = (
                    state
                        .selections
                        .get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                    state
                        .signal_outputs
                        .get(&(sig_e.from_node.clone(), sig_e.from_port.clone())),
                ) {
                    let mut primitives = Vec::new();
                    let mut global_idx = 0;

                    for selection in selections {
                        for item in &selection.items {
                            // Broadcast N
                            let sig_idx = if signal.n <= 1 {
                                0
                            } else {
                                global_idx % signal.n
                            };

                            let mut samples = Vec::new();

                            // Broadcast T
                            if signal.t == 1 {
                                // Constant -> 2 points
                                let flat_idx_base = sig_idx * (signal.t * signal.c) + 0;
                                let val = signal
                                    .data
                                    .get(flat_idx_base)
                                    .copied()
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1.0);

                                samples.push(SeriesSample {
                                    time: context.start_time,
                                    values: vec![val],
                                    label: None,
                                });
                                samples.push(SeriesSample {
                                    time: context.end_time,
                                    values: vec![val],
                                    label: None,
                                });
                            } else {
                                // Animated -> Map
                                let duration = (context.end_time - context.start_time).max(0.001);
                                for t in 0..signal.t {
                                    let flat_idx_base =
                                        sig_idx * (signal.t * signal.c) + t * signal.c;
                                    let val = signal
                                        .data
                                        .get(flat_idx_base)
                                        .copied()
                                        .unwrap_or(0.0)
                                        .clamp(0.0, 1.0);

                                    let time = context.start_time
                                        + (t as f32 / (signal.t - 1).max(1) as f32) * duration;
                                    samples.push(SeriesSample {
                                        time,
                                        values: vec![val],
                                        label: None,
                                    });
                                }
                            }

                            primitives.push(PrimitiveTimeSeries {
                                primitive_id: item.id.clone(),
                                color: None,
                                dimmer: None,
                                position: None,
                                strobe: Some(Series {
                                    dim: 1,
                                    labels: None,
                                    samples,
                                }),
                                speed: None,
                            });

                            global_idx += 1;
                        }
                    }

                    state.apply_outputs.push(LayerTimeSeries { primitives });
                }
            }
            Ok(true)
        }
        "apply_position" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let pan_edge = input_edges.iter().find(|e| e.to_port == "pan");
            let tilt_edge = input_edges.iter().find(|e| e.to_port == "tilt");

            let Some(sel_e) = selection_edge else {
                return Ok(true);
            };
            let Some(selections) = state
                .selections
                .get(&(sel_e.from_node.clone(), sel_e.from_port.clone()))
            else {
                return Ok(true);
            };

            // Pan and/or tilt may be disconnected; treat missing axis as "hold" by writing NaN.
            let pan_signal = pan_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let tilt_signal = tilt_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            if pan_signal.is_none() && tilt_signal.is_none() {
                return Ok(true);
            }

            let t_steps = pan_signal
                .map(|s| s.t)
                .unwrap_or(1)
                .max(tilt_signal.map(|s| s.t).unwrap_or(1))
                .max(1);
            let duration = (context.end_time - context.start_time).max(0.001);

            let mut primitives = Vec::new();
            let mut global_idx = 0;

            for selection in selections {
                for item in &selection.items {
                    let (pan_n, pan_t_max) = if let Some(pan) = pan_signal {
                        (if pan.n <= 1 { 0 } else { global_idx % pan.n }, pan.t)
                    } else {
                        (0, 1)
                    };
                    let (tilt_n, tilt_t_max) = if let Some(tilt) = tilt_signal {
                        (if tilt.n <= 1 { 0 } else { global_idx % tilt.n }, tilt.t)
                    } else {
                        (0, 1)
                    };

                    let mut samples = Vec::new();
                    for t in 0..t_steps {
                        let time = if t_steps == 1 {
                            context.start_time
                        } else {
                            context.start_time + (t as f32 / (t_steps - 1) as f32) * duration
                        };

                        let pan_val = if let Some(pan) = pan_signal {
                            let pan_t = if pan_t_max == 1 {
                                0
                            } else {
                                ((t as f32 / (t_steps - 1).max(1) as f32) * (pan_t_max - 1) as f32)
                                    .round() as usize
                            };
                            let pan_idx = pan_n * (pan.t * pan.c) + pan_t * pan.c;
                            pan.data.get(pan_idx).copied().unwrap_or(0.0)
                        } else {
                            f32::NAN
                        };

                        let tilt_val = if let Some(tilt) = tilt_signal {
                            let tilt_t = if tilt_t_max == 1 {
                                0
                            } else {
                                ((t as f32 / (t_steps - 1).max(1) as f32) * (tilt_t_max - 1) as f32)
                                    .round() as usize
                            };
                            let tilt_idx = tilt_n * (tilt.t * tilt.c) + tilt_t * tilt.c;
                            tilt.data.get(tilt_idx).copied().unwrap_or(0.0)
                        } else {
                            f32::NAN
                        };

                        samples.push(SeriesSample {
                            time,
                            values: vec![pan_val, tilt_val],
                            label: None,
                        });
                    }

                    primitives.push(PrimitiveTimeSeries {
                        primitive_id: item.id.clone(),
                        color: None,
                        dimmer: None,
                        position: Some(Series {
                            dim: 2,
                            labels: None,
                            samples,
                        }),
                        strobe: None,
                        speed: None,
                    });

                    global_idx += 1;
                }
            }

            state.apply_outputs.push(LayerTimeSeries { primitives });
            Ok(true)
        }
        "apply_speed" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let speed_edge = input_edges.iter().find(|e| e.to_port == "speed");

            if let (Some(sel_e), Some(spd_e)) = (selection_edge, speed_edge) {
                if let (Some(selections), Some(signal)) = (
                    state
                        .selections
                        .get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                    state
                        .signal_outputs
                        .get(&(spd_e.from_node.clone(), spd_e.from_port.clone())),
                ) {
                    let mut primitives = Vec::new();
                    let duration = (context.end_time - context.start_time).max(0.001);
                    let mut global_idx = 0;

                    for selection in selections {
                        for item in &selection.items {
                            let sig_idx = if signal.n <= 1 {
                                0
                            } else {
                                global_idx % signal.n
                            };
                            let mut samples = Vec::new();

                            if signal.t == 1 {
                                let flat_idx_base = sig_idx * (signal.t * signal.c);
                                let val = signal.data.get(flat_idx_base).copied().unwrap_or(1.0);
                                // Binary: 0 = frozen, 1 = fast
                                let speed_val = if val > 0.5 { 1.0 } else { 0.0 };
                                samples.push(SeriesSample {
                                    time: context.start_time,
                                    values: vec![speed_val],
                                    label: None,
                                });
                                samples.push(SeriesSample {
                                    time: context.end_time,
                                    values: vec![speed_val],
                                    label: None,
                                });
                            } else {
                                for t in 0..signal.t {
                                    let time = if signal.t == 1 {
                                        context.start_time
                                    } else {
                                        context.start_time
                                            + (t as f32 / (signal.t - 1) as f32) * duration
                                    };
                                    let flat_idx = sig_idx * (signal.t * signal.c) + t * signal.c;
                                    let val = signal.data.get(flat_idx).copied().unwrap_or(1.0);
                                    // Binary: 0 = frozen, 1 = fast
                                    let speed_val = if val > 0.5 { 1.0 } else { 0.0 };
                                    samples.push(SeriesSample {
                                        time,
                                        values: vec![speed_val],
                                        label: None,
                                    });
                                }
                            }

                            primitives.push(PrimitiveTimeSeries {
                                primitive_id: item.id.clone(),
                                color: None,
                                dimmer: None,
                                position: None,
                                strobe: None,
                                speed: Some(Series {
                                    dim: 1,
                                    labels: None,
                                    samples,
                                }),
                            });

                            global_idx += 1;
                        }
                    }

                    state.apply_outputs.push(LayerTimeSeries { primitives });
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        // NOTE: apply_dimmer runtime handler is kept for backward compat (see run_node above),
        // but it is no longer offered in the node palette. Users control brightness via Apply Color.
        NodeTypeDef {
            id: "apply_color".into(),
            name: "Apply Color".into(),
            description: Some("Applies RGB(A) signal to selected primitives.".into()),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "signal".into(),
                    name: "Signal (4ch)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![], // No output wire, contributes to Layer
            params: vec![],
        },
        NodeTypeDef {
            id: "apply_strobe".into(),
            name: "Apply Strobe".into(),
            description: Some("Applies a strobe signal to selected primitives.".into()),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "signal".into(),
                    name: "Signal (1ch)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![], // No output wire, contributes to Layer
            params: vec![],
        },
        NodeTypeDef {
            id: "apply_position".into(),
            name: "Apply Position".into(),
            description: Some(
                "Applies pan/tilt (degrees) to selected primitives. Degrees are signed and centered at 0."
                    .into(),
            ),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "pan".into(),
                    name: "Pan (deg)".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "tilt".into(),
                    name: "Tilt (deg)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "apply_speed".into(),
            name: "Apply Speed".into(),
            description: Some(
                "Applies movement speed to selected primitives. 0 = frozen, 1 = fast (binary)."
                    .into(),
            ),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "speed".into(),
                    name: "Speed".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![],
            params: vec![],
        },
    ]
}
