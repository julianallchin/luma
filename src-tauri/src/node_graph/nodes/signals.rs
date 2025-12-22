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
                let idx_a_n = if a.n == 1 { 0 } else { i % a.n };
                let idx_b_n = if b.n == 1 { 0 } else { i % b.n };

                for j in 0..out_t {
                    let idx_a_t = if a.t == 1 { 0 } else { j % a.t };
                    let idx_b_t = if b.t == 1 { 0 } else { j % b.t };

                    for k in 0..out_c {
                        let idx_a_c = if a.c == 1 { 0 } else { k % a.c };
                        let idx_b_c = if b.c == 1 { 0 } else { k % b.c };

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
                            "modulo" => {
                                if val_b != 0.0 {
                                    val_a % val_b
                                } else {
                                    0.0
                                }
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
        "orbit" => {
            // Get beat grid for timing
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

            // Get phase offset input (optional)
            let phase_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "phase"));
            let phase_signal = phase_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            // Get params
            let center_x = node
                .params
                .get("center_x")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let center_y = node
                .params
                .get("center_y")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0) as f32;
            let center_z = node
                .params
                .get("center_z")
                .and_then(|v| v.as_f64())
                .unwrap_or(5.0) as f32;
            let radius_x = node
                .params
                .get("radius_x")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0) as f32;
            let radius_z = node
                .params
                .get("radius_z")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0) as f32;
            let speed_cycles = node
                .params
                .get("speed")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25) as f32;
            let tilt_deg = node
                .params
                .get("tilt_deg")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;

            let t_steps = 256usize;
            let duration = (context.end_time - context.start_time).max(0.001);
            let tilt_rad = tilt_deg.to_radians();

            // Get beat duration for timing
            let beat_len = grid
                .map(|g| if g.bpm > 0.0 { 60.0 / g.bpm } else { 0.5 })
                .unwrap_or(0.5);

            // Calculate N based on phase input
            let n = phase_signal.map(|s| s.n).unwrap_or(1);

            let mut x_data = Vec::with_capacity(n * t_steps);
            let mut y_data = Vec::with_capacity(n * t_steps);
            let mut z_data = Vec::with_capacity(n * t_steps);

            for prim_idx in 0..n {
                for t_idx in 0..t_steps {
                    let t = if t_steps == 1 {
                        0.0
                    } else {
                        (t_idx as f32 / (t_steps - 1) as f32) * duration
                    };

                    // Get phase offset for this primitive at this time
                    let phase_offset = if let Some(phase_sig) = phase_signal {
                        let idx = prim_idx * (phase_sig.t * phase_sig.c)
                            + (t_idx % phase_sig.t) * phase_sig.c;
                        phase_sig.data.get(idx).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };

                    // Convert time to beats
                    let beats = t / beat_len;
                    let angle = 2.0 * std::f32::consts::PI * (speed_cycles * beats + phase_offset);

                    // Calculate position in orbit plane (XZ)
                    let orbit_x = radius_x * angle.cos();
                    let orbit_z = radius_z * angle.sin();

                    // Apply plane tilt (rotate around X axis)
                    let y_offset = orbit_z * tilt_rad.sin();
                    let z_final = orbit_z * tilt_rad.cos();

                    x_data.push(center_x + orbit_x);
                    y_data.push(center_y + y_offset);
                    z_data.push(center_z + z_final);
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "x".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data: x_data,
                },
            );
            state.signal_outputs.insert(
                (node.id.clone(), "y".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data: y_data,
                },
            );
            state.signal_outputs.insert(
                (node.id.clone(), "z".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data: z_data,
                },
            );
            Ok(true)
        }
        "random_position" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let trigger_edge = input_edges.iter().find(|e| e.to_port == "trigger");

            let trigger_opt = trigger_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            // Get params
            let min_x = node
                .params
                .get("min_x")
                .and_then(|v| v.as_f64())
                .unwrap_or(-3.0) as f32;
            let max_x = node
                .params
                .get("max_x")
                .and_then(|v| v.as_f64())
                .unwrap_or(3.0) as f32;
            let min_y = node
                .params
                .get("min_y")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let max_y = node
                .params
                .get("max_y")
                .and_then(|v| v.as_f64())
                .unwrap_or(3.0) as f32;
            let min_z = node
                .params
                .get("min_z")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0) as f32;
            let max_z = node
                .params
                .get("max_z")
                .and_then(|v| v.as_f64())
                .unwrap_or(8.0) as f32;

            if let Some(trigger) = trigger_opt {
                let t_steps = trigger.t;

                // Helper for hashing (same as random_select_mask)
                fn hash_combine(seed: u64, v: u64) -> u64 {
                    let mut x = seed ^ v;
                    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                    x ^ (x >> 31)
                }

                // Node ID hash for deterministic randomness
                let mut node_hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(&node.id, &mut node_hasher);
                let node_seed = std::hash::Hasher::finish(&node_hasher);

                // Time-based seed for variety across executions
                let time_seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);

                let mut x_data = Vec::with_capacity(t_steps);
                let mut y_data = Vec::with_capacity(t_steps);
                let mut z_data = Vec::with_capacity(t_steps);

                let mut prev_trig_seed: Option<i64> = None;
                let mut current_x = (min_x + max_x) / 2.0;
                let mut current_y = (min_y + max_y) / 2.0;
                let mut current_z = (min_z + max_z) / 2.0;
                let mut position_counter: u64 = 0;

                for t in 0..t_steps {
                    let trig_val = trigger.data.get(t * trigger.c).copied().unwrap_or(0.0);
                    let trig_seed = (trig_val * 1000.0) as i64;

                    let trigger_changed = prev_trig_seed.is_none_or(|prev| prev != trig_seed);

                    if trigger_changed {
                        // Generate new random position
                        let step_seed = hash_combine(
                            hash_combine(hash_combine(node_seed, time_seed), trig_seed as u64),
                            position_counter,
                        );

                        // Generate pseudo-random values in [0, 1]
                        let rand_x = (hash_combine(step_seed, 0) as f64 / u64::MAX as f64) as f32;
                        let rand_y = (hash_combine(step_seed, 1) as f64 / u64::MAX as f64) as f32;
                        let rand_z = (hash_combine(step_seed, 2) as f64 / u64::MAX as f64) as f32;

                        current_x = min_x + rand_x * (max_x - min_x);
                        current_y = min_y + rand_y * (max_y - min_y);
                        current_z = min_z + rand_z * (max_z - min_z);

                        prev_trig_seed = Some(trig_seed);
                        position_counter += 1;
                    }

                    x_data.push(current_x);
                    y_data.push(current_y);
                    z_data.push(current_z);
                }

                state.signal_outputs.insert(
                    (node.id.clone(), "x".into()),
                    Signal {
                        n: 1,
                        t: t_steps,
                        c: 1,
                        data: x_data,
                    },
                );
                state.signal_outputs.insert(
                    (node.id.clone(), "y".into()),
                    Signal {
                        n: 1,
                        t: t_steps,
                        c: 1,
                        data: y_data,
                    },
                );
                state.signal_outputs.insert(
                    (node.id.clone(), "z".into()),
                    Signal {
                        n: 1,
                        t: t_steps,
                        c: 1,
                        data: z_data,
                    },
                );
            }
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
        "sine_wave" => {
            let frequency_hz = node
                .params
                .get("frequency_hz")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25) as f32;
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

            let t_steps = 256usize;
            let duration = (context.end_time - context.start_time).max(0.001);
            let phase = phase_deg.to_radians();
            let omega = 2.0 * std::f32::consts::PI * frequency_hz;

            let mut data = Vec::with_capacity(t_steps);
            for i in 0..t_steps {
                let t = if t_steps == 1 {
                    0.0
                } else {
                    (i as f32 / (t_steps - 1) as f32) * duration
                };
                data.push(offset + amplitude * (omega * t + phase).sin());
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
        "smooth_movement" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let pan_edge = input_edges.iter().find(|e| e.to_port == "pan_in");
            let tilt_edge = input_edges.iter().find(|e| e.to_port == "tilt_in");

            let pan = pan_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let tilt = tilt_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            if pan.is_none() && tilt.is_none() {
                return Ok(true);
            }

            let pan_max_deg_per_s = node
                .params
                .get("pan_max_deg_per_s")
                .and_then(|v| v.as_f64())
                .unwrap_or(360.0) as f32;
            let tilt_max_deg_per_s = node
                .params
                .get("tilt_max_deg_per_s")
                .and_then(|v| v.as_f64())
                .unwrap_or(180.0) as f32;

            let t_steps = pan
                .map(|s| s.t)
                .unwrap_or(1)
                .max(tilt.map(|s| s.t).unwrap_or(1))
                .max(1);
            let duration = (context.end_time - context.start_time).max(0.001);
            let dt = if t_steps <= 1 {
                0.0
            } else {
                duration / (t_steps - 1) as f32
            };

            let n = pan
                .map(|s| s.n)
                .unwrap_or(1)
                .max(tilt.map(|s| s.n).unwrap_or(1))
                .max(1);

            let sample = |sig: Option<&Signal>, i: usize, t: usize, t_steps: usize| -> f32 {
                let Some(sig) = sig else { return f32::NAN };
                let sig_i = if sig.n == 1 { 0 } else { i % sig.n };
                let sig_t = if sig.t == 1 {
                    0
                } else if t_steps <= 1 {
                    0
                } else {
                    ((t as f32 / (t_steps - 1) as f32) * (sig.t - 1) as f32).round() as usize
                };
                let idx = sig_i * (sig.t * sig.c) + sig_t * sig.c;
                sig.data.get(idx).copied().unwrap_or(0.0)
            };

            let mut pan_data = Vec::with_capacity(n * t_steps);
            let mut tilt_data = Vec::with_capacity(n * t_steps);

            for i in 0..n {
                let mut prev_pan = sample(pan, i, 0, t_steps);
                let mut prev_tilt = sample(tilt, i, 0, t_steps);

                pan_data.push(prev_pan);
                tilt_data.push(prev_tilt);

                for t in 1..t_steps {
                    let target_pan = sample(pan, i, t, t_steps);
                    let target_tilt = sample(tilt, i, t, t_steps);

                    let max_pan_delta = if pan_max_deg_per_s > 0.0 {
                        pan_max_deg_per_s * dt
                    } else {
                        f32::INFINITY
                    };
                    let max_tilt_delta = if tilt_max_deg_per_s > 0.0 {
                        tilt_max_deg_per_s * dt
                    } else {
                        f32::INFINITY
                    };

                    if target_pan.is_finite() && prev_pan.is_finite() {
                        prev_pan += (target_pan - prev_pan).clamp(-max_pan_delta, max_pan_delta);
                    } else if target_pan.is_finite() {
                        prev_pan = target_pan;
                    } else {
                        prev_pan = f32::NAN;
                    }

                    if target_tilt.is_finite() && prev_tilt.is_finite() {
                        prev_tilt +=
                            (target_tilt - prev_tilt).clamp(-max_tilt_delta, max_tilt_delta);
                    } else if target_tilt.is_finite() {
                        prev_tilt = target_tilt;
                    } else {
                        prev_tilt = f32::NAN;
                    }

                    pan_data.push(prev_pan);
                    tilt_data.push(prev_tilt);
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "pan".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data: pan_data,
                },
            );
            state.signal_outputs.insert(
                (node.id.clone(), "tilt".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data: tilt_data,
                },
            );
            Ok(true)
        }
        "look_at_position" => {
            use std::collections::HashMap;

            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();
            let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
            let x_edge = input_edges.iter().find(|e| e.to_port == "x");
            let y_edge = input_edges.iter().find(|e| e.to_port == "y");
            let z_edge = input_edges.iter().find(|e| e.to_port == "z");

            let Some(sel_e) = selection_edge else {
                return Ok(true);
            };
            let Some(selection) = state
                .selections
                .get(&(sel_e.from_node.clone(), sel_e.from_port.clone()))
            else {
                return Ok(true);
            };

            let x_sig = x_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let y_sig = y_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });
            let z_sig = z_edge.and_then(|e| {
                state
                    .signal_outputs
                    .get(&(e.from_node.clone(), e.from_port.clone()))
            });

            if x_sig.is_none() && y_sig.is_none() && z_sig.is_none() {
                return Ok(true);
            }

            let pan_offset_deg = node
                .params
                .get("pan_offset_deg")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let tilt_offset_deg = node
                .params
                .get("tilt_offset_deg")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let clamp_enabled = node
                .params
                .get("clamp")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0)
                >= 0.5;

            let t_steps = x_sig
                .map(|s| s.t)
                .unwrap_or(1)
                .max(y_sig.map(|s| s.t).unwrap_or(1))
                .max(z_sig.map(|s| s.t).unwrap_or(1))
                .max(1);

            let sample = |sig: Option<&Signal>, i: usize, t: usize, t_steps: usize| -> f32 {
                let Some(sig) = sig else { return 0.0 };
                let sig_i = if sig.n == 1 { 0 } else { i % sig.n };
                let sig_t = if sig.t == 1 {
                    0
                } else if t_steps <= 1 {
                    0
                } else {
                    ((t as f32 / (t_steps - 1) as f32) * (sig.t - 1) as f32).round() as usize
                };
                let idx = sig_i * (sig.t * sig.c) + sig_t * sig.c;
                sig.data.get(idx).copied().unwrap_or(0.0)
            };

            let mut pan_tilt_max_by_fixture: HashMap<String, (f32, f32)> = HashMap::new();
            let mut rot_by_fixture: HashMap<String, (f32, f32, f32)> = HashMap::new();
            if let Some(proj_pool) = project_pool {
                let fixtures = crate::database::local::fixtures::get_fixtures_for_venue(
                    proj_pool,
                    ctx.graph_context.venue_id,
                )
                .await
                .map_err(|e| format!("LookAtPosition node failed to fetch fixtures: {}", e))?;

                let mut fixture_path_by_id: HashMap<String, String> = HashMap::new();
                for fx in fixtures {
                    fixture_path_by_id.insert(fx.id.clone(), fx.fixture_path.clone());
                    rot_by_fixture.insert(
                        fx.id.clone(),
                        (fx.rot_x as f32, fx.rot_y as f32, fx.rot_z as f32),
                    );
                }

                for item in &selection.items {
                    if pan_tilt_max_by_fixture.contains_key(&item.fixture_id) {
                        return Ok(true);
                    }

                    let Some(fixture_path) = fixture_path_by_id.get(&item.fixture_id).cloned()
                    else {
                        return Ok(true);
                    };

                    let def_path = if let Some(root) = &resource_path_root {
                        root.join(&fixture_path)
                    } else {
                        PathBuf::from(&fixture_path)
                    };

                    let (pan_max, tilt_max) = if let Ok(def) = parse_definition(&def_path) {
                        let pan_max = def
                            .physical
                            .as_ref()
                            .and_then(|p| p.focus.as_ref())
                            .and_then(|f| f.pan_max)
                            .unwrap_or(360) as f32;
                        let tilt_max = def
                            .physical
                            .as_ref()
                            .and_then(|p| p.focus.as_ref())
                            .and_then(|f| f.tilt_max)
                            .unwrap_or(180) as f32;
                        (pan_max, tilt_max)
                    } else {
                        (360.0, 180.0)
                    };

                    pan_tilt_max_by_fixture.insert(item.fixture_id.clone(), (pan_max, tilt_max));
                }
            }

            let n = selection.items.len();
            let mut pan_data = Vec::with_capacity(n * t_steps);
            let mut tilt_data = Vec::with_capacity(n * t_steps);

            // Transform a world-space direction into fixture-local space.
            // The fixture has Euler XYZ rotation (rx, ry, rz), meaning the object
            // is rotated first around X, then Y, then Z.
            // In matrix terms: R = Rz * Ry * Rx (so Rx applies first to the local coords).
            // To go from world to local, we apply R^-1 = Rx^-1 * Ry^-1 * Rz^-1,
            // which means we apply Rz^-1 first, then Ry^-1, then Rx^-1.
            let world_to_local =
                |v: (f32, f32, f32), rx: f32, ry: f32, rz: f32| -> (f32, f32, f32) {
                    let (mut x, mut y, mut z) = v;

                    // Step 1: Inverse rotate around Z (Rz^-1, i.e., rotate by -rz)
                    let (cz, sz) = (rz.cos(), rz.sin());
                    let x1 = x * cz + y * sz;
                    let y1 = -x * sz + y * cz;
                    x = x1;
                    y = y1;

                    // Step 2: Inverse rotate around Y (Ry^-1, i.e., rotate by -ry)
                    let (cy, sy) = (ry.cos(), ry.sin());
                    let x2 = x * cy - z * (-sy);
                    let z2 = x * (-sy) + z * cy;
                    x = x2;
                    z = z2;

                    // Step 3: Inverse rotate around X (Rx^-1, i.e., rotate by -rx)
                    let (cx, sx) = (rx.cos(), rx.sin());
                    let y3 = y * cx + z * sx;
                    let z3 = -y * sx + z * cx;
                    y = y3;
                    z = z3;

                    (x, y, z)
                };

            for (i, item) in selection.items.iter().enumerate() {
                let (pan_max, tilt_max) = pan_tilt_max_by_fixture
                    .get(&item.fixture_id)
                    .copied()
                    .unwrap_or((360.0, 180.0));
                let (rx, ry, rz) = rot_by_fixture
                    .get(&item.fixture_id)
                    .copied()
                    .unwrap_or((0.0, 0.0, 0.0));

                for t in 0..t_steps {
                    let tx = sample(x_sig, i, t, t_steps);
                    let ty = sample(y_sig, i, t, t_steps);
                    let tz = sample(z_sig, i, t, t_steps);

                    let dx = tx - item.pos.0;
                    let dy = ty - item.pos.1;
                    let dz = tz - item.pos.2;

                    // Transform direction into fixture-local space.
                    let (lx, ly, lz) = world_to_local((dx, dy, dz), rx, ry, rz);

                    // Moving head beam geometry:
                    // - At pan=0, tilt=0, beam points straight down (-Y in fixture-local space)
                    // - Pan rotates the arm around Y: arm.rotation.y = pan_deg * PI/180
                    // - Tilt rotates the head around X: head.rotation.x = -tilt_deg * PI/180
                    //
                    // The beam direction given (pan, tilt) is:
                    //   (sin(pan)*sin(tilt), -cos(tilt), cos(pan)*sin(tilt))
                    //
                    // To aim at target (lx, ly, lz):
                    //   pan = atan2(lx, lz)
                    //   tilt = atan2(sqrt(lx² + lz²), -ly)

                    let mut pan_deg = lx.atan2(lz).to_degrees();
                    let horiz = (lx * lx + lz * lz).sqrt();
                    let mut tilt_deg = horiz.atan2(-ly).to_degrees();

                    pan_deg += pan_offset_deg;
                    tilt_deg += tilt_offset_deg;

                    if clamp_enabled {
                        pan_deg = pan_deg.clamp(-pan_max / 2.0, pan_max / 2.0);
                        tilt_deg = tilt_deg.clamp(-tilt_max / 2.0, tilt_max / 2.0);
                    }

                    pan_data.push(pan_deg);
                    tilt_data.push(tilt_deg);
                }
            }

            state.signal_outputs.insert(
                (node.id.clone(), "pan".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data: pan_data,
                },
            );
            state.signal_outputs.insert(
                (node.id.clone(), "tilt".into()),
                Signal {
                    n,
                    t: t_steps,
                    c: 1,
                    data: tilt_data,
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
                default_text: Some("add".into()), // add, subtract, multiply, divide, max, min, abs_diff, modulo
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
            description: Some("Generates a sine wave signal in the range -1..1.".into()),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "frequency_hz".into(),
                    name: "Frequency (Hz)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.25),
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
        NodeTypeDef {
            id: "smooth_movement".into(),
            name: "Smooth Movement".into(),
            description: Some(
                "Applies a per-axis max-speed (deg/s) slew limiter to pan/tilt degrees.".into(),
            ),
            category: Some("Transform".into()),
            inputs: vec![
                PortDef {
                    id: "pan_in".into(),
                    name: "Pan (deg)".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "tilt_in".into(),
                    name: "Tilt (deg)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![
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
            params: vec![
                ParamDef {
                    id: "pan_max_deg_per_s".into(),
                    name: "Pan Max Speed (deg/s)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(360.0),
                    default_text: None,
                },
                ParamDef {
                    id: "tilt_max_deg_per_s".into(),
                    name: "Tilt Max Speed (deg/s)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(180.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "look_at_position".into(),
            name: "Look At Position".into(),
            description: Some(
                "Computes pan/tilt degrees for each selected head to aim at a target (x,y,z)."
                    .into(),
            ),
            category: Some("Transform".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "x".into(),
                    name: "Target X".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "y".into(),
                    name: "Target Y".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "z".into(),
                    name: "Target Z".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![
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
            params: vec![
                ParamDef {
                    id: "pan_offset_deg".into(),
                    name: "Pan Offset (deg)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "tilt_offset_deg".into(),
                    name: "Tilt Offset (deg)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
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
        NodeTypeDef {
            id: "orbit".into(),
            name: "Orbit".into(),
            description: Some(
                "Generates circular/elliptical position in 3D space. Outputs x, y, z coordinates."
                    .into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![PortDef {
                id: "phase".into(),
                name: "Phase Offset".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![
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
                PortDef {
                    id: "z".into(),
                    name: "Z".into(),
                    port_type: PortType::Signal,
                },
            ],
            params: vec![
                ParamDef {
                    id: "center_x".into(),
                    name: "Center X".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "center_y".into(),
                    name: "Center Y".into(),
                    param_type: ParamType::Number,
                    default_number: Some(2.0),
                    default_text: None,
                },
                ParamDef {
                    id: "center_z".into(),
                    name: "Center Z".into(),
                    param_type: ParamType::Number,
                    default_number: Some(5.0),
                    default_text: None,
                },
                ParamDef {
                    id: "radius_x".into(),
                    name: "Radius X".into(),
                    param_type: ParamType::Number,
                    default_number: Some(2.0),
                    default_text: None,
                },
                ParamDef {
                    id: "radius_z".into(),
                    name: "Radius Z".into(),
                    param_type: ParamType::Number,
                    default_number: Some(2.0),
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
                    id: "tilt_deg".into(),
                    name: "Plane Tilt (deg)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "random_position".into(),
            name: "Random Position".into(),
            description: Some(
                "Generates random positions. New position when trigger value changes."
                    .into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![PortDef {
                id: "trigger".into(),
                name: "Trigger".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![
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
                PortDef {
                    id: "z".into(),
                    name: "Z".into(),
                    port_type: PortType::Signal,
                },
            ],
            params: vec![
                ParamDef {
                    id: "min_x".into(),
                    name: "Min X".into(),
                    param_type: ParamType::Number,
                    default_number: Some(-3.0),
                    default_text: None,
                },
                ParamDef {
                    id: "max_x".into(),
                    name: "Max X".into(),
                    param_type: ParamType::Number,
                    default_number: Some(3.0),
                    default_text: None,
                },
                ParamDef {
                    id: "min_y".into(),
                    name: "Min Y".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "max_y".into(),
                    name: "Max Y".into(),
                    param_type: ParamType::Number,
                    default_number: Some(3.0),
                    default_text: None,
                },
                ParamDef {
                    id: "min_z".into(),
                    name: "Min Z".into(),
                    param_type: ParamType::Number,
                    default_number: Some(2.0),
                    default_text: None,
                },
                ParamDef {
                    id: "max_z".into(),
                    name: "Max Z".into(),
                    param_type: ParamType::Number,
                    default_number: Some(8.0),
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
    ]
}
