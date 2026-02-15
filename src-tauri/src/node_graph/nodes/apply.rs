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
                                let ch = |i: usize| base + i.min(signal.c - 1);
                                let r = signal
                                    .data
                                    .get(ch(0))
                                    .copied()
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1.0);
                                let g = signal
                                    .data
                                    .get(ch(1))
                                    .copied()
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1.0);
                                let b = signal
                                    .data
                                    .get(ch(2))
                                    .copied()
                                    .unwrap_or(0.0)
                                    .clamp(0.0, 1.0);
                                let a = if signal.c >= 4 {
                                    signal
                                        .data
                                        .get(ch(3))
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
                                    let ch = |i: usize| base + i.min(signal.c - 1);
                                    let r = signal
                                        .data
                                        .get(ch(0))
                                        .copied()
                                        .unwrap_or(0.0)
                                        .clamp(0.0, 1.0);
                                    let g = signal
                                        .data
                                        .get(ch(1))
                                        .copied()
                                        .unwrap_or(0.0)
                                        .clamp(0.0, 1.0);
                                    let b = signal
                                        .data
                                        .get(ch(2))
                                        .copied()
                                        .unwrap_or(0.0)
                                        .clamp(0.0, 1.0);
                                    let a = if signal.c >= 4 {
                                        signal
                                            .data
                                            .get(ch(3))
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
        "apply_movement" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let uv_edge = input_edges.iter().find(|e| e.to_port == "uv");

            let Some(sel_e) = selection_edge else {
                return Ok(true);
            };
            let Some(selections) = state
                .selections
                .get(&(sel_e.from_node.clone(), sel_e.from_port.clone()))
            else {
                return Ok(true);
            };

            let uv_signal = uv_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            let Some(uv_signal) = uv_signal else {
                return Ok(true);
            };

            // Build fixture→MovementConfig mapping from database
            use crate::models::groups::MovementConfig;
            use std::collections::HashMap;

            let default_config = MovementConfig::default();
            let mut config_by_fixture: HashMap<String, MovementConfig> = HashMap::new();
            let mut rot_by_fixture: HashMap<String, (f32, f32, f32)> = HashMap::new();

            if let Some(proj_pool) = ctx.project_pool {
                // Get all fixtures for the venue (for rotation data)
                let fixtures = crate::database::local::fixtures::get_fixtures_for_venue(
                    proj_pool,
                    ctx.graph_context.venue_id,
                )
                .await
                .map_err(|e| format!("apply_movement: failed to fetch fixtures: {}", e))?;

                for fx in &fixtures {
                    // Match Y/Z swap from selection.rs ("legacy UI mapping")
                    rot_by_fixture.insert(
                        fx.id.clone(),
                        (fx.rot_x as f32, fx.rot_z as f32, fx.rot_y as f32),
                    );
                }

                // Get all groups for the venue
                let groups = crate::database::local::groups::list_groups(
                    proj_pool,
                    ctx.graph_context.venue_id,
                )
                .await
                .map_err(|e| format!("apply_movement: failed to fetch groups: {}", e))?;

                // Build fixture→group config mapping
                for group in &groups {
                    if let Some(ref config) = group.movement_config {
                        let members = crate::database::local::groups::get_fixtures_in_group(
                            proj_pool, group.id,
                        )
                        .await
                        .unwrap_or_default();

                        for member in members {
                            config_by_fixture.insert(member.id.clone(), config.clone());
                        }
                    }
                }
            }

            let t_steps = uv_signal.t.max(1);
            let duration = (context.end_time - context.start_time).max(0.001);

            // world_to_local: transform a world-space direction into fixture-local space.
            // Forward transform (in selection.rs): v_world = Rx(Ry(Rz(v_local)))
            // Inverse: v_local = Rz^-1(Ry^-1(Rx^-1(v_world)))
            // Apply Rx^-1 first, then Ry^-1, then Rz^-1
            let world_to_local =
                |v: (f32, f32, f32), rx: f32, ry: f32, rz: f32| -> (f32, f32, f32) {
                    let (mut x, mut y, mut z) = v;
                    // Rx^-1
                    let (cx, sx) = (rx.cos(), rx.sin());
                    let (y1, z1) = (y * cx + z * sx, -y * sx + z * cx);
                    y = y1;
                    z = z1;
                    // Ry^-1
                    let (cy, sy) = (ry.cos(), ry.sin());
                    let (x2, z2) = (x * cy + z * sy, -x * sy + z * cy);
                    x = x2;
                    z = z2;
                    // Rz^-1
                    let (cz, sz) = (rz.cos(), rz.sin());
                    let (x3, y3) = (x * cz + y * sz, -x * sz + y * cz);
                    x = x3;
                    y = y3;
                    (x, y, z)
                };

            let mut primitives = Vec::new();
            let mut global_idx = 0;

            for selection in selections {
                for item in &selection.items {
                    let config = config_by_fixture
                        .get(&item.fixture_id)
                        .unwrap_or(&default_config);
                    let (rx, ry, rz) = rot_by_fixture
                        .get(&item.fixture_id)
                        .copied()
                        .unwrap_or((0.0, 0.0, 0.0));

                    // Build movement basis exactly like the visualizer pyramid:
                    // base direction + UV tangent axes rotated by uv_rotation.
                    let base = (config.base_dir_x, config.base_dir_y, config.base_dir_z);
                    let base_len = (base.0 * base.0 + base.1 * base.1 + base.2 * base.2).sqrt();
                    let base_dir = if base_len > 1e-9 {
                        (base.0 / base_len, base.1 / base_len, base.2 / base_len)
                    } else {
                        (0.0, 0.0, -1.0)
                    };

                    let cross = |a: (f32, f32, f32), b: (f32, f32, f32)| -> (f32, f32, f32) {
                        (
                            a.1 * b.2 - a.2 * b.1,
                            a.2 * b.0 - a.0 * b.2,
                            a.0 * b.1 - a.1 * b.0,
                        )
                    };
                    let normalize = |v: (f32, f32, f32)| -> (f32, f32, f32) {
                        let len = (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt();
                        if len > 1e-9 {
                            (v.0 / len, v.1 / len, v.2 / len)
                        } else {
                            (0.0, 0.0, 0.0)
                        }
                    };

                    // Data-space up axis is +Z.
                    let world_up = (0.0f32, 0.0f32, 1.0f32);
                    let world_forward = (0.0f32, 1.0f32, 0.0f32);
                    let mut axis_u = cross(
                        (base_dir.0 as f32, base_dir.1 as f32, base_dir.2 as f32),
                        world_up,
                    );
                    let axis_u_len =
                        (axis_u.0 * axis_u.0 + axis_u.1 * axis_u.1 + axis_u.2 * axis_u.2).sqrt();
                    if axis_u_len < 1e-6 {
                        axis_u = cross(
                            (base_dir.0 as f32, base_dir.1 as f32, base_dir.2 as f32),
                            world_forward,
                        );
                    }
                    axis_u = normalize(axis_u);
                    let axis_v = normalize(cross(
                        (base_dir.0 as f32, base_dir.1 as f32, base_dir.2 as f32),
                        axis_u,
                    ));

                    let uv_rot = config.uv_rotation.to_radians() as f32;
                    let cos_r = uv_rot.cos();
                    let sin_r = uv_rot.sin();
                    let axis_u_rot = normalize((
                        axis_u.0 * cos_r + axis_v.0 * sin_r,
                        axis_u.1 * cos_r + axis_v.1 * sin_r,
                        axis_u.2 * cos_r + axis_v.2 * sin_r,
                    ));
                    let axis_v_rot = normalize((
                        axis_u.0 * -sin_r + axis_v.0 * cos_r,
                        axis_u.1 * -sin_r + axis_v.1 * cos_r,
                        axis_u.2 * -sin_r + axis_v.2 * cos_r,
                    ));

                    let extent_u_rad = (config.extent_u as f32).to_radians();
                    let extent_v_rad = (config.extent_v as f32).to_radians();
                    let max_angle = std::f32::consts::FRAC_PI_2 - 1e-3;

                    let sig_n = if uv_signal.n <= 1 {
                        0
                    } else {
                        global_idx % uv_signal.n
                    };

                    let mut samples = Vec::with_capacity(t_steps);
                    for t in 0..t_steps {
                        let time = if t_steps == 1 {
                            context.start_time
                        } else {
                            context.start_time + (t as f32 / (t_steps - 1) as f32) * duration
                        };

                        // Read UV from signal (C=2)
                        let uv_t = if uv_signal.t == 1 { 0 } else { t % uv_signal.t };
                        let uv_base = sig_n * (uv_signal.t * uv_signal.c) + uv_t * uv_signal.c;
                        let u_raw = uv_signal.data.get(uv_base).copied().unwrap_or(0.0);
                        let v_raw = uv_signal.data.get(uv_base + 1).copied().unwrap_or(0.0);

                        let angle_u = (u_raw * extent_u_rad).clamp(-max_angle, max_angle);
                        let angle_v = (v_raw * extent_v_rad).clamp(-max_angle, max_angle);

                        // Match the movement pyramid geometry:
                        // dir = normalize(base + U*tan(angle_u) + V*tan(angle_v))
                        let tan_u = angle_u.tan();
                        let tan_v = angle_v.tan();
                        let dir_world = normalize((
                            base_dir.0 as f32 + axis_u_rot.0 * tan_u + axis_v_rot.0 * tan_v,
                            base_dir.1 as f32 + axis_u_rot.1 * tan_u + axis_v_rot.1 * tan_v,
                            base_dir.2 as f32 + axis_u_rot.2 * tan_u + axis_v_rot.2 * tan_v,
                        ));

                        // Convert data-space direction (Z-up) to rotation-space (Y-up)
                        // by swapping Y↔Z to match the rotation convention from selection.rs
                        let dir_rot = (dir_world.0, dir_world.2, dir_world.1);
                        let (lx, ly, lz) = world_to_local(dir_rot, rx, ry, rz);

                        // Fixture rotation-space is Y-up (Three.js convention):
                        // X-right, Y-up, Z-forward.
                        // Tilt=0 points down (-Y), pan=0 points forward (+Z).
                        let horiz = (lx * lx + lz * lz).sqrt();
                        let tilt_deg = horiz.atan2(-ly).to_degrees();
                        let pan_deg = if horiz < 1e-6 {
                            0.0
                        } else {
                            lx.atan2(lz).to_degrees()
                        };

                        samples.push(SeriesSample {
                            time,
                            values: vec![pan_deg, tilt_deg],
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
        // NOTE: apply_position runtime handler is kept for backward compat (see run_node above),
        // but it is no longer offered in the node palette. Use Apply Movement instead.
        NodeTypeDef {
            id: "apply_movement".into(),
            name: "Apply Movement".into(),
            description: Some(
                "Maps UV perturbation through the group's movement pyramid to absolute pan/tilt per fixture."
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
                    id: "uv".into(),
                    name: "UV (2ch)".into(),
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
