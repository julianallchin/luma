use super::*;
use crate::node_graph::AudioBuffer;
use std::sync::Arc;

/// n2n class name → output port name for the `drum_events` node.
const DRUM_PORTS: [(&str, &str); 4] = [
    ("kick", "kick_out"),
    ("snare", "snare_out"),
    ("hat", "hat_out"),
    ("cymbal", "cymbal_out"),
];

/// Read a Signal-typed input edge and sample it at the midpoint of the
/// graph's time window. Falls back to the named param if no edge.
fn read_signal_or_param(
    incoming_edges: &std::collections::HashMap<&str, Vec<&Edge>>,
    state: &ExecutionState,
    node: &NodeInstance,
    port: &str,
    context: &GraphContext,
    default: f32,
) -> f32 {
    let edge = incoming_edges
        .get(node.id.as_str())
        .and_then(|e| e.iter().find(|x| x.to_port == port));
    if let Some(edge) = edge {
        if let Some(sig) = state
            .signal_outputs
            .get(&(edge.from_node.clone(), edge.from_port.clone()))
        {
            let mid_t = (context.start_time + context.end_time) / 2.0 - context.start_time;
            let duration = (context.end_time - context.start_time).max(0.001);
            let idx = ((mid_t / duration) * sig.data.len() as f32) as usize;
            return sig
                .data
                .get(idx.min(sig.data.len().saturating_sub(1)))
                .copied()
                .unwrap_or(default);
        }
    }
    node.params
        .get(port)
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(default)
}

/// Smallest gap between consecutive pulses, ignoring sub-millisecond pairs.
fn pulse_min_spacing(pulse_starts: &[f32]) -> Option<f32> {
    pulse_starts
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .filter(|d| *d > 1e-4)
        .fold(None, |acc: Option<f32>, d| {
            Some(acc.map_or(d, |a| a.min(d)))
        })
}

/// Build subdivision-aligned pulse start times from a beat grid. The
/// subdivision pattern stays anchored to global beat 0 (not to the first
/// element of `source_beats`, which may be sliced).
fn beat_grid_pulses(
    grid: &BeatGrid,
    subdivision: f32,
    offset: f32,
    only_downbeats: bool,
) -> Vec<f32> {
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

    let mut pulse_starts = Vec::new();
    if source_beats.is_empty() {
        return pulse_starts;
    }
    let beat_step = beat_step_beats.max(1e-4);
    let last_index = (source_beats.len() - 1) as f32;

    let global_beat_0 = if beat_len > 1e-6 {
        ((source_beats[0] - grid.downbeat_offset) / beat_len).round()
    } else {
        0.0
    };
    let phase = global_beat_0 % beat_step;
    let start_offset = if phase.abs() < 1e-4 {
        0.0
    } else {
        beat_step - phase
    };
    let mut beat_pos = start_offset;

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
        pulse_starts.push(time + offset * beat_len);
        beat_pos += beat_step;
    }
    pulse_starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    pulse_starts
}

/// Parameters controlling how an event/pulse stream is shaped into a Signal.
///
/// A/D/S/R are unitless ratios that sum to ~1 and define the *shape* of the
/// envelope. The total time the envelope spans is `span_sec`, computed from
/// `length_source`, `length_beats`, and the optional inter-pulse gap.
struct AdsrParams {
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    sustain_level: f32,
    a_curve: f32,
    d_curve: f32,
    amp: f32,
    /// When true, the envelope spans the inter-pulse gap (current
    /// `beat_envelope` semantics). When false, it spans `length_beats`.
    fit_to_gap: bool,
    /// Envelope length in beats. Used directly in fixed mode, and as a
    /// fallback in fit-to-gap mode when the input has fewer than two pulses.
    length_beats: f32,
    /// Track BPM (used to convert `length_beats` → seconds). Falls back to
    /// 120 BPM when the graph runs without a beat grid.
    bpm: f32,
}

impl AdsrParams {
    fn fixed_length_sec(&self) -> f32 {
        let bpm = self.bpm.max(1e-3);
        (self.length_beats * 60.0 / bpm).max(1e-3)
    }
}

/// Render an ADSR-shaped Signal triggered by a list of pulse start times.
/// Overlapping tails are resolved by `max(prev_env, current_env)` so a new
/// trigger doesn't punch a hole in the previous release.
fn sample_adsr_signal(pulse_starts: &[f32], params: &AdsrParams, context: &GraphContext) -> Signal {
    let duration = (context.end_time - context.start_time).max(0.001);
    let mut t_steps = (duration * SIMULATION_RATE).ceil() as usize;
    t_steps = t_steps.max(PREVIEW_LENGTH);

    let span_sec = if params.fit_to_gap {
        pulse_min_spacing(pulse_starts).unwrap_or_else(|| params.fixed_length_sec())
    } else {
        params.fixed_length_sec()
    };
    let (att_s, dec_s, sus_s, rel_s) = adsr_durations(
        span_sec,
        params.attack,
        params.decay,
        params.sustain,
        params.release,
    );
    let shape_len = att_s + dec_s + sus_s + rel_s;

    // Anti-alias: at high subdivisions the pulse period can be much shorter
    // than 1/SIMULATION_RATE, causing irregular peak heights. Ensure at least
    // 32 samples per pulse cycle, capped to bound allocation.
    if let Some(pulse_min) = pulse_min_spacing(pulse_starts) {
        if pulse_min > 1e-4 {
            let pulse_count = (duration / pulse_min).ceil() as usize;
            t_steps = t_steps.max(pulse_count * 32).min(16_384);
        }
    }

    let mut data = Vec::with_capacity(t_steps);
    let shape_eps = shape_len + 1e-3;
    for i in 0..t_steps {
        let t = context.start_time + (i as f32 / t_steps.max(1) as f32) * duration;
        // Attack leads up to the event: include pulses whose attack has started
        // (up to att_s in the future) so the peak lands on the event itself.
        let idx = pulse_starts.partition_point(|&p| p <= t + att_s);
        let val = if idx > 0 {
            let dt = t - pulse_starts[idx - 1] + att_s;
            let current = if dt <= shape_eps {
                calc_envelope(
                    dt,
                    att_s,
                    dec_s,
                    sus_s,
                    rel_s,
                    params.sustain_level,
                    params.a_curve,
                    params.d_curve,
                )
            } else {
                0.0
            };
            if idx >= 2 {
                let dt_prev = t - pulse_starts[idx - 2] + att_s;
                if dt_prev <= shape_eps {
                    let prev = calc_envelope(
                        dt_prev,
                        att_s,
                        dec_s,
                        sus_s,
                        rel_s,
                        params.sustain_level,
                        params.a_curve,
                        params.d_curve,
                    );
                    current.max(prev)
                } else {
                    current
                }
            } else {
                current
            }
        } else {
            0.0
        };
        data.push(val * params.amp);
    }

    Signal {
        n: 1,
        t: t_steps,
        c: 1,
        data,
    }
}

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
    let context_drum_onsets = ctx.context_drum_onsets;
    let _compute_visualizations = ctx.compute_visualizations;
    match node.type_id.as_str() {
        "beat_envelope" => {
            // Convenience macro: equivalent to `beat_pulses → adsr(fit_to_gap)`.
            // Kept as a primitive for back-compat with existing DSL files.
            if let Some(grid) = context_beat_grid {
                let subdivision =
                    read_signal_or_param(incoming_edges, state, node, "subdivision", context, 1.0);
                let offset =
                    read_signal_or_param(incoming_edges, state, node, "offset", context, 0.0);
                let only_downbeats = node
                    .params
                    .get("only_downbeats")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
                    > 0.5;

                let pulse_starts = beat_grid_pulses(grid, subdivision, offset, only_downbeats);

                let beat_step_beats = if subdivision.abs() < 1e-3 {
                    1.0
                } else {
                    (1.0 / subdivision).abs()
                };

                let params = AdsrParams {
                    attack: node
                        .params
                        .get("attack")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.3) as f32,
                    decay: node
                        .params
                        .get("decay")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.2) as f32,
                    sustain: node
                        .params
                        .get("sustain")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.3) as f32,
                    release: node
                        .params
                        .get("release")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.2) as f32,
                    sustain_level: node
                        .params
                        .get("sustain_level")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.7) as f32,
                    a_curve: node
                        .params
                        .get("attack_curve")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32,
                    d_curve: node
                        .params
                        .get("decay_curve")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32,
                    amp: node
                        .params
                        .get("amplitude")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0) as f32,
                    fit_to_gap: true,
                    length_beats: beat_step_beats,
                    bpm: if grid.bpm > 0.0 { grid.bpm } else { 120.0 },
                };

                let signal = sample_adsr_signal(&pulse_starts, &params, context);
                state
                    .signal_outputs
                    .insert((node.id.clone(), "out".into()), signal);
            }
            Ok(true)
        }
        "beat_pulses" => {
            if let Some(grid) = context_beat_grid {
                let subdivision =
                    read_signal_or_param(incoming_edges, state, node, "subdivision", context, 1.0);
                let offset =
                    read_signal_or_param(incoming_edges, state, node, "offset", context, 0.0);
                let only_downbeats = node
                    .params
                    .get("only_downbeats")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
                    > 0.5;
                let pulses = beat_grid_pulses(grid, subdivision, offset, only_downbeats);
                state
                    .event_outputs
                    .insert((node.id.clone(), "events_out".into()), pulses);
            } else {
                state
                    .event_outputs
                    .insert((node.id.clone(), "events_out".into()), Vec::new());
            }
            Ok(true)
        }
        "drum_events" => {
            for (class, port) in DRUM_PORTS {
                let times = context_drum_onsets
                    .and_then(|onsets| onsets.get(class))
                    .cloned()
                    .unwrap_or_default();
                let mut sorted = times;
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                state
                    .event_outputs
                    .insert((node.id.clone(), port.into()), sorted);
            }
            Ok(true)
        }
        "adsr" => {
            let events_edge = incoming_edges
                .get(node.id.as_str())
                .and_then(|e| e.iter().find(|x| x.to_port == "events_in"));
            let pulse_starts: Vec<f32> = events_edge
                .and_then(|edge| {
                    state
                        .event_outputs
                        .get(&(edge.from_node.clone(), edge.from_port.clone()))
                })
                .cloned()
                .unwrap_or_default();

            let bpm = context_beat_grid
                .map(|g| if g.bpm > 0.0 { g.bpm } else { 120.0 })
                .unwrap_or(120.0);

            let params = AdsrParams {
                attack: node
                    .params
                    .get("attack")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.1) as f32,
                decay: node
                    .params
                    .get("decay")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.4) as f32,
                sustain: node
                    .params
                    .get("sustain")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32,
                release: node
                    .params
                    .get("release")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5) as f32,
                sustain_level: node
                    .params
                    .get("sustain_level")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5) as f32,
                a_curve: node
                    .params
                    .get("attack_curve")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32,
                d_curve: node
                    .params
                    .get("decay_curve")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32,
                amp: node
                    .params
                    .get("amplitude")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0) as f32,
                fit_to_gap: node
                    .params
                    .get("fit_to_gap")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
                    > 0.5,
                length_beats: node
                    .params
                    .get("length_beats")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5) as f32,
                bpm,
            };

            let signal = sample_adsr_signal(&pulse_starts, &params, context);
            state
                .signal_outputs
                .insert((node.id.clone(), "signal_out".into()), signal);
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

            let track_id = audio_buffer.track_id.clone().ok_or_else(|| {
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

            // Fetch stem file paths from DB only if any stem is uncached
            let stems_map: Option<Arc<HashMap<String, String>>> = {
                let any_uncached = STEM_OUTPUTS
                    .iter()
                    .any(|(name, _)| stem_cache.get(&track_id, name).is_none());
                if any_uncached {
                    let stems = crate::database::local::tracks::get_track_stems(pool, &track_id)
                        .await
                        .map_err(|e| {
                            format!("Failed to load stems for track {}: {}", track_id, e)
                        })?;
                    if stems.is_empty() {
                        return Err(format!(
                            "Stem splitter node '{}' requires preprocessed stems for track {}",
                            node.id, track_id
                        ));
                    }
                    Some(Arc::new(
                        stems
                            .into_iter()
                            .map(|s| (s.stem_name, s.file_path))
                            .collect(),
                    ))
                } else {
                    None
                }
            };

            // Load all 4 stems concurrently to avoid serializing disk I/O
            let mut join_set = tokio::task::JoinSet::new();
            for (stem_name, port_id) in STEM_OUTPUTS {
                let node_id = node.id.clone();
                let track_hash_cl = track_hash.clone();
                let track_id_borrow = track_id.clone();
                let track_id_cl = track_id.clone();
                let stems_map_ref = stems_map.clone();
                let stem_cache = stem_cache.clone();

                join_set.spawn(async move {
                    let (stem_samples, stem_rate) = stem_cache
                        .get_or_load(&track_id_borrow, stem_name, move || {
                            let stems_by_name = stems_map_ref.as_ref().unwrap();
                            let file_path = stems_by_name.get(stem_name).ok_or_else(|| {
                                format!(
                                    "Stem splitter node '{}' missing '{}' stem for track {}",
                                    node_id, stem_name, track_id_cl
                                )
                            })?;

                            let cache_tag = format!("{}_stem_{}", track_hash_cl, stem_name);
                            let audio =
                                load_or_decode_audio(Path::new(file_path), &cache_tag, target_rate)
                                    .map_err(|e| {
                                        format!(
                                    "Stem splitter node '{}' failed to decode '{}' stem: {}",
                                    node_id, stem_name, e
                                )
                                    })?;

                            if audio.samples.is_empty() {
                                return Err(format!(
                                    "Stem splitter node '{}' decoded empty '{}' stem for track {}",
                                    node_id, stem_name, track_id_cl
                                ));
                            }

                            let mono_samples = stereo_to_mono(&audio.samples);
                            Ok((Arc::new(mono_samples), audio.sample_rate))
                        })
                        .await?;
                    Ok::<_, String>((stem_name, port_id, stem_samples, stem_rate))
                });
            }

            let mut results = Vec::new();
            while let Some(result) = join_set.join_next().await {
                results.push(result.map_err(|e| format!("Stem load task panicked: {}", e))?);
            }
            for result in results {
                let (stem_name, port_id, stem_samples, stem_rate) = result?;

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
                        samples: std::sync::Arc::new(segment),
                        sample_rate: stem_rate,
                        crop: Some(crop),
                        track_id: Some(track_id.clone()),
                        track_hash: Some(track_hash.clone()),
                    },
                );
            }
            Ok(true)
        }
        "audio_input" => {
            // Use provided context audio, or fall back to silence when compiling
            // for the simulated deck (no real track loaded).
            let audio_buf = context_audio_buffer
                .cloned()
                .unwrap_or_else(|| AudioBuffer {
                    samples: std::sync::Arc::new(Vec::new()),
                    sample_rate: 44100,
                    crop: None,
                    track_id: None,
                    track_hash: None,
                });

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
                    samples: std::sync::Arc::new(filtered),
                    sample_rate: audio_buffer.sample_rate,
                    crop: audio_buffer.crop,
                    track_id: audio_buffer.track_id.clone(),
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
                    id: "subdivision".into(),
                    name: "Subdivision".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "offset".into(),
                    name: "Beat Offset".into(),
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
        NodeTypeDef {
            id: "beat_pulses".into(),
            name: "Beat Pulses".into(),
            description: Some(
                "Emits subdivision-aligned pulse timestamps from the beat grid. Feed into ADSR for shaping."
                    .into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![
                PortDef {
                    id: "subdivision".into(),
                    name: "Subdivision".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "offset".into(),
                    name: "Beat Offset".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "events_out".into(),
                name: "Events".into(),
                port_type: PortType::Events,
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
            ],
        },
        NodeTypeDef {
            id: "drum_events".into(),
            name: "Drum Events".into(),
            description: Some(
                "Per-class drum onset timestamps from the n2n preprocessor.".into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![
                PortDef {
                    id: "kick_out".into(),
                    name: "Kick".into(),
                    port_type: PortType::Events,
                },
                PortDef {
                    id: "snare_out".into(),
                    name: "Snare".into(),
                    port_type: PortType::Events,
                },
                PortDef {
                    id: "hat_out".into(),
                    name: "Hi-Hat".into(),
                    port_type: PortType::Events,
                },
                PortDef {
                    id: "cymbal_out".into(),
                    name: "Cymbal".into(),
                    port_type: PortType::Events,
                },
            ],
            params: vec![],
        },
        NodeTypeDef {
            id: "adsr".into(),
            name: "ADSR Envelope".into(),
            description: Some(
                "Shapes incoming events into an ADSR envelope. A/D/S/R are unitless ratios that sum to ~1; total duration is `Length` in beats (or the inter-event gap if Fit To Gap is on)."
                    .into(),
            ),
            category: Some("Signal".into()),
            inputs: vec![PortDef {
                id: "events_in".into(),
                name: "Events".into(),
                port_type: PortType::Events,
            }],
            outputs: vec![PortDef {
                id: "signal_out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "attack".into(),
                    name: "Attack Ratio".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.1),
                    default_text: None,
                },
                ParamDef {
                    id: "decay".into(),
                    name: "Decay Ratio".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.4),
                    default_text: None,
                },
                ParamDef {
                    id: "sustain".into(),
                    name: "Sustain Ratio".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "release".into(),
                    name: "Release Ratio".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
                ParamDef {
                    id: "sustain_level".into(),
                    name: "Sustain Level".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
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
                ParamDef {
                    id: "length_beats".into(),
                    name: "Length (beats)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
                ParamDef {
                    id: "fit_to_gap".into(),
                    name: "Fit To Gap (0/1)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
    ]
}
