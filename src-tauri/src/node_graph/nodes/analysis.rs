use super::*;
use crate::models::tracks::MelSpec;
use crate::node_graph::state::RootCache;

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    let pool = ctx.pool;
    let context = ctx.graph_context;
    let context_audio_buffer = ctx.context_audio_buffer;
    let context_beat_grid = ctx.context_beat_grid;
    let compute_visualizations = ctx.compute_visualizations;
    let fft_service = ctx.fft_service;
    match node.type_id.as_str() {

            "harmony_analysis" => {
                let audio_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                    .ok_or_else(|| {
                        format!("HarmonyAnalysis node '{}' missing audio input", node.id)
                    })?;

                let audio_buffer = state.audio_buffers
                    .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!("HarmonyAnalysis node '{}' audio input unavailable", node.id)
                    })?;

                let crop_start = audio_buffer.crop.map(|c| c.start_seconds).unwrap_or(0.0);
                let crop_end = audio_buffer.crop.map(|c| c.end_seconds).unwrap_or_else(|| {
                    if audio_buffer.sample_rate == 0 {
                        0.0
                    } else {
                        audio_buffer.samples.len() as f32 / audio_buffer.sample_rate as f32
                    }
                });

                if let Some(track_id) = audio_buffer.track_id {
                    eprintln!(
                        "[harmony_analysis] '{}' processing track {} (crop {:.3?}-{:.3?})",
                        node.id, track_id, crop_start, crop_end
                    );

                    // Load harmony sections from the precomputed cache if they have not
                    // been pulled into memory yet for this run.
                    if !state.root_caches.contains_key(&track_id) {
                        eprintln!(
                            "[harmony_analysis] '{}' cache miss for track {}; loading from DB",
                            node.id, track_id
                        );
                        // Modified query to fetch logits_path
                        if let Some((sections_json, logits_path)) =
                            crate::database::local::tracks::get_track_roots(pool, track_id)
                                .await
                                .map_err(|e| format!("Failed to load chord sections: {}", e))?
                        {
                            let sections: Vec<crate::root_worker::ChordSection> =
                                serde_json::from_str(&sections_json).map_err(|e| {
                                    format!("Failed to parse chord sections: {}", e)
                                })?;

                            state.root_caches.insert(
                                track_id,
                                RootCache {
                                    sections,
                                    logits_path,
                                },
                            );
                        } else {
                            eprintln!(
                                "[harmony_analysis] '{}' no chord sections row for track {}; harmony will be empty",
                                node.id, track_id
                            );
                        }
                    } else {
                        eprintln!(
                            "[harmony_analysis] '{}' cache hit for track {}",
                            node.id, track_id
                        );
                    }
                }

                // Re-do signal generation using `state.root_caches` if available
                if let Some(track_id) = audio_buffer.track_id {
                    if let Some(cache) = state.root_caches.get(&track_id) {
                        let duration = (context.end_time - context.start_time).max(0.001);
                        let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
                        let t_steps = t_steps.max(PREVIEW_LENGTH);
                        let mut signal_data = vec![0.0; t_steps * CHROMA_DIM];

                        // Check if we have dense logits available
                        let mut used_logits = false;
                        if let Some(path_str) = &cache.logits_path {
                            let path = Path::new(path_str);
                            if path.exists() {
                                if let Ok(bytes) = std::fs::read(path) {
                                    // Parse f32 bytes
                                    // Frame size = 13 floats (13 * 4 bytes)
                                    // 13th index is "No Chord", we use 0-11 for chroma
                                    let frame_size = 13;
                                    let bytes_per_frame = frame_size * 4;

                                    if bytes.len() % bytes_per_frame == 0 {
                                        let num_frames = bytes.len() / bytes_per_frame;
                                        // Assuming hop_length=512, sr=22050 => hop ~0.023s (approx 43Hz)
                                        // We need to map graph time -> frame index
                                        // The python script reports `frame_hop_seconds`, but we don't have it cached here readily
                                        // except inside `sections_json` or `RootAnalysis` struct in worker.
                                        // We can infer or hardcode standard hop: 512/22050 ~= 0.0232199
                                        let hop_sec = 512.0 / 22050.0;

                                        for i in 0..t_steps {
                                            let t = context.start_time
                                                + (i as f32 / (t_steps - 1).max(1) as f32)
                                                    * duration;
                                            let frame_idx = (t / hop_sec).floor() as usize;

                                            if frame_idx < num_frames {
                                                let offset = frame_idx * bytes_per_frame;
                                                // Read 12 logits
                                                let mut logits = [0.0f32; 12];
                                                for c in 0..12 {
                                                    let b_start = offset + c * 4;
                                                    let b = &bytes[b_start..b_start + 4];
                                                    logits[c] =
                                                        f32::from_le_bytes(b.try_into().unwrap());
                                                }

                                                // Softmax
                                                let max_l = logits
                                                    .iter()
                                                    .fold(f32::NEG_INFINITY, |a, &b| a.max(b));
                                                let mut sum_exp = 0.0;
                                                let mut probs = [0.0f32; 12];
                                                for c in 0..12 {
                                                    probs[c] = (logits[c] - max_l).exp();
                                                    sum_exp += probs[c];
                                                }

                                                // Write to signal
                                                for c in 0..12 {
                                                    signal_data[i * CHROMA_DIM + c] =
                                                        probs[c] / sum_exp;
                                                }
                                            }
                                        }
                                        used_logits = true;
                                    }
                                }
                            }
                        }

                        if !used_logits {
                            // Fallback to naive O(T * S) rasterization of sections
                            for section in &cache.sections {
                                // Transform section time to local time
                                // Note: we don't need to snap to grid here necessarily, but keeping it consistent with Series is good.
                                // For raw signal, maybe precision is better? Let's use raw times.

                                let start_idx = ((section.start - context.start_time) / duration
                                    * t_steps as f32)
                                    .floor()
                                    as isize;
                                let end_idx = ((section.end - context.start_time) / duration
                                    * t_steps as f32)
                                    .ceil() as isize;

                                let start = start_idx.clamp(0, t_steps as isize) as usize;
                                let end = end_idx.clamp(0, t_steps as isize) as usize;

                                if start >= end {
                                    continue;
                                }

                                let mut values = vec![0.0f32; CHROMA_DIM];
                                if let Some(root) = section.root {
                                    let idx = (root as usize).min(CHROMA_DIM - 1);
                                    values[idx] = 1.0;
                                }

                                for t in start..end {
                                    for c in 0..CHROMA_DIM {
                                        signal_data[t * CHROMA_DIM + c] = values[c];
                                    }
                                }
                            }
                        }

                        state.signal_outputs.insert(
                            (node.id.clone(), "signal".into()),
                            Signal {
                                n: 1,
                                t: t_steps,
                                c: CHROMA_DIM,
                                data: signal_data,
                            },
                        );
                    }
                }
                Ok(true)
            }
            "view_signal" => {
                if compute_visualizations {
                    let input_edge = incoming_edges
                        .get(node.id.as_str())
                        .and_then(|edges| edges.first())
                        .ok_or_else(|| format!("View Signal node '{}' missing input", node.id))?;

                    let input_signal = state
                        .signal_outputs
                        .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                        .ok_or_else(|| {
                            format!("View Signal node '{}' input signal not found", node.id)
                        })?;

                    state
                        .view_results
                        .insert(node.id.clone(), input_signal.clone());
                }
                Ok(true)
            }
            "harmonic_tension" => {
                let chroma_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "chroma"))
                    .ok_or_else(|| {
                        format!("Harmonic Tension node '{}' missing chroma input", node.id)
                    })?;

                if let Some(chroma_sig) = state
                    .signal_outputs
                    .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()))
                {
                    if chroma_sig.c != 12 {
                        return Ok(true);
                    }

                    let mut out_data = vec![0.0; chroma_sig.t];
                    let max_entropy = (12.0f32).ln(); // ~2.4849

                    for t in 0..chroma_sig.t {
                        let mut entropy = 0.0;
                        for c in 0..12 {
                            let p = chroma_sig.data[t * 12 + c];
                            if p > 0.0001 {
                                entropy -= p * p.ln();
                            }
                        }
                        // Normalize 0..1
                        out_data[t] = (entropy / max_entropy).clamp(0.0, 1.0);
                    }

                    state.signal_outputs.insert(
                        (node.id.clone(), "tension".into()),
                        Signal {
                            n: 1,
                            t: chroma_sig.t,
                            c: 1,
                            data: out_data,
                        },
                    );
                }
                Ok(true)
            }
            "mel_spec_viewer" => {
                if compute_visualizations {
                    let Some(input_edge) = incoming_edges
                        .get(node.id.as_str())
                        .and_then(|edges| edges.iter().find(|e| e.to_port == "in"))
                    else {
                        eprintln!(
                            "[run_graph] mel_spec_viewer '{}' missing audio input; skipping",
                            node.id
                        );
                        return Ok(true);
                    };

                    let Some(audio_buffer) = state.audio_buffers
                        .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                    else {
                        eprintln!(
                            "[run_graph] mel_spec_viewer '{}' input audio not found; skipping",
                            node.id
                        );
                        return Ok(true);
                    };

                    // Look for optional beat grid input
                    let beat_grid = incoming_edges
                        .get(node.id.as_str())
                        .and_then(|edges| edges.iter().find(|e| e.to_port == "grid"))
                        .and_then(|grid_edge| {
                            state.beat_grids
                                .get(&(grid_edge.from_node.clone(), grid_edge.from_port.clone()))
                        })
                        .cloned()
                        .as_ref()
                        .map(|grid| crate::node_graph::context::beat_grid_relative_to_crop(grid, audio_buffer.crop.as_ref()));

                    let mel_start = std::time::Instant::now();
                    let data = generate_melspec(
                        fft_service,
                        &audio_buffer.samples,
                        audio_buffer.sample_rate,
                        MEL_SPEC_WIDTH,
                        MEL_SPEC_HEIGHT,
                    );
                    println!(
                        "[run_graph] mel_spec_viewer '{}' computed mel in {:.2?}",
                        node.id,
                        mel_start.elapsed()
                    );

                    state.mel_specs.insert(
                        node.id.clone(),
                        MelSpec {
                            width: MEL_SPEC_WIDTH,
                            height: MEL_SPEC_HEIGHT,
                            data,
                            beat_grid,
                        },
                    );
                }
                Ok(true)
            }
        _ => Ok(false),
    }
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "mel_spec_viewer".into(),
            name: "Mel Spectrogram".into(),
            description: Some("Shows the mel spectrogram for the chosen track.".into()),
            category: Some("View".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
            ],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "harmony_analysis".into(),
            name: "Harmony Analysis".into(),
            description: Some(
                "Detects chords from incoming audio and exposes a confidence timeline.".into(),
            ),
            category: Some("Audio".into()),
            inputs: vec![
                PortDef {
                    id: "audio_in".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "grid_in".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
            ],
            outputs: vec![PortDef {
                id: "signal".into(),
                name: "Chroma (Signal)".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "view_signal".into(),
            name: "View Signal".into(),
            description: Some("Displays the incoming signal (flattened to 1D preview).".into()),
            category: Some("View".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "harmonic_tension".into(),
            name: "Harmonic Tension".into(),
            description: Some("Calculates tension/dissonance from harmony spread.".into()),
            category: Some("Math".into()),
            inputs: vec![PortDef {
                id: "chroma".into(),
                name: "Chroma".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "tension".into(),
                name: "Tension".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
    ]
}
