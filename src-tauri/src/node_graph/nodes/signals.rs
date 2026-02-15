use super::*;

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    let context = ctx.graph_context;
    let project_pool = ctx.project_pool;
    let resource_path_root = ctx.resource_path_root;
    match node.type_id.as_str() {
        "pattern_args" => {
            for arg in ctx.arg_defs {
                let value = ctx.arg_values.get(&arg.id).unwrap_or(&arg.default_value);

                match arg.arg_type {
                    PatternArgType::Color => {
                        let (r, g, b, a) = crate::node_graph::context::parse_color_value(value);
                        state.signal_outputs.insert(
                            (node.id.clone(), arg.id.clone()),
                            Signal {
                                n: 1,
                                t: 1,
                                c: 4,
                                data: vec![r, g, b, a],
                            },
                        );

                        let color_json = serde_json::json!({
                            "r": (r * 255.0).round() as i32,
                            "g": (g * 255.0).round() as i32,
                            "b": (b * 255.0).round() as i32,
                            "a": a,
                        })
                        .to_string();
                        state
                            .color_outputs
                            .insert((node.id.clone(), arg.id.clone()), color_json.clone());
                        state
                            .color_views
                            .insert(format!("{}:{}", node.id, arg.id), color_json);
                    }
                    PatternArgType::Scalar => {
                        let scalar_value = value.as_f64().unwrap_or(0.0) as f32;
                        state.signal_outputs.insert(
                            (node.id.clone(), arg.id.clone()),
                            Signal {
                                n: 1,
                                t: 1,
                                c: 1,
                                data: vec![scalar_value],
                            },
                        );
                    }
                    PatternArgType::Selection => {
                        // Parse the Selection arg value: { expression, spatialReference }
                        let expression = value
                            .get("expression")
                            .and_then(|v| v.as_str())
                            .unwrap_or("all");
                        let spatial_reference = value
                            .get("spatialReference")
                            .and_then(|v| v.as_str())
                            .unwrap_or("global");

                        let rng_seed = ctx.graph_context.instance_seed.unwrap_or_else(|| {
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            std::hash::Hash::hash(&node.id, &mut hasher);
                            std::hash::Hash::hash(&arg.id, &mut hasher);
                            std::hash::Hasher::finish(&hasher)
                        });

                        let selections = super::selection::build_selection_from_expression(
                            ctx,
                            expression,
                            spatial_reference,
                            rng_seed,
                        )
                        .await?;

                        state
                            .selections
                            .insert((node.id.clone(), arg.id.clone()), selections);
                    }
                }
            }
            Ok(true)
        }
        "math" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let a_edge = input_edges.iter().find(|e| e.to_port == "a");
            let b_edge = input_edges.iter().find(|e| e.to_port == "b");

            let op = node
                .params
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("add");

            let signal_a = a_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let signal_b = b_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            // Helper for default scalar signal (0.0)
            let default_sig = Signal {
                n: 1,
                t: 1,
                c: 1,
                data: vec![0.0],
            };

            let a = signal_a.unwrap_or(&default_sig);
            let b = signal_b.unwrap_or(&default_sig);

            // Determine output dimensions (Broadcasting)
            let out_n = a.n.max(b.n);
            let out_t = a.t.max(b.t);
            let out_c = a.c.max(b.c);

            let mut data = Vec::with_capacity(out_n * out_t * out_c);

            // Tensor broadcasting loop
            for i in 0..out_n {
                // Map output index i to input index (clamp to size if 1, else must match or crash/modulo)
                // Broadcasting rule: if dim is 1, repeat. If match, use index. Else undefined (we'll use modulo for safety).
                let idx_a_n = if a.n <= 1 { 0 } else { i % a.n };
                let idx_b_n = if b.n <= 1 { 0 } else { i % b.n };

                for j in 0..out_t {
                    let idx_a_t = if a.t <= 1 { 0 } else { j % a.t };
                    let idx_b_t = if b.t <= 1 { 0 } else { j % b.t };

                    for k in 0..out_c {
                        let idx_a_c = if a.c <= 1 { 0 } else { k % a.c };
                        let idx_b_c = if b.c <= 1 { 0 } else { k % b.c };

                        // Flattened index: [n * (t * c) + t * c + c]
                        let flat_a = idx_a_n * (a.t * a.c) + idx_a_t * a.c + idx_a_c;
                        let flat_b = idx_b_n * (b.t * b.c) + idx_b_t * b.c + idx_b_c;

                        let val_a = a.data.get(flat_a).copied().unwrap_or(0.0);
                        let val_b = b.data.get(flat_b).copied().unwrap_or(0.0);

                        let res = match op {
                            "add" => val_a + val_b,
                            "subtract" => val_a - val_b,
                            "multiply" => val_a * val_b,
                            "divide" => {
                                if val_b != 0.0 {
                                    val_a / val_b
                                } else {
                                    0.0
                                }
                            }
                            "max" => val_a.max(val_b),
                            "min" => val_a.min(val_b),
                            "abs_diff" => (val_a - val_b).abs(),
                            "abs" => val_a.abs(),
                            "modulo" => {
                                if val_b != 0.0 {
                                    val_a % val_b
                                } else {
                                    0.0
                                }
                            }
                            "circular_distance" => {
                                // Shortest distance between two positions on a unit circle (0..1)
                                // A = position 1, B = position 2
                                // Returns 0..0.5 (multiply by 2 to normalize to 0..1)
                                let diff = (val_a - val_b).abs() % 1.0;
                                diff.min(1.0 - diff)
                            }
                            _ => val_a + val_b,
                        };

                        data.push(res);
                    }
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: out_n,
                    t: out_t,
                    c: out_c,
                    data,
                },
            );
            Ok(true)
        }
        "round" => {
            let input_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

            let Some(input_edge) = input_edge else {
                eprintln!(
                    "[run_graph] round '{}' missing signal input; skipping",
                    node.id
                );
                return Ok(true);
            };

            let Some(signal) = state
                .signal_outputs
                .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
            else {
                eprintln!(
                    "[run_graph] round '{}' input signal unavailable; skipping",
                    node.id
                );
                return Ok(true);
            };

            let op = node
                .params
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("round");

            let mut data = Vec::with_capacity(signal.data.len());
            for &val in &signal.data {
                let res = match op {
                    "floor" => val.floor(),
                    "ceil" => val.ceil(),
                    "round" => val.round(),
                    _ => val.round(),
                };
                data.push(res);
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: signal.n,
                    t: signal.t,
                    c: signal.c,
                    data,
                },
            );
            Ok(true)
        }
        "ramp" => {
            let grid_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
            let grid = grid_edge.and_then(|edge| {
                state
                    .beat_grids
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
            });

            // Beat grid input is required
            let Some(grid) = grid else {
                return Ok(true);
            };

            let bpm = grid.bpm;

            // Determine simulation steps
            let duration = (context.end_time - context.start_time).max(0.001);
            let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
            let t_steps = t_steps.max(PREVIEW_LENGTH);

            let mut data = Vec::with_capacity(t_steps);

            for i in 0..t_steps {
                let time = context.start_time + (i as f32 / (t_steps - 1).max(1) as f32) * duration;

                // Beat position relative to pattern start (0 to n_beats)
                let time_in_pattern = time - context.start_time;
                let beat_in_pattern = time_in_pattern * (bpm / 60.0);
                data.push(beat_in_pattern);
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: 1,
                    t: t_steps,
                    c: 1,
                    data,
                },
            );
            Ok(true)
        }
        "ramp_between" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let grid_edge = input_edges.iter().find(|x| x.to_port == "grid");
            let start_edge = input_edges.iter().find(|x| x.to_port == "start");
            let end_edge = input_edges.iter().find(|x| x.to_port == "end");

            let grid = grid_edge.and_then(|edge| {
                state
                    .beat_grids
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
            });
            let start_signal = start_edge.and_then(|edge| {
                state
                    .signal_outputs
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
            });
            let end_signal = end_edge.and_then(|edge| {
                state
                    .signal_outputs
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
            });

            // All inputs are required
            let (Some(grid), Some(start_signal), Some(end_signal)) =
                (grid, start_signal, end_signal)
            else {
                return Ok(true);
            };

            let bpm = grid.bpm;

            // Determine simulation steps
            let duration = (context.end_time - context.start_time).max(0.001);
            let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
            let t_steps = t_steps.max(PREVIEW_LENGTH);
            let total_beats = (duration * (bpm / 60.0)).max(0.0001);

            let mut data = Vec::with_capacity(t_steps);
            for i in 0..t_steps {
                let time = context.start_time + (i as f32 / (t_steps - 1).max(1) as f32) * duration;

                let time_in_pattern = time - context.start_time;
                let beat_in_pattern = time_in_pattern * (bpm / 60.0);
                let progress = (beat_in_pattern / total_beats).clamp(0.0, 1.0);

                let start_idx = (i.min(start_signal.data.len().saturating_sub(1))) as usize;
                let end_idx = (i.min(end_signal.data.len().saturating_sub(1))) as usize;
                let start_val = start_signal.data.get(start_idx).copied().unwrap_or(0.0);
                let end_val = end_signal.data.get(end_idx).copied().unwrap_or(0.0);

                data.push(start_val + (end_val - start_val) * progress);
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: 1,
                    t: t_steps,
                    c: 1,
                    data,
                },
            );
            Ok(true)
        }
        "threshold" => {
            let input_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

            let Some(input_edge) = input_edge else {
                eprintln!(
                    "[run_graph] threshold '{}' missing signal input; skipping",
                    node.id
                );
                return Ok(true);
            };

            let Some(signal) = state
                .signal_outputs
                .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
            else {
                eprintln!(
                    "[run_graph] threshold '{}' input signal unavailable; skipping",
                    node.id
                );
                return Ok(true);
            };

            let cutoff = node
                .params
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;

            let mut data = Vec::with_capacity(signal.data.len());
            for &val in &signal.data {
                data.push(if val >= cutoff { 1.0 } else { 0.0 });
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: signal.n,
                    t: signal.t,
                    c: signal.c,
                    data,
                },
            );
            Ok(true)
        }
        "normalize" => {
            let input_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

            let Some(input_edge) = input_edge else {
                eprintln!(
                    "[run_graph] normalize '{}' missing signal input; skipping",
                    node.id
                );
                return Ok(true);
            };

            let Some(signal) = state
                .signal_outputs
                .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
            else {
                eprintln!(
                    "[run_graph] normalize '{}' input signal unavailable; skipping",
                    node.id
                );
                return Ok(true);
            };

            let mut min_val = f32::INFINITY;
            let mut max_val = f32::NEG_INFINITY;
            let mut saw_finite = false;

            for &val in &signal.data {
                if !val.is_finite() {
                    return Ok(true);
                }
                saw_finite = true;
                min_val = min_val.min(val);
                max_val = max_val.max(val);
            }

            let mut data = Vec::with_capacity(signal.data.len());
            if !saw_finite {
                data.resize(signal.data.len(), 0.0);
            } else {
                let range = max_val - min_val;
                if range.abs() <= f32::EPSILON {
                    data.resize(signal.data.len(), 0.0);
                } else {
                    for &val in &signal.data {
                        if !val.is_finite() {
                            data.push(0.0);
                            return Ok(true);
                        }
                        data.push(((val - min_val) / range).clamp(0.0, 1.0));
                    }
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: signal.n,
                    t: signal.t,
                    c: signal.c,
                    data,
                },
            );
            Ok(true)
        }
        "falloff" => {
            let input_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

            let Some(input_edge) = input_edge else {
                eprintln!(
                    "[run_graph] falloff '{}' missing signal input; skipping",
                    node.id
                );
                return Ok(true);
            };

            let Some(signal) = state
                .signal_outputs
                .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
            else {
                eprintln!(
                    "[run_graph] falloff '{}' input signal unavailable; skipping",
                    node.id
                );
                return Ok(true);
            };

            let width = node
                .params
                .get("width")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
                .max(0.0) as f32;
            let curve = node
                .params
                .get("curve")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;

            let mut data = Vec::with_capacity(signal.data.len());
            let w = width.max(1e-6);
            for &val in &signal.data {
                let norm = val.clamp(0.0, 1.0);
                let tightened = (norm * w).clamp(0.0, 1.0);
                let shaped = shape_curve(tightened, curve);
                data.push(shaped);
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: signal.n,
                    t: signal.t,
                    c: signal.c,
                    data,
                },
            );
            Ok(true)
        }
        "invert" => {
            let input_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

            let Some(input_edge) = input_edge else {
                eprintln!(
                    "[run_graph] invert '{}' missing signal input; skipping",
                    node.id
                );
                return Ok(true);
            };

            let Some(signal) = state
                .signal_outputs
                .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
            else {
                eprintln!(
                    "[run_graph] invert '{}' input signal unavailable; skipping",
                    node.id
                );
                return Ok(true);
            };

            // Compute observed range
            let (mut min_v, mut max_v) = (f32::INFINITY, f32::NEG_INFINITY);
            for &v in &signal.data {
                if v < min_v {
                    min_v = v;
                }
                if v > max_v {
                    max_v = v;
                }
            }

            if !min_v.is_finite() || !max_v.is_finite() {
                return Ok(true);
            }

            let mid = (max_v + min_v) * 0.5;

            let mut data = Vec::with_capacity(signal.data.len());
            for &v in &signal.data {
                // Reflect around midpoint; clamp to observed range to avoid numeric overshoot.
                let reflected = 2.0 * mid - v;
                data.push(reflected.clamp(min_v, max_v));
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: signal.n,
                    t: signal.t,
                    c: signal.c,
                    data,
                },
            );
            Ok(true)
        }
        "scalar" => {
            let value = node
                .params
                .get("value")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: 1,
                    t: 1,
                    c: 1,
                    data: vec![value],
                },
            );
            Ok(true)
        }
        "modulo" => {
            let input_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

            let Some(input_edge) = input_edge else {
                return Ok(true);
            };

            let Some(signal) = state
                .signal_outputs
                .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
            else {
                return Ok(true);
            };

            let divisor = node
                .params
                .get("divisor")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;

            let mut data = Vec::with_capacity(signal.data.len());
            for &val in &signal.data {
                let res = if divisor != 0.0 {
                    ((val % divisor) + divisor) % divisor // Ensure positive result
                } else {
                    0.0
                };
                data.push(res);
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: signal.n,
                    t: signal.t,
                    c: signal.c,
                    data,
                },
            );
            Ok(true)
        }
        "sine_wave" => {
            let grid_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
            let grid = grid_edge.and_then(|edge| {
                state
                    .beat_grids
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
            });

            // Beat grid input is required
            let Some(grid) = grid else {
                return Ok(true);
            };

            let bpm = grid.bpm;

            let subdivision_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "subdivision"));
            let subdivision_signal = subdivision_edge.and_then(|edge| {
                state
                    .signal_outputs
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
            });

            let subdivision = if let Some(sig) = subdivision_signal {
                let mid_t = (context.start_time + context.end_time) / 2.0 - context.start_time;
                let duration = (context.end_time - context.start_time).max(0.001);
                let idx = ((mid_t / duration) * sig.data.len() as f32) as usize;
                sig.data
                    .get(idx.min(sig.data.len().saturating_sub(1)))
                    .copied()
                    .unwrap_or(1.0)
            } else {
                node.params
                    .get("subdivision")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0) as f32
            };
            let phase_deg = node
                .params
                .get("phase_deg")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let amplitude = node
                .params
                .get("amplitude")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let offset = node
                .params
                .get("offset")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;

            let duration = (context.end_time - context.start_time).max(0.001);
            let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
            let t_steps = t_steps.max(PREVIEW_LENGTH);
            let phase = phase_deg.to_radians();
            // subdivision = cycles per beat, so frequency = subdivision * (bpm / 60)
            let freq_hz = subdivision * (bpm / 60.0);
            let omega = 2.0 * std::f32::consts::PI * freq_hz;

            let mut data = Vec::with_capacity(t_steps);
            for i in 0..t_steps {
                let t = context.start_time + (i as f32 / (t_steps - 1).max(1) as f32) * duration;
                let time_in_pattern = t - context.start_time;
                data.push(offset + amplitude * (omega * time_in_pattern + phase).sin());
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: 1,
                    t: t_steps,
                    c: 1,
                    data,
                },
            );
            Ok(true)
        }
        "remap" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let input_edge = input_edges.iter().find(|e| e.to_port == "in");
            let Some(edge) = input_edge else {
                return Ok(true);
            };
            let Some(signal) = state
                .signal_outputs
                .get(&(edge.from_node.clone(), edge.from_port.clone()))
            else {
                eprintln!(
                    "[run_graph] remap '{}' input signal unavailable; skipping",
                    node.id
                );
                return Ok(true);
            };

            let in_min = node
                .params
                .get("in_min")
                .and_then(|v| v.as_f64())
                .unwrap_or(-1.0) as f32;
            let in_max = node
                .params
                .get("in_max")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let out_min = node
                .params
                .get("out_min")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let out_max = node
                .params
                .get("out_max")
                .and_then(|v| v.as_f64())
                .unwrap_or(180.0) as f32;
            let clamp_in = node
                .params
                .get("clamp")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
                > 0.5;

            let denom = in_max - in_min;
            let safe_denom = if denom.abs() < 1e-6 { 1.0 } else { denom };

            let mut data = Vec::with_capacity(signal.data.len());
            for &v0 in &signal.data {
                let v = if clamp_in {
                    v0.clamp(in_min.min(in_max), in_min.max(in_max))
                } else {
                    v0
                };
                let u = (v - in_min) / safe_denom;
                data.push(out_min + u * (out_max - out_min));
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: signal.n,
                    t: signal.t,
                    c: 1,
                    data,
                },
            );
            Ok(true)
        }
        "noise" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let time_edge = input_edges.iter().find(|e| e.to_port == "time");
            let x_edge = input_edges.iter().find(|e| e.to_port == "x");
            let y_edge = input_edges.iter().find(|e| e.to_port == "y");

            let time_opt = time_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let x_opt = x_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let y_opt = y_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            // Get params
            let scale = node
                .params
                .get("scale")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let octaves = node
                .params
                .get("octaves")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
                .clamp(1.0, 8.0) as u32;
            let amplitude = node
                .params
                .get("amplitude")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let offset = node
                .params
                .get("offset")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;

            // Hash function (same as random_select_mask / random_position)
            fn hash_combine(seed: u64, v: u64) -> u64 {
                let mut x = seed ^ v;
                x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                x ^ (x >> 31)
            }

            // Node ID hash for deterministic randomness per node instance
            let mut node_hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&node.id, &mut node_hasher);
            let base_seed = std::hash::Hasher::finish(&node_hasher);

            // Get a pseudo-random float in [-1, 1] for a given integer grid position
            fn noise_at_3d(x: i64, y: i64, z: i64, seed: u64) -> f32 {
                let h = hash_combine(
                    hash_combine(hash_combine(seed, x as u64), y as u64),
                    z as u64,
                );
                (h as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
            }

            // Smoothstep interpolation
            fn smoothstep(t: f32) -> f32 {
                t * t * (3.0 - 2.0 * t)
            }

            // 3D interpolated value noise (x, y spatial + z for time)
            fn value_noise_3d(x: f32, y: f32, z: f32, seed: u64) -> f32 {
                let x0 = x.floor() as i64;
                let x1 = x0 + 1;
                let y0 = y.floor() as i64;
                let y1 = y0 + 1;
                let z0 = z.floor() as i64;
                let z1 = z0 + 1;

                let tx = smoothstep(x - x0 as f32);
                let ty = smoothstep(y - y0 as f32);
                let tz = smoothstep(z - z0 as f32);

                let n000 = noise_at_3d(x0, y0, z0, seed);
                let n100 = noise_at_3d(x1, y0, z0, seed);
                let n010 = noise_at_3d(x0, y1, z0, seed);
                let n110 = noise_at_3d(x1, y1, z0, seed);
                let n001 = noise_at_3d(x0, y0, z1, seed);
                let n101 = noise_at_3d(x1, y0, z1, seed);
                let n011 = noise_at_3d(x0, y1, z1, seed);
                let n111 = noise_at_3d(x1, y1, z1, seed);

                let nx00 = n000 + tx * (n100 - n000);
                let nx10 = n010 + tx * (n110 - n010);
                let nx01 = n001 + tx * (n101 - n001);
                let nx11 = n011 + tx * (n111 - n011);

                let nxy0 = nx00 + ty * (nx10 - nx00);
                let nxy1 = nx01 + ty * (nx11 - nx01);

                nxy0 + tz * (nxy1 - nxy0)
            }

            // Fractal noise with octaves
            fn fractal_noise_3d(x: f32, y: f32, z: f32, seed: u64, octaves: u32) -> f32 {
                let mut total = 0.0f32;
                let mut frequency = 1.0f32;
                let mut amplitude_scale = 1.0f32;
                let mut max_value = 0.0f32;

                for i in 0..octaves {
                    let octave_seed = hash_combine(seed, i as u64 * 12345);
                    total +=
                        value_noise_3d(x * frequency, y * frequency, z * frequency, octave_seed)
                            * amplitude_scale;
                    max_value += amplitude_scale;
                    amplitude_scale *= 0.5;
                    frequency *= 2.0;
                }

                total / max_value
            }

            // Determine dimensions from inputs
            let n = x_opt.map(|s| s.n).or(y_opt.map(|s| s.n)).unwrap_or(1);
            let t_steps = time_opt
                .map(|s| s.t)
                .or(x_opt.map(|s| s.t))
                .or(y_opt.map(|s| s.t))
                .unwrap_or(256);

            let mut data = Vec::with_capacity(n * t_steps);

            for n_idx in 0..n {
                for t_idx in 0..t_steps {
                    // Get time value from input (smooth sampling coordinate)
                    let time_val = if let Some(time_sig) = time_opt {
                        let idx = (t_idx % time_sig.t) * time_sig.c;
                        time_sig.data.get(idx).copied().unwrap_or(0.0) * scale
                    } else {
                        0.0
                    };

                    // Get spatial coordinates from inputs or use defaults
                    let x_val = if let Some(x_sig) = x_opt {
                        let idx = n_idx * (x_sig.t * x_sig.c) + (t_idx % x_sig.t) * x_sig.c;
                        x_sig.data.get(idx).copied().unwrap_or(0.0) * scale
                    } else {
                        n_idx as f32 * scale
                    };

                    let y_val = if let Some(y_sig) = y_opt {
                        let idx = n_idx * (y_sig.t * y_sig.c) + (t_idx % y_sig.t) * y_sig.c;
                        y_sig.data.get(idx).copied().unwrap_or(0.0) * scale
                    } else {
                        0.0
                    };

                    let noise_val = fractal_noise_3d(x_val, y_val, time_val, base_seed, octaves);
                    data.push(offset + amplitude * noise_val);
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data,
                },
            );
            Ok(true)
        }
        "time_delay" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let input_edge = input_edges.iter().find(|e| e.to_port == "in");
            let delay_edge = input_edges.iter().find(|e| e.to_port == "delay");

            let Some(in_e) = input_edge else {
                return Ok(true);
            };
            let Some(input_signal) = state
                .signal_outputs
                .get(&(in_e.from_node.clone(), in_e.from_port.clone()))
            else {
                return Ok(true);
            };

            // Default delay signal is 0 (no delay)
            let default_delay = Signal {
                n: 1,
                t: 1,
                c: 1,
                data: vec![0.0],
            };
            let delay_signal = delay_edge
                .and_then(|e| {
                    state
                        .signal_outputs
                        .get(&(e.from_node.clone(), e.from_port.clone()))
                })
                .unwrap_or(&default_delay);

            let duration = (context.end_time - context.start_time).max(0.001);

            // Output dimensions: broadcast n from both signals, keep input's t and c
            let out_n = input_signal.n.max(delay_signal.n);
            let out_t = input_signal.t;
            let out_c = input_signal.c;

            let mut data = Vec::with_capacity(out_n * out_t * out_c);

            for i in 0..out_n {
                // Get delay for this fixture (sample from delay signal's n dimension)
                let delay_n_idx = if delay_signal.n <= 1 {
                    0
                } else {
                    i % delay_signal.n
                };
                // Use first time step of delay signal (delay is typically constant per fixture)
                let delay_idx = delay_n_idx * (delay_signal.t * delay_signal.c);
                let delay_seconds = delay_signal.data.get(delay_idx).copied().unwrap_or(0.0);

                // Map input signal's n dimension
                let input_n_idx = if input_signal.n <= 1 {
                    0
                } else {
                    i % input_signal.n
                };

                for t in 0..out_t {
                    // Current time as fraction of duration [0, 1]
                    let t_frac = if out_t <= 1 {
                        0.0
                    } else {
                        t as f32 / (out_t - 1) as f32
                    };
                    let current_time = t_frac * duration;

                    // Delayed time: we want to sample from (current_time - delay)
                    // Positive delay = lag (sample from past), negative delay = advance (sample from future)
                    let sample_time = current_time - delay_seconds;

                    // Convert sample_time back to a fractional position in the input signal
                    let sample_frac = sample_time / duration;

                    // Clamp to [0, 1] range
                    let clamped_frac = sample_frac.clamp(0.0, 1.0);

                    // Convert to input signal's time index (with linear interpolation)
                    let input_t_f = if input_signal.t <= 1 {
                        0.0
                    } else {
                        clamped_frac * (input_signal.t - 1) as f32
                    };

                    let t_lo = (input_t_f.floor() as usize).min(input_signal.t.saturating_sub(1));
                    let t_hi = (t_lo + 1).min(input_signal.t.saturating_sub(1));
                    let t_blend = input_t_f - input_t_f.floor();

                    for c in 0..out_c {
                        let c_idx = if input_signal.c <= 1 {
                            0
                        } else {
                            c % input_signal.c
                        };

                        let idx_lo = input_n_idx * (input_signal.t * input_signal.c)
                            + t_lo * input_signal.c
                            + c_idx;
                        let idx_hi = input_n_idx * (input_signal.t * input_signal.c)
                            + t_hi * input_signal.c
                            + c_idx;

                        let val_lo = input_signal.data.get(idx_lo).copied().unwrap_or(0.0);
                        let val_hi = input_signal.data.get(idx_hi).copied().unwrap_or(0.0);

                        // Linear interpolation
                        let val = val_lo + t_blend * (val_hi - val_lo);
                        data.push(val);
                    }
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "out".into()),
                Signal {
                    n: out_n,
                    t: out_t,
                    c: out_c,
                    data,
                },
            );
            Ok(true)
        }
        // =====================================================================
        // Movement perturbation nodes — output Signal C=2 (u,v) in [-1,1]
        // =====================================================================
        "circle" => {
            let grid_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
            let grid = if let Some(edge) = grid_edge {
                state
                    .beat_grids
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .or(context.beat_grid.as_ref())
            } else {
                context.beat_grid.as_ref()
            };

            let phase_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "phase"));
            let phase_signal = phase_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            let radius = node
                .params
                .get("radius")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let speed_cycles = node
                .params
                .get("speed")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25) as f32;

            let duration = (context.end_time - context.start_time).max(0.001);
            let t_steps = ((duration * SIMULATION_RATE).ceil() as usize).max(PREVIEW_LENGTH);
            let beat_len = grid
                .map(|g| if g.bpm > 0.0 { 60.0 / g.bpm } else { 0.5 })
                .unwrap_or(0.5);
            let n = phase_signal.map(|s| s.n).unwrap_or(1);

            let mut data = Vec::with_capacity(n * t_steps * 2);
            for prim_idx in 0..n {
                for t_idx in 0..t_steps {
                    let t = if t_steps == 1 {
                        0.0
                    } else {
                        (t_idx as f32 / (t_steps - 1) as f32) * duration
                    };
                    let phase_offset = if let Some(phase_sig) = phase_signal {
                        let idx = prim_idx * (phase_sig.t * phase_sig.c)
                            + (t_idx % phase_sig.t) * phase_sig.c;
                        phase_sig.data.get(idx).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    let beats = t / beat_len;
                    let angle = 2.0 * std::f32::consts::PI * (speed_cycles * beats + phase_offset);
                    data.push(angle.cos() * radius);
                    data.push(angle.sin() * radius);
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "uv".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 2,
                    data,
                },
            );
            Ok(true)
        }
        "figure_8" => {
            let grid_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
            let grid = if let Some(edge) = grid_edge {
                state
                    .beat_grids
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .or(context.beat_grid.as_ref())
            } else {
                context.beat_grid.as_ref()
            };

            let phase_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "phase"));
            let phase_signal = phase_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            let width = node
                .params
                .get("width")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let height = node
                .params
                .get("height")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;
            let speed_cycles = node
                .params
                .get("speed")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25) as f32;

            let duration = (context.end_time - context.start_time).max(0.001);
            let t_steps = ((duration * SIMULATION_RATE).ceil() as usize).max(PREVIEW_LENGTH);
            let beat_len = grid
                .map(|g| if g.bpm > 0.0 { 60.0 / g.bpm } else { 0.5 })
                .unwrap_or(0.5);
            let n = phase_signal.map(|s| s.n).unwrap_or(1);

            let mut data = Vec::with_capacity(n * t_steps * 2);
            for prim_idx in 0..n {
                for t_idx in 0..t_steps {
                    let t = if t_steps == 1 {
                        0.0
                    } else {
                        (t_idx as f32 / (t_steps - 1) as f32) * duration
                    };
                    let phase_offset = if let Some(phase_sig) = phase_signal {
                        let idx = prim_idx * (phase_sig.t * phase_sig.c)
                            + (t_idx % phase_sig.t) * phase_sig.c;
                        phase_sig.data.get(idx).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    let beats = t / beat_len;
                    let theta = 2.0 * std::f32::consts::PI * (speed_cycles * beats + phase_offset);
                    // Lissajous 2:1 — figure-8
                    data.push(theta.cos() * width);
                    data.push((2.0 * theta).sin() * height);
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "uv".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 2,
                    data,
                },
            );
            Ok(true)
        }
        "sweep" => {
            let grid_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
            let grid = if let Some(edge) = grid_edge {
                state
                    .beat_grids
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .or(context.beat_grid.as_ref())
            } else {
                context.beat_grid.as_ref()
            };

            let phase_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "phase"));
            let phase_signal = phase_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            let angle_deg = node
                .params
                .get("angle")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let range = node
                .params
                .get("range")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32;
            let speed_cycles = node
                .params
                .get("speed")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;

            let duration = (context.end_time - context.start_time).max(0.001);
            let t_steps = ((duration * SIMULATION_RATE).ceil() as usize).max(PREVIEW_LENGTH);
            let beat_len = grid
                .map(|g| if g.bpm > 0.0 { 60.0 / g.bpm } else { 0.5 })
                .unwrap_or(0.5);
            let n = phase_signal.map(|s| s.n).unwrap_or(1);
            let angle_rad = angle_deg.to_radians();
            let cos_a = angle_rad.cos();
            let sin_a = angle_rad.sin();

            let mut data = Vec::with_capacity(n * t_steps * 2);
            for prim_idx in 0..n {
                for t_idx in 0..t_steps {
                    let t = if t_steps == 1 {
                        0.0
                    } else {
                        (t_idx as f32 / (t_steps - 1) as f32) * duration
                    };
                    let phase_offset = if let Some(phase_sig) = phase_signal {
                        let idx = prim_idx * (phase_sig.t * phase_sig.c)
                            + (t_idx % phase_sig.t) * phase_sig.c;
                        phase_sig.data.get(idx).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    let beats = t / beat_len;
                    let theta = 2.0 * std::f32::consts::PI * (speed_cycles * beats + phase_offset);
                    let sweep_val = theta.sin() * range;
                    // Project along angle: (cos(angle)*val, sin(angle)*val)
                    data.push(cos_a * sweep_val);
                    data.push(sin_a * sweep_val);
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "uv".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 2,
                    data,
                },
            );
            Ok(true)
        }
        "wander" => {
            let grid_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
            let grid = if let Some(edge) = grid_edge {
                state
                    .beat_grids
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
                    .or(context.beat_grid.as_ref())
            } else {
                context.beat_grid.as_ref()
            };

            let phase_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "phase"));
            let phase_signal = phase_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            let radius = node
                .params
                .get("radius")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;
            let speed_cycles = node
                .params
                .get("speed")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25) as f32;
            let smoothness = node
                .params
                .get("smoothness")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0)
                .clamp(0.5, 8.0) as f32;

            let duration = (context.end_time - context.start_time).max(0.001);
            let t_steps = ((duration * SIMULATION_RATE).ceil() as usize).max(PREVIEW_LENGTH);
            let beat_len = grid
                .map(|g| if g.bpm > 0.0 { 60.0 / g.bpm } else { 0.5 })
                .unwrap_or(0.5);
            let n = phase_signal.map(|s| s.n).unwrap_or(1);

            // Reuse noise infrastructure from the "noise" node
            fn wander_hash(seed: u64, v: u64) -> u64 {
                let mut x = seed ^ v;
                x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                x ^ (x >> 31)
            }

            fn wander_noise_1d(pos: i64, seed: u64) -> f32 {
                let h = wander_hash(seed, pos as u64);
                (h as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
            }

            fn wander_smoothstep(t: f32) -> f32 {
                t * t * (3.0 - 2.0 * t)
            }

            fn wander_interp(x: f32, seed: u64) -> f32 {
                let x0 = x.floor() as i64;
                let x1 = x0 + 1;
                let t = wander_smoothstep(x - x0 as f32);
                let n0 = wander_noise_1d(x0, seed);
                let n1 = wander_noise_1d(x1, seed);
                n0 + t * (n1 - n0)
            }

            fn wander_fractal(x: f32, seed: u64, octaves: u32) -> f32 {
                let mut total = 0.0f32;
                let mut freq = 1.0f32;
                let mut amp = 1.0f32;
                let mut max_val = 0.0f32;
                for i in 0..octaves {
                    let oct_seed = wander_hash(seed, i as u64 * 7919);
                    total += wander_interp(x * freq, oct_seed) * amp;
                    max_val += amp;
                    amp *= 0.5;
                    freq *= 2.0;
                }
                total / max_val
            }

            let mut node_hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&node.id, &mut node_hasher);
            let base_seed = std::hash::Hasher::finish(&node_hasher);

            let octaves = smoothness.round() as u32;

            let mut data = Vec::with_capacity(n * t_steps * 2);
            for prim_idx in 0..n {
                let phase_offset = if let Some(phase_sig) = phase_signal {
                    let idx = prim_idx * (phase_sig.t * phase_sig.c);
                    phase_sig.data.get(idx).copied().unwrap_or(0.0)
                } else {
                    0.0
                };

                // Use different seeds for U and V channels
                let seed_u = wander_hash(base_seed, prim_idx as u64 * 2);
                let seed_v = wander_hash(base_seed, prim_idx as u64 * 2 + 1);

                for t_idx in 0..t_steps {
                    let t = if t_steps == 1 {
                        0.0
                    } else {
                        (t_idx as f32 / (t_steps - 1) as f32) * duration
                    };
                    let beats = t / beat_len;
                    let noise_coord = speed_cycles * beats + phase_offset * 10.0;

                    let u = wander_fractal(noise_coord, seed_u, octaves) * radius;
                    let v = wander_fractal(noise_coord, seed_v, octaves) * radius;
                    data.push(u.clamp(-1.0, 1.0));
                    data.push(v.clamp(-1.0, 1.0));
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "uv".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 2,
                    data,
                },
            );
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "math".into(),
            name: "Math".into(),
            description: Some("Performs math operations on signals with broadcasting.".into()),
            category: Some("Transform".into()),
            inputs: vec![
                PortDef {
                    id: "a".into(),
                    name: "A".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "b".into(),
                    name: "B".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "operation".into(),
                name: "Operation".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("add".into()), // add, subtract, multiply, divide, max, min, abs_diff, abs, modulo, circular_distance
            }],
        },
        NodeTypeDef {
            id: "round".into(),
            name: "Round".into(),
            description: Some("Quantizes signal values (floor, ceil, round).".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "operation".into(),
                name: "Operation".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("round".into()), // round, floor, ceil
            }],
        },
        NodeTypeDef {
            id: "ramp".into(),
            name: "Time Ramp".into(),
            description: Some(
                "Generates a linear ramp from 0 to n_beats over the pattern duration.".into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![PortDef {
                id: "grid".into(),
                name: "Beat Grid".into(),
                port_type: PortType::BeatGrid,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "ramp_between".into(),
            name: "Linear Ramp".into(),
            description: Some(
                "Generates a linear ramp from start to end signals over the pattern duration."
                    .into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
                PortDef {
                    id: "start".into(),
                    name: "Start".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "end".into(),
                    name: "End".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "threshold".into(),
            name: "Threshold".into(),
            description: Some("Binarizes a signal using a cutoff value.".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "threshold".into(),
                name: "Threshold".into(),
                param_type: ParamType::Number,
                default_number: Some(0.5),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "normalize".into(),
            name: "Normalize (0-1)".into(),
            description: Some(
                "Normalizes an input signal into the 0..1 range using min/max over the whole time series."
                    .into(),
            ),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "falloff".into(),
            name: "Falloff".into(),
            description: Some("Applies a soft falloff to a normalized signal (0..1).".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "width".into(),
                    name: "Width".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "curve".into(),
                    name: "Curve".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "invert".into(),
            name: "Invert".into(),
            description: Some("Reflects a signal around its observed midpoint.".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "sine_wave".into(),
            name: "Sine Wave".into(),
            description: Some("Generates a beat-synced sine wave. Subdivision controls cycles per beat (1 = one full cycle per beat, 0.5 = every 2 beats, 2 = twice per beat).".into()),
            category: Some("Generator".into()),
            inputs: vec![
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
                PortDef {
                    id: "subdivision".into(),
                    name: "Subdivision".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "subdivision".into(),
                    name: "Subdivision".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "phase_deg".into(),
                    name: "Phase (deg)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "amplitude".into(),
                    name: "Amplitude".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "offset".into(),
                    name: "Offset".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "noise".into(),
            name: "Noise".into(),
            description: Some(
                "Generates 3D fractal noise. Samples at (x, y, time) coordinates."
                    .into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![
                PortDef {
                    id: "time".into(),
                    name: "Time".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "x".into(),
                    name: "X".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "y".into(),
                    name: "Y".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "scale".into(),
                    name: "Scale".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "octaves".into(),
                    name: "Octaves".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "amplitude".into(),
                    name: "Amplitude".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "offset".into(),
                    name: "Offset".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "remap".into(),
            name: "Remap".into(),
            description: Some("Linearly maps an input range [in_min..in_max] to [out_min..out_max].".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "in_min".into(),
                    name: "In Min".into(),
                    param_type: ParamType::Number,
                    default_number: Some(-1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "in_max".into(),
                    name: "In Max".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "out_min".into(),
                    name: "Out Min".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "out_max".into(),
                    name: "Out Max".into(),
                    param_type: ParamType::Number,
                    default_number: Some(180.0),
                    default_text: None,
                },
                ParamDef {
                    id: "clamp".into(),
                    name: "Clamp".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
            ],
        },
        // ----- Movement perturbation nodes -----
        NodeTypeDef {
            id: "circle".into(),
            name: "Circle".into(),
            description: Some(
                "Circular motion in UV space. Outputs normalized (u,v) in [-1,1].".into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
                PortDef {
                    id: "phase".into(),
                    name: "Phase Offset".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "radius".into(),
                    name: "Radius".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.25),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "figure_8".into(),
            name: "Figure 8".into(),
            description: Some(
                "Lissajous 2:1 figure-eight motion in UV space. Outputs normalized (u,v) in [-1,1]."
                    .into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
                PortDef {
                    id: "phase".into(),
                    name: "Phase Offset".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "width".into(),
                    name: "Width".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "height".into(),
                    name: "Height".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.25),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "sweep".into(),
            name: "Sweep".into(),
            description: Some(
                "Linear sweep at an angle in UV space. 0\u{00b0}=U axis, 90\u{00b0}=V axis. Outputs normalized (u,v) in [-1,1]."
                    .into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
                PortDef {
                    id: "phase".into(),
                    name: "Phase Offset".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "angle".into(),
                    name: "Angle (deg)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "range".into(),
                    name: "Range".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "wander".into(),
            name: "Wander".into(),
            description: Some(
                "Noise-based organic drift in UV space. Outputs normalized (u,v) in [-1,1]."
                    .into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
                PortDef {
                    id: "phase".into(),
                    name: "Phase Offset".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "radius".into(),
                    name: "Radius".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.25),
                    default_text: None,
                },
                ParamDef {
                    id: "smoothness".into(),
                    name: "Smoothness".into(),
                    param_type: ParamType::Number,
                    default_number: Some(2.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "scalar".into(),
            name: "Scalar".into(),
            description: Some("Outputs a constant scalar value.".into()),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "value".into(),
                name: "Value".into(),
                param_type: ParamType::Number,
                default_number: Some(1.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "modulo".into(),
            name: "Modulo".into(),
            description: Some("Wraps input values to range [0, divisor). Useful for looping animations.".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "divisor".into(),
                name: "Divisor".into(),
                param_type: ParamType::Number,
                default_number: Some(1.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "time_delay".into(),
            name: "Time Delay".into(),
            description: Some(
                "Delays a signal in time per-fixture. Positive delay = lag, negative = advance."
                    .into(),
            ),
            category: Some("Transform".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Signal".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "delay".into(),
                    name: "Delay (s)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
    ]
}
