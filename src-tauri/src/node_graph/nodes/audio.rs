use super::*;
use crate::node_graph::AudioBuffer;
use std::sync::Arc;

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    let pool = ctx.pool;
    let fft_service = ctx.fft_service;
    let stem_cache = ctx.stem_cache;
    let context = ctx.graph_context;
    let context_audio_buffer = ctx.context_audio_buffer;
    let context_beat_grid = ctx.context_beat_grid;
    let _compute_visualizations = ctx.compute_visualizations;
    match node.type_id.as_str() {
        "beat_envelope" => {
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

            let subdivision_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "subdivision"));
            let subdivision_signal = subdivision_edge.and_then(|edge| {
                state
                    .signal_outputs
                    .get(&(edge.from_node.clone(), edge.from_port.clone()))
            });

            if let Some(grid) = grid {
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
                let only_downbeats = node
                    .params
                    .get("only_downbeats")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
                    > 0.5;
                let offset = node
                    .params
                    .get("offset")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;
                let attack = node
                    .params
                    .get("attack")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.3) as f32;
                let decay = node
                    .params
                    .get("decay")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.2) as f32;
                let sustain = node
                    .params
                    .get("sustain")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.3) as f32;
                let release = node
                    .params
                    .get("release")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.2) as f32;
                let sustain_level = node
                    .params
                    .get("sustain_level")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.7) as f32;
                let a_curve = node
                    .params
                    .get("attack_curve")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;
                let d_curve = node
                    .params
                    .get("decay_curve")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;
                let amp = node
                    .params
                    .get("amplitude")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0) as f32;

                let mut pulse_times = Vec::new();
                let source_beats = if only_downbeats {
                    &grid.downbeats
                } else {
                    &grid.beats
                };

                let beat_len = if grid.bpm > 0.0 { 60.0 / grid.bpm } else { 0.5 };
                let beat_step_beats = if subdivision.abs() < 1e-3 {
                    1.0
                } else {
                    (1.0 / subdivision).abs()
                };

                if !source_beats.is_empty() {
                    let beat_step = if subdivision.abs() < 1e-3 {
                        1.0
                    } else {
                        (1.0 / subdivision).abs()
                    };

                    let last_index = (source_beats.len() - 1) as f32;
                    let mut beat_pos = 0.0;

                    while beat_pos <= last_index + 1e-4 {
                        let base_idx = beat_pos.floor() as usize;
                        let frac = beat_pos - base_idx as f32;

                        let time = if base_idx + 1 < source_beats.len() {
                            let t0 = source_beats[base_idx];
                            let t1 = source_beats[base_idx + 1];
                            t0 + (t1 - t0) * frac
                        } else {
                            source_beats[base_idx]
                        };

                        pulse_times.push(time + offset * beat_len);
                        beat_pos += beat_step.max(1e-4);
                    }
                }

                let duration = (context.end_time - context.start_time).max(0.001);
                let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
                let t_steps = t_steps.max(PREVIEW_LENGTH);

                let mut data = Vec::with_capacity(t_steps);

                let pulse_spacing = pulse_times
                    .windows(2)
                    .map(|w| (w[1] - w[0]).abs())
                    .filter(|d| *d > 1e-4)
                    .fold(None, |acc: Option<f32>, d| {
                        Some(acc.map_or(d, |a| a.min(d)))
                    });
                let pulse_span_sec = pulse_spacing.unwrap_or(beat_step_beats * beat_len);

                let (att_s, dec_s, sus_s, rel_s) =
                    adsr_durations(pulse_span_sec, attack, decay, sustain, release);

                let sample_dt = duration / (t_steps.max(1) as f32);
                let snap_eps = (sample_dt * 1.1).max(1e-6);

                if !pulse_times.is_empty() {
                    pulse_times
                        .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

                    let post_peak_span = dec_s + sus_s + rel_s;
                    if att_s > 1e-6 && post_peak_span <= 1e-6 {
                        let has_later_pulse = pulse_times
                            .iter()
                            .any(|p| *p > context.start_time + snap_eps);
                        if has_later_pulse {
                            pulse_times.retain(|p| (p - context.start_time).abs() > snap_eps);
                        }
                    }
                }

                for i in 0..t_steps {
                    let t = context.start_time + (i as f32 / t_steps.max(1) as f32) * duration;
                    let mut val = 0.0;

                    for &peak in &pulse_times {
                        if t < peak - att_s || t > peak + dec_s + sus_s + rel_s {
                            continue;
                        }

                        val += calc_envelope(
                            t,
                            peak,
                            att_s,
                            dec_s,
                            sus_s,
                            rel_s,
                            sustain_level,
                            a_curve,
                            d_curve,
                        );
                    }

                    data.push(val * amp);
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
            }
            Ok(true)
        }
        "stem_splitter" => {
            let input_edges = incoming_edges
                .get(node.id.as_str())
                .cloned()
                .unwrap_or_default();

            let audio_edge = input_edges
                .iter()
                .find(|edge| edge.to_port == "audio_in")
                .ok_or_else(|| format!("Stem splitter node '{}' missing audio input", node.id))?;

            let audio_buffer = state
                .audio_buffers
                .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                .ok_or_else(|| {
                    format!("Stem splitter node '{}' audio input unavailable", node.id)
                })?;

            if audio_buffer.samples.is_empty() {
                return Err(format!(
                    "Stem splitter node '{}' received empty audio input",
                    node.id
                ));
            }

            let crop = audio_buffer.crop.ok_or_else(|| {
                format!("Stem splitter node '{}' requires crop metadata", node.id)
            })?;

            let track_id = audio_buffer.track_id.ok_or_else(|| {
                format!(
                    "Stem splitter node '{}' requires audio sourced from a track",
                    node.id
                )
            })?;

            let track_hash = audio_buffer.track_hash.clone().ok_or_else(|| {
                format!("Stem splitter node '{}' missing track metadata", node.id)
            })?;

            let target_len = audio_buffer.samples.len();
            let target_rate = audio_buffer.sample_rate;
            if target_rate == 0 {
                return Err(format!(
                    "Stem splitter node '{}' cannot process audio with zero sample rate",
                    node.id
                ));
            }

            const STEM_OUTPUTS: [(&str, &str); 4] = [
                ("drums", "drums_out"),
                ("bass", "bass_out"),
                ("vocals", "vocals_out"),
                ("other", "other_out"),
            ];

            let mut all_cached = true;
            for (stem_name, _) in STEM_OUTPUTS {
                if stem_cache.get(track_id, stem_name).is_none() {
                    all_cached = false;
                    break;
                }
            }

            let stems_map = if !all_cached {
                let stems = crate::database::local::tracks::get_track_stems(pool, track_id)
                    .await
                    .map_err(|e| format!("Failed to load stems for track {}: {}", track_id, e))?;

                if stems.is_empty() {
                    return Err(format!(
                        "Stem splitter node '{}' requires preprocessed stems for track {}",
                        node.id, track_id
                    ));
                }
                Some(stems.into_iter().collect::<HashMap<String, String>>())
            } else {
                None
            };

            for (stem_name, port_id) in STEM_OUTPUTS {
                let (stem_samples, stem_rate) =
                    if let Some(cached) = stem_cache.get(track_id, stem_name) {
                        cached
                    } else {
                        let stems_by_name = stems_map.as_ref().unwrap();
                        let file_path = stems_by_name.get(stem_name).ok_or_else(|| {
                            format!(
                                "Stem splitter node '{}' missing '{}' stem for track {}",
                                node.id, stem_name, track_id
                            )
                        })?;

                        let cache_tag = format!("{}_stem_{}", track_hash, stem_name);
                        let (loaded_samples, loaded_rate) =
                            load_or_decode_audio(Path::new(file_path), &cache_tag, target_rate)
                                .map_err(|e| {
                                    format!(
                                        "Stem splitter node '{}' failed to decode '{}' stem: {}",
                                        node.id, stem_name, e
                                    )
                                })?;

                        if loaded_samples.is_empty() {
                            return Err(format!(
                                "Stem splitter node '{}' decoded empty '{}' stem for track {}",
                                node.id, stem_name, track_id
                            ));
                        }

                        let samples_arc = Arc::new(loaded_samples);
                        stem_cache.insert(
                            track_id,
                            stem_name.to_string(),
                            samples_arc.clone(),
                            loaded_rate,
                        );
                        (samples_arc, loaded_rate)
                    };

                let segment = crate::node_graph::context::crop_samples_to_range(
                    &stem_samples,
                    stem_rate,
                    crop,
                    target_len,
                )
                .map_err(|err| {
                    format!(
                        "Stem splitter node '{}' failed to crop '{}' stem: {}",
                        node.id, stem_name, err
                    )
                })?;

                state.audio_buffers.insert(
                    (node.id.clone(), port_id.into()),
                    AudioBuffer {
                        samples: segment,
                        sample_rate: stem_rate,
                        crop: Some(crop),
                        track_id: Some(track_id),
                        track_hash: Some(track_hash.clone()),
                    },
                );
            }
            Ok(true)
        }
        "audio_input" => {
            let audio_buf = context_audio_buffer
                .cloned()
                .ok_or_else(|| format!("Audio input node '{}' requires context audio", node.id))?;

            state
                .audio_buffers
                .insert((node.id.clone(), "out".into()), audio_buf);

            if let Some(grid) = context_beat_grid {
                state
                    .beat_grids
                    .insert((node.id.clone(), "grid_out".into()), grid.clone());
            }
            Ok(true)
        }
        "beat_clock" => {
            if let Some(grid) = context_beat_grid {
                state
                    .beat_grids
                    .insert((node.id.clone(), "grid_out".into()), grid.clone());
            }
            Ok(true)
        }
        "lowpass_filter" | "highpass_filter" => {
            let audio_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                .ok_or_else(|| {
                    format!("{} node '{}' missing audio input", node.type_id, node.id)
                })?;

            let audio_buffer = state
                .audio_buffers
                .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                .ok_or_else(|| {
                    format!(
                        "{} node '{}' audio input unavailable",
                        node.type_id, node.id
                    )
                })?;

            if audio_buffer.sample_rate == 0 {
                return Err(format!(
                    "{} node '{}' cannot process audio with zero sample rate",
                    node.type_id, node.id
                ));
            }

            let cutoff = node
                .params
                .get("cutoff_hz")
                .and_then(|v| v.as_f64())
                .unwrap_or(200.0) as f32;

            let sr = audio_buffer.sample_rate as f32;
            let nyquist = sr * 0.5;
            if nyquist <= 1.0 {
                return Err(format!(
                    "{} node '{}' has an invalid sample rate of {}",
                    node.type_id, node.id, audio_buffer.sample_rate
                ));
            }
            let max_cutoff = (nyquist - 1.0).max(1.0);
            let normalized_cutoff = cutoff.max(1.0).min(max_cutoff);

            let filtered = if node.type_id == "lowpass_filter" {
                lowpass_filter(&audio_buffer.samples, normalized_cutoff, sr)
            } else {
                highpass_filter(&audio_buffer.samples, normalized_cutoff, sr)
            };

            state.audio_buffers.insert(
                (node.id.clone(), "audio_out".into()),
                AudioBuffer {
                    samples: filtered,
                    sample_rate: audio_buffer.sample_rate,
                    crop: audio_buffer.crop,
                    track_id: audio_buffer.track_id,
                    track_hash: audio_buffer.track_hash.clone(),
                },
            );
            Ok(true)
        }
        "frequency_amplitude" => {
            let audio_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                .ok_or_else(|| {
                    format!("Frequency Amplitude node '{}' missing audio input", node.id)
                })?;

            let audio_buffer = state
                .audio_buffers
                .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                .ok_or_else(|| {
                    format!(
                        "Frequency Amplitude node '{}' audio input unavailable",
                        node.id
                    )
                })?;

            let ranges_json = node
                .params
                .get("selected_frequency_ranges")
                .and_then(|v| v.as_str())
                .unwrap_or("[]");
            let frequency_ranges: Vec<[f32; 2]> = serde_json::from_str(ranges_json)
                .map_err(|e| format!("Failed to parse frequency ranges: {}", e))?;

            let raw = calculate_frequency_amplitude(
                fft_service,
                &audio_buffer.samples,
                audio_buffer.sample_rate,
                &frequency_ranges,
            );

            state.signal_outputs.insert(
                (node.id.clone(), "amplitude_out".into()),
                Signal {
                    n: 1,
                    t: raw.len(),
                    c: 1,
                    data: raw,
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
            id: "audio_input".into(),
            name: "Audio Input".into(),
            description: Some("Context-provided audio segment for this pattern instance.".into()),
            category: Some("Input".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            params: vec![
                ParamDef {
                    id: "trackId".into(),
                    name: "Track".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "startTime".into(),
                    name: "Start Time (s)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "endTime".into(),
                    name: "End Time (s)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "beatGrid".into(),
                    name: "Beat Grid JSON".into(),
                    param_type: ParamType::Text,
                    default_number: None,
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "beat_clock".into(),
            name: "Beat Clock".into(),
            description: Some("Context-provided beat grid for this pattern instance.".into()),
            category: Some("Input".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "grid_out".into(),
                name: "Beat Grid".into(),
                port_type: PortType::BeatGrid,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "stem_splitter".into(),
            name: "Stem Splitter".into(),
            description: Some(
                "Loads cached stems for the incoming track and emits drums/bass/vocals/other."
                    .into(),
            ),
            category: Some("Audio".into()),
            inputs: vec![PortDef {
                id: "audio_in".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            outputs: vec![
                PortDef {
                    id: "drums_out".into(),
                    name: "Drums".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "bass_out".into(),
                    name: "Bass".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "vocals_out".into(),
                    name: "Vocals".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "other_out".into(),
                    name: "Other".into(),
                    port_type: PortType::Audio,
                },
            ],
            params: vec![],
        },
        NodeTypeDef {
            id: "frequency_amplitude".into(),
            name: "Frequency Amplitude".into(),
            description: Some("Extracts amplitude at a specific frequency range.".into()),
            category: Some("Audio".into()),
            inputs: vec![PortDef {
                id: "audio_in".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            outputs: vec![PortDef {
                id: "amplitude_out".into(),
                name: "Amplitude".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "selected_frequency_ranges".into(),
                name: "Frequency Ranges (JSON)".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("[]".into()),
            }],
        },
        NodeTypeDef {
            id: "lowpass_filter".into(),
            name: "Lowpass Filter".into(),
            description: Some("Applies a lowpass filter to incoming audio.".into()),
            category: Some("Audio".into()),
            inputs: vec![PortDef {
                id: "audio_in".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            outputs: vec![PortDef {
                id: "audio_out".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            params: vec![ParamDef {
                id: "cutoff_hz".into(),
                name: "Cutoff (Hz)".into(),
                param_type: ParamType::Number,
                default_number: Some(200.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "highpass_filter".into(),
            name: "Highpass Filter".into(),
            description: Some("Applies a highpass filter to incoming audio.".into()),
            category: Some("Audio".into()),
            inputs: vec![PortDef {
                id: "audio_in".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            outputs: vec![PortDef {
                id: "audio_out".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            params: vec![ParamDef {
                id: "cutoff_hz".into(),
                name: "Cutoff (Hz)".into(),
                param_type: ParamType::Number,
                default_number: Some(200.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "beat_envelope".into(),
            name: "Beat Envelope".into(),
            description: Some("Generates rhythmic envelopes aligned to the beat grid.".into()),
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
                    id: "only_downbeats".into(),
                    name: "Only Downbeats".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "offset".into(),
                    name: "Beat Offset".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "attack".into(),
                    name: "Attack Weight".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.3),
                    default_text: None,
                },
                ParamDef {
                    id: "decay".into(),
                    name: "Decay Weight".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.2),
                    default_text: None,
                },
                ParamDef {
                    id: "sustain".into(),
                    name: "Sustain Hold Weight".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.3),
                    default_text: None,
                },
                ParamDef {
                    id: "release".into(),
                    name: "Release Weight".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.2),
                    default_text: None,
                },
                ParamDef {
                    id: "sustain_level".into(),
                    name: "Sustain Level".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.7),
                    default_text: None,
                },
                ParamDef {
                    id: "attack_curve".into(),
                    name: "Attack Curve".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "decay_curve".into(),
                    name: "Decay Curve".into(),
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
            ],
        },
    ]
}
