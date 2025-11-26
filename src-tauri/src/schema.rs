use crate::audio::{generate_melspec, load_or_decode_audio, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH};
use crate::database::Db;
pub use crate::models::schema::*;
use crate::models::tracks::MelSpec;
use crate::tracks::TARGET_SAMPLE_RATE;
use chord_detector::{Chord, ChordDetector, ChordKind, Chromagram, NoteName};
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde_json;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
use tauri::State;

const CHROMA_WINDOW: usize = 1024;
const CHROMA_HOP: usize = 1024;
const CHROMA_DIM: usize = 12;
const PITCH_CLASS_LABELS: [&str; CHROMA_DIM] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
const MAX_CHORD_CHOICES: usize = CHROMA_DIM;
const RMS_THRESHOLD: f32 = 1e-4;

fn crop_samples_to_range(
    samples: &[f32],
    sample_rate: u32,
    crop: AudioCrop,
    target_len: usize,
) -> Result<Vec<f32>, String> {
    if sample_rate == 0 {
        return Err("Cannot crop audio with zero sample rate".into());
    }
    if samples.is_empty() {
        return Err("Cannot crop audio with no samples".into());
    }
    if target_len == 0 {
        return Ok(Vec::new());
    }

    let mut start_sample = (crop.start_seconds * sample_rate as f32).floor().max(0.0) as usize;
    start_sample = start_sample.min(samples.len().saturating_sub(1));
    let mut end_sample = (crop.end_seconds * sample_rate as f32).ceil() as usize;
    end_sample = end_sample.min(samples.len());

    if end_sample <= start_sample {
        return Err("Computed invalid crop window for stem data".into());
    }

    let mut segment = samples[start_sample..end_sample].to_vec();
    if segment.len() > target_len {
        segment.truncate(target_len);
    } else if segment.len() < target_len {
        segment.resize(target_len, 0.0);
    }

    Ok(segment)
}

fn estimate_zero_crossing_frequency(chunk: &[f32], sample_rate: u32) -> f32 {
    if sample_rate == 0 || chunk.len() < 2 {
        return 0.0;
    }

    let mut crossings = 0u32;
    for window in chunk.windows(2) {
        let prev = window[0];
        let curr = window[1];
        if (prev <= 0.0 && curr > 0.0) || (prev >= 0.0 && curr < 0.0) {
            crossings += 1;
        }
    }

    let duration = chunk.len() as f32 / sample_rate as f32;
    if duration <= 0.0 {
        return 0.0;
    }

    crossings as f32 / (2.0 * duration)
}

fn normalize_frequency_to_brightness(freq: f32) -> f32 {
    if freq <= 0.0 {
        return 0.5;
    }
    let min_freq = 80.0;
    let max_freq = 2000.0;
    ((freq - min_freq) / (max_freq - min_freq)).clamp(0.0, 1.0)
}

fn estimate_segment_brightness(
    samples: &[f32],
    sample_rate: u32,
    start_time: f32,
    end_time: f32,
) -> f32 {
    if sample_rate == 0 || start_time.is_nan() || end_time.is_nan() {
        return 0.5;
    }
    let mut start_idx = (start_time * sample_rate as f32).floor() as usize;
    let mut end_idx = (end_time * sample_rate as f32).ceil() as usize;
    if start_idx >= samples.len() {
        start_idx = samples.len().saturating_sub(1);
    }
    if end_idx > samples.len() {
        end_idx = samples.len();
    }
    if end_idx <= start_idx + 1 {
        return 0.5;
    }

    let chunk = &samples[start_idx..end_idx];
    let freq = estimate_zero_crossing_frequency(chunk, sample_rate);
    normalize_frequency_to_brightness(freq)
}

// Graph execution returns preview data (channels, mel specs, series, colors).
#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "sample_pattern".into(),
            name: "Sample Pattern".into(),
            description: Some("Generates a repeating kick-style intensity envelope.".into()),
            category: Some("Sources".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "threshold".into(),
            name: "Threshold".into(),
            description: Some("Outputs 1.0 when input exceeds threshold, otherwise 0.0.".into()),
            category: Some("Modifiers".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
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
            id: "audio_input".into(),
            name: "Audio Input".into(),
            description: Some("Context-provided audio segment for this pattern instance.".into()),
            category: Some("Context".into()),
            inputs: vec![],
            outputs: vec![
                PortDef {
                    id: "out".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
            ],
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
            id: "audio_passthrough".into(),
            name: "Audio Passthrough".into(),
            description: Some(
                "Forwards audio (and optional beat grid) without modification.".into(),
            ),
            category: Some("Utilities".into()),
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
            outputs: vec![
                PortDef {
                    id: "audio_out".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "grid_out".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
            ],
            params: vec![],
        },
        NodeTypeDef {
            id: "stem_splitter".into(),
            name: "Stem Splitter".into(),
            description: Some(
                "Loads cached stems for the incoming track and emits drums/bass/vocals/other."
                    .into(),
            ),
            category: Some("Modifiers".into()),
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
            id: "beat_clock".into(),
            name: "Beat Clock".into(),
            description: Some("Context-provided beat grid for this pattern instance.".into()),
            category: Some("Context".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "grid_out".into(),
                name: "Beat Grid".into(),
                port_type: PortType::BeatGrid,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "mel_spec_viewer".into(),
            name: "Mel Spectrogram".into(),
            description: Some("Shows the mel spectrogram for the chosen track.".into()),
            category: Some("Visualization".into()),
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
            category: Some("Analysis / Audio".into()),
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
                id: "chroma".into(),
                name: "Harmony".into(),
                port_type: PortType::Series,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "view_channel".into(),
            name: "View Channel".into(),
            description: Some("Displays the incoming intensity channel.".into()),
            category: Some("Utilities".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Channel".into(),
                    port_type: PortType::Intensity,
                },
                PortDef {
                    id: "series_in".into(),
                    name: "Series".into(),
                    port_type: PortType::Series,
                },
            ],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "apply_zone_dimmer".into(),
            name: "Apply Zone Dimmer".into(),
            description: Some("Marks the intensity channel for output to a zone dimmer.".into()),
            category: Some("Outputs".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
            outputs: vec![],
            params: vec![ParamDef {
                id: "zone".into(),
                name: "Zone".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("Main".into()),
            }],
        },
        NodeTypeDef {
            id: "color".into(),
            name: "Color".into(),
            description: Some("Outputs a color value.".into()),
            category: Some("Sources".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Color,
            }],
            params: vec![ParamDef {
                id: "color".into(),
                name: "Color".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(r#"{"r":255,"g":0,"b":0,"a":1}"#.into()),
            }],
        },
        NodeTypeDef {
            id: "harmony_color_visualizer".into(),
            name: "Harmony Color Visualizer".into(),
            description: Some("Visualizes harmony analysis as colors using a generated palette from a base color.".into()),
            category: Some("Visualizers".into()),
            inputs: vec![
                PortDef {
                    id: "harmony_in".into(),
                    name: "Harmony".into(),
                    port_type: PortType::Series,
                },
                PortDef {
                    id: "color_in".into(),
                    name: "Base Color".into(),
                    port_type: PortType::Color,
                },
                PortDef {
                    id: "audio_in".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
            ],
            outputs: vec![],
            params: vec![ParamDef {
                id: "palette_size".into(),
                name: "Palette Size".into(),
                param_type: ParamType::Number,
                default_number: Some(4.0),
                default_text: None,
            }],
        },
    ]
}

#[tauri::command]
pub async fn run_graph(
    db: State<'_, Db>,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    run_graph_internal(&db.0, graph, context).await
}

async fn run_graph_internal(
    pool: &SqlitePool,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    println!("Received graph with {} nodes to run.", graph.nodes.len());

    if graph.nodes.is_empty() {
        return Ok(RunResult {
            views: HashMap::new(),
            series_views: HashMap::new(),
            mel_specs: HashMap::new(),
            color_views: HashMap::new(),
        });
    }

    const PREVIEW_LENGTH: usize = 256;

    let nodes_by_id: HashMap<&str, &NodeInstance> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut dependency_graph: DiGraph<&str, ()> = DiGraph::new();
    let mut node_indices = HashMap::new();

    for node in &graph.nodes {
        let idx = dependency_graph.add_node(node.id.as_str());
        node_indices.insert(node.id.as_str(), idx);
    }

    for edge in &graph.edges {
        let Some(&from_idx) = node_indices.get(edge.from_node.as_str()) else {
            return Err(format!("Unknown from_node '{}' in edge", edge.from_node));
        };
        let Some(&to_idx) = node_indices.get(edge.to_node.as_str()) else {
            return Err(format!("Unknown to_node '{}' in edge", edge.to_node));
        };
        dependency_graph.add_edge(from_idx, to_idx, ());
    }

    let sorted = toposort(&dependency_graph, None)
        .map_err(|_| "Graph has a cycle. Execution aborted.".to_string())?;

    let mut incoming_edges: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        incoming_edges
            .entry(edge.to_node.as_str())
            .or_default()
            .push(edge);
    }

    #[derive(Clone)]
    struct AudioBuffer {
        samples: Vec<f32>,
        sample_rate: u32,
        crop: Option<AudioCrop>,
        track_id: Option<i64>,
        track_hash: Option<String>,
    }

    #[derive(Clone)]
    struct RootCache {
        sections: Vec<crate::root_worker::ChordSection>,
    }

    let mut output_buffers: HashMap<(String, String), Vec<f32>> = HashMap::new();
    let mut audio_buffers: HashMap<(String, String), AudioBuffer> = HashMap::new();
    let mut beat_grids: HashMap<(String, String), BeatGrid> = HashMap::new();

    // === Lazy context loading ===
    // Only load audio if graph contains nodes that need it (audio_input, beat_clock, etc.)
    let needs_context = graph.nodes.iter().any(|n| {
        matches!(
            n.type_id.as_str(),
            "audio_input" | "beat_clock" | "stem_splitter" | "harmony_analysis"
        )
    });

    // Context data - loaded lazily
    let (
        context_audio_buffer,
        _context_samples,
        _context_sample_rate,
        _context_duration,
        context_beat_grid,
        _context_track_hash,
    ): (
        Option<AudioBuffer>,
        Vec<f32>,
        u32,
        f32,
        Option<BeatGrid>,
        Option<String>,
    ) = if needs_context {
        let track_row: Option<(String, String)> =
            sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
                .bind(context.track_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("Failed to fetch track path: {}", e))?;

        let (context_file_path, track_hash) =
            track_row.ok_or_else(|| format!("Track {} not found", context.track_id))?;

        let context_path = Path::new(&context_file_path);
        let (context_full_samples, sample_rate) =
            load_or_decode_audio(context_path, &track_hash, TARGET_SAMPLE_RATE)
                .map_err(|e| format!("Failed to decode track: {}", e))?;

        if context_full_samples.is_empty() || sample_rate == 0 {
            return Err("Context track has no audio data".into());
        }

        // Slice to the context time range
        let ctx_start_sample = (context.start_time * sample_rate as f32).floor().max(0.0) as usize;
        let ctx_end_sample = if context.end_time > 0.0 {
            (context.end_time * sample_rate as f32).ceil() as usize
        } else {
            context_full_samples.len()
        };
        let samples = if ctx_start_sample >= context_full_samples.len() {
            Vec::new()
        } else {
            let capped_end = ctx_end_sample.min(context_full_samples.len());
            context_full_samples[ctx_start_sample..capped_end].to_vec()
        };

        if samples.is_empty() {
            return Err("Context time range produced empty audio segment".into());
        }

        let duration = samples.len() as f32 / sample_rate as f32;

        let audio_buffer = AudioBuffer {
            samples: samples.clone(),
            sample_rate,
            crop: Some(AudioCrop {
                start_seconds: context.start_time,
                end_seconds: context.end_time.max(context.start_time + duration),
            }),
            track_id: Some(context.track_id),
            track_hash: Some(track_hash.clone()),
        };

        // Load beat grid from context or fallback to DB
        let beat_grid: Option<BeatGrid> = if let Some(grid) = context.beat_grid.clone() {
            Some(grid)
        } else {
            if let Some((beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)) =
                sqlx::query_as::<_, (String, String, Option<f64>, Option<f64>, Option<i64>)>(
                    "SELECT beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar FROM track_beats WHERE track_id = ?",
                )
                .bind(context.track_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("Failed to load beat data: {}", e))?
            {
                let beats: Vec<f32> = serde_json::from_str(&beats_json)
                    .map_err(|e| format!("Failed to parse beats data: {}", e))?;
                let downbeats: Vec<f32> = serde_json::from_str(&downbeats_json)
                    .map_err(|e| format!("Failed to parse downbeats data: {}", e))?;
                let (fallback_bpm, fallback_offset, fallback_bpb) =
                    crate::tracks::infer_grid_metadata(&beats, &downbeats);
                let bpm_value = bpm.unwrap_or(fallback_bpm as f64) as f32;
                let offset_value = downbeat_offset.unwrap_or(fallback_offset as f64) as f32;
                let bpb_value = beats_per_bar.unwrap_or(fallback_bpb as i64) as i32;
                Some(BeatGrid {
                    beats,
                    downbeats,
                    bpm: bpm_value,
                    downbeat_offset: offset_value,
                    beats_per_bar: bpb_value,
                })
            } else {
                None
            }
        };

        (
            Some(audio_buffer),
            samples,
            sample_rate,
            duration,
            beat_grid,
            Some(track_hash),
        )
    } else {
        // No context needed - use empty defaults
        (None, Vec::new(), 0, 0.0, None, None)
    };

    let mut series_outputs: HashMap<(String, String), Series> = HashMap::new();
    let mut color_outputs: HashMap<(String, String), String> = HashMap::new();
    let mut root_caches: HashMap<i64, RootCache> = HashMap::new();
    let mut view_results: HashMap<String, Vec<f32>> = HashMap::new();
    let mut mel_specs: HashMap<String, MelSpec> = HashMap::new();
    let mut series_views: HashMap<String, Series> = HashMap::new();
    let mut color_views: HashMap<String, String> = HashMap::new();

    for node_idx in sorted {
        let node_id = dependency_graph[node_idx];
        let node = nodes_by_id
            .get(node_id)
            .copied()
            .ok_or_else(|| format!("Node '{}' not found during execution", node_id))?;

        match node.type_id.as_str() {
            "sample_pattern" => {
                let mut buffer = vec![0.0f32; PREVIEW_LENGTH];

                for start in (0..PREVIEW_LENGTH).step_by(64) {
                    buffer[start] = 1.0;
                    if start + 1 < PREVIEW_LENGTH {
                        buffer[start + 1] = 0.5;
                    }
                    if start + 2 < PREVIEW_LENGTH {
                        buffer[start + 2] = 0.2;
                    }
                }

                output_buffers.insert((node.id.clone(), "out".into()), buffer);
            }
            "stem_splitter" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();

                let audio_edge = input_edges
                    .iter()
                    .find(|edge| edge.to_port == "audio_in")
                    .ok_or_else(|| {
                        format!("Stem splitter node '{}' missing audio input", node.id)
                    })?;

                let audio_buffer = audio_buffers
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

                let stems: Vec<(String, String)> = sqlx::query_as(
                    "SELECT stem_name, file_path FROM track_stems WHERE track_id = ?",
                )
                .bind(track_id)
                .fetch_all(pool)
                .await
                .map_err(|e| format!("Failed to load stems for track {}: {}", track_id, e))?;

                if stems.is_empty() {
                    return Err(format!(
                        "Stem splitter node '{}' requires preprocessed stems for track {}",
                        node.id, track_id
                    ));
                }

                let stems_by_name: HashMap<String, String> = stems.into_iter().collect();
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

                for (stem_name, port_id) in STEM_OUTPUTS {
                    let file_path = stems_by_name.get(stem_name).ok_or_else(|| {
                        format!(
                            "Stem splitter node '{}' missing '{}' stem for track {}",
                            node.id, stem_name, track_id
                        )
                    })?;

                    let cache_tag = format!("{}_stem_{}", track_hash, stem_name);
                    let (stem_samples, stem_rate) =
                        load_or_decode_audio(Path::new(file_path), &cache_tag, target_rate)
                            .map_err(|e| {
                                format!(
                                    "Stem splitter node '{}' failed to decode '{}' stem: {}",
                                    node.id, stem_name, e
                                )
                            })?;

                    if stem_samples.is_empty() {
                        return Err(format!(
                            "Stem splitter node '{}' decoded empty '{}' stem for track {}",
                            node.id, stem_name, track_id
                        ));
                    }

                    let segment = crop_samples_to_range(&stem_samples, stem_rate, crop, target_len)
                        .map_err(|err| {
                            format!(
                                "Stem splitter node '{}' failed to crop '{}' stem: {}",
                                node.id, stem_name, err
                            )
                        })?;

                    audio_buffers.insert(
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
            }
            "audio_input" => {
                // Audio input reads from pre-loaded context (host responsibility)
                // The node is a pure passthrough - no DB access, no file loading, no playback registration
                let audio_buf = context_audio_buffer.clone().ok_or_else(|| {
                    format!("Audio input node '{}' requires context audio", node.id)
                })?;

                audio_buffers.insert((node.id.clone(), "out".into()), audio_buf);

                // Beat grid from context
                if let Some(ref grid) = context_beat_grid {
                    beat_grids.insert((node.id.clone(), "grid_out".into()), grid.clone());
                }
            }
            "audio_passthrough" => {
                let audio_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                    .ok_or_else(|| {
                        format!("Audio passthrough node '{}' missing audio input", node.id)
                    })?;

                let audio_buffer = audio_buffers
                    .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!(
                            "Audio passthrough node '{}' audio input unavailable",
                            node.id
                        )
                    })?
                    .clone();

                audio_buffers.insert((node.id.clone(), "audio_out".into()), audio_buffer.clone());

                if let Some(grid_edge) = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "grid_in"))
                {
                    let grid = beat_grids
                        .get(&(grid_edge.from_node.clone(), grid_edge.from_port.clone()))
                        .ok_or_else(|| {
                            format!(
                                "Audio passthrough node '{}' beat grid input unavailable",
                                node.id
                            )
                        })?
                        .clone();

                    beat_grids.insert((node.id.clone(), "grid_out".into()), grid);
                }
            }
            "beat_clock" => {
                // Beat clock now reads from pre-loaded context (host responsibility)
                if let Some(ref grid) = context_beat_grid {
                    beat_grids.insert((node.id.clone(), "grid_out".into()), grid.clone());
                }
            }
            "threshold" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.first())
                    .ok_or_else(|| format!("Threshold node '{}' missing input", node.id))?;

                let input_buffer = output_buffers
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!("Threshold node '{}' input buffer not found", node.id)
                    })?;

                let threshold = node
                    .params
                    .get("threshold")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.5) as f32;

                let mut output = Vec::with_capacity(PREVIEW_LENGTH);
                for &sample in input_buffer.iter().take(PREVIEW_LENGTH) {
                    output.push(if sample >= threshold { 1.0 } else { 0.0 });
                }

                output_buffers.insert((node.id.clone(), "out".into()), output);
            }
            "view_channel" => {
                let mut consumed = false;

                if let Some(series_edge) = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "series_in"))
                {
                    if let Some(series) = series_outputs
                        .get(&(series_edge.from_node.clone(), series_edge.from_port.clone()))
                    {
                        series_views.insert(node.id.clone(), series.clone());
                        consumed = true;
                    } else {
                        eprintln!(
                            "[run_graph] view_channel '{}' missing series payload on '{}'",
                            node.id, series_edge.from_node
                        );
                    }
                }

                if consumed {
                    continue;
                }

                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.first())
                    .ok_or_else(|| format!("View node '{}' missing input", node.id))?;

                let input_buffer = output_buffers
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                    .ok_or_else(|| format!("View node '{}' input buffer not found", node.id))?;

                view_results.insert(node.id.clone(), input_buffer.clone());
            }
            "apply_zone_dimmer" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.first())
                    .ok_or_else(|| format!("Zone dimmer node '{}' missing input", node.id))?;

                let _ = output_buffers
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!("Zone dimmer node '{}' input buffer not found", node.id)
                    })?;

                if let Some(zone) = node.params.get("zone").and_then(|value| value.as_str()) {
                    println!("Zone '{}' dimmer updated from node '{}'", zone, node.id);
                } else {
                    println!("Zone dimmer node '{}' executed", node.id);
                }
            }
            "harmony_analysis" => {
                let audio_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                    .ok_or_else(|| {
                        format!("HarmonyAnalysis node '{}' missing audio input", node.id)
                    })?;

                let audio_buffer = audio_buffers
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

                let mut harmony_series: Option<Series> = None;

                if let Some(track_id) = audio_buffer.track_id {
                    eprintln!(
                        "[harmony_analysis] '{}' processing track {} (crop {:.3?}-{:.3?})",
                        node.id, track_id, crop_start, crop_end
                    );

                    // Load harmony sections from the precomputed cache if they have not
                    // been pulled into memory yet for this run.
                    if !root_caches.contains_key(&track_id) {
                        eprintln!(
                            "[harmony_analysis] '{}' cache miss for track {}; loading from DB",
                            node.id, track_id
                        );
                        if let Some((sections_json,)) = sqlx::query_as::<_, (String,)>(
                            "SELECT sections_json FROM track_roots WHERE track_id = ?",
                        )
                        .bind(track_id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| format!("Failed to load chord sections: {}", e))?
                        {
                            let sections: Vec<crate::root_worker::ChordSection> =
                                serde_json::from_str(&sections_json).map_err(|e| {
                                    format!("Failed to parse chord sections: {}", e)
                                })?;

                            root_caches.insert(track_id, RootCache { sections });
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

                    if let Some(cache) = root_caches.get(&track_id) {
                        eprintln!(
                            "[harmony_analysis] '{}' building series from {} sections",
                            node.id,
                            cache.sections.len()
                        );

                        // Optional beat grid for quantizing chord boundaries.
                        let beat_times: Option<Vec<f32>> = incoming_edges
                            .get(node.id.as_str())
                            .and_then(|edges| edges.iter().find(|e| e.to_port == "grid_in"))
                            .and_then(|grid_edge| {
                                beat_grids
                                    .get(&(grid_edge.from_node.clone(), grid_edge.from_port.clone()))
                            })
                            .map(|grid| {
                                eprintln!(
                                    "[harmony_analysis] '{}' using beat grid with {} beats / {} downbeats",
                                    node.id,
                                    grid.beats.len(),
                                    grid.downbeats.len()
                                );
                                // Beat grids upstream of crop nodes are relative to the cropped
                                // segment. Shift them back into the absolute timeline so we can
                                // snap chord sections (which are stored in absolute seconds).
                                let offset = audio_buffer
                                    .crop
                                    .map(|c| c.start_seconds)
                                    .unwrap_or(0.0);
                                let mut times = Vec::new();
                                times.extend(grid.downbeats.iter().map(|t| t + offset));
                                times.extend(grid.beats.iter().map(|t| t + offset));
                                times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                                times.dedup_by(|a, b| (*a - *b).abs() < f32::EPSILON);
                                times
                            });

                        // Quantize a time value to the nearest beat using binary search for efficiency
                        let snap_to_beat = |value: f32, beats: &[f32]| -> f32 {
                            if beats.is_empty() {
                                return value;
                            }
                            // Binary search for the insertion point
                            match beats.binary_search_by(|t| {
                                t.partial_cmp(&value).unwrap_or(std::cmp::Ordering::Equal)
                            }) {
                                Ok(idx) => beats[idx], // Exact match
                                Err(idx) => {
                                    // idx is the position where value would be inserted
                                    // Compare with beats[idx-1] (if exists) and beats[idx] (if exists)
                                    if idx == 0 {
                                        // Value is before all beats, snap to first beat
                                        beats[0]
                                    } else if idx >= beats.len() {
                                        // Value is after all beats, snap to last beat
                                        beats[beats.len() - 1]
                                    } else {
                                        // Value is between beats[idx-1] and beats[idx], pick the closest
                                        let dist_prev = (value - beats[idx - 1]).abs();
                                        let dist_next = (value - beats[idx]).abs();
                                        if dist_prev <= dist_next {
                                            beats[idx - 1]
                                        } else {
                                            beats[idx]
                                        }
                                    }
                                }
                            }
                        };

                        let mut samples = Vec::new();
                        for section in &cache.sections {
                            let start = section.start;
                            let end = section.end;
                            if end < crop_start || start > crop_end {
                                continue;
                            }
                            let mut snapped_start = start;
                            let mut snapped_end = end;
                            if let Some(beats) = beat_times.as_ref() {
                                snapped_start = snap_to_beat(start, beats);
                                snapped_end = snap_to_beat(end, beats);
                            }
                            let clamped_start = snapped_start.max(crop_start) - crop_start;
                            let clamped_end = snapped_end.min(crop_end) - crop_start;

                            if clamped_end <= clamped_start {
                                continue;
                            }

                            let mut values = vec![0.0f32; CHROMA_DIM];
                            if let Some(root) = section.root {
                                let idx = (root as usize).min(CHROMA_DIM - 1);
                                values[idx] = 1.0;
                            }

                            samples.push(SeriesSample {
                                time: clamped_start,
                                values: values.clone(),
                                label: Some(section.label.clone()),
                            });

                            samples.push(SeriesSample {
                                time: clamped_end,
                                values,
                                label: Some(section.label.clone()),
                            });
                        }

                        samples.sort_by(|a, b| {
                            a.time
                                .partial_cmp(&b.time)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });

                        harmony_series = Some(Series {
                            dim: CHROMA_DIM,
                            labels: Some(
                                PITCH_CLASS_LABELS
                                    .iter()
                                    .map(|label| label.to_string())
                                    .collect(),
                            ),
                            samples,
                        });
                    } else {
                        eprintln!(
                            "[run_graph] no chord sections cache for track {}; harmony timeline empty",
                            track_id
                        );
                    }
                }

                if let Some(series) = harmony_series {
                    series_outputs.insert((node.id.clone(), "chroma".into()), series);
                }
            }
            "mel_spec_viewer" => {
                let Some(input_edge) = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|e| e.to_port == "in"))
                else {
                    eprintln!(
                        "[run_graph] mel_spec_viewer '{}' missing audio input; skipping",
                        node.id
                    );
                    continue;
                };

                let Some(audio_buffer) = audio_buffers
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                else {
                    eprintln!(
                        "[run_graph] mel_spec_viewer '{}' input audio not found; skipping",
                        node.id
                    );
                    continue;
                };

                // Look for optional beat grid input
                let beat_grid = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|e| e.to_port == "grid"))
                    .and_then(|grid_edge| {
                        beat_grids.get(&(grid_edge.from_node.clone(), grid_edge.from_port.clone()))
                    })
                    .cloned();

                let mel_start = std::time::Instant::now();
                let data = generate_melspec(
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

                mel_specs.insert(
                    node.id.clone(),
                    MelSpec {
                        width: MEL_SPEC_WIDTH,
                        height: MEL_SPEC_HEIGHT,
                        data,
                        beat_grid,
                    },
                );
            }
            "color" => {
                let color_param = node
                    .params
                    .get("color")
                    .and_then(|v| v.as_str())
                    .unwrap_or(r#"{"r":255,"g":0,"b":0,"a":1}"#);

                color_outputs.insert((node.id.clone(), "out".into()), color_param.to_string());
            }
            "harmony_color_visualizer" => {
                let harmony_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "harmony_in"))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' missing harmony input",
                            node.id
                        )
                    })?;

                let color_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "color_in"))
                    .ok_or_else(|| {
                        format!("Harmony Color Visualizer '{}' missing color input", node.id)
                    })?;

                let audio_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                    .ok_or_else(|| {
                        format!("Harmony Color Visualizer '{}' missing audio input", node.id)
                    })?;

                let harmony_series = series_outputs
                    .get(&(
                        harmony_edge.from_node.clone(),
                        harmony_edge.from_port.clone(),
                    ))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' harmony input unavailable",
                            node.id
                        )
                    })?;

                let base_color_json = color_outputs
                    .get(&(color_edge.from_node.clone(), color_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' color input unavailable",
                            node.id
                        )
                    })?
                    .clone();

                let audio_buffer = audio_buffers
                    .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' audio input unavailable",
                            node.id
                        )
                    })?;

                color_views.insert(node.id.clone(), base_color_json);

                let palette_size = node
                    .params
                    .get("palette_size")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as usize)
                    .unwrap_or(4)
                    .max(2);

                let audio_duration = if audio_buffer.sample_rate == 0 {
                    0.0
                } else {
                    audio_buffer.samples.len() as f32 / audio_buffer.sample_rate as f32
                };

                let mut color_samples: Vec<SeriesSample> = Vec::new();

                for (index, sample) in harmony_series.samples.iter().enumerate() {
                    let start_time = sample.time;
                    let end_time = if let Some(next) = harmony_series.samples.get(index + 1) {
                        next.time
                    } else {
                        audio_duration
                    };

                    if end_time <= start_time {
                        continue;
                    }

                    let values = &sample.values;
                    if values.is_empty() {
                        continue;
                    }

                    let mut max_idx = 0usize;
                    let mut max_val = values[0];
                    for (idx, &val) in values.iter().enumerate() {
                        if val > max_val {
                            max_val = val;
                            max_idx = idx;
                        }
                    }

                    let palette_idx = (max_idx % palette_size) as f32;
                    let brightness = estimate_segment_brightness(
                        &audio_buffer.samples,
                        audio_buffer.sample_rate,
                        start_time,
                        end_time,
                    );

                    let clamped_brightness = brightness.clamp(0.0, 1.0);

                    color_samples.push(SeriesSample {
                        time: start_time,
                        values: vec![palette_idx, clamped_brightness],
                        label: sample.label.clone(),
                    });
                    color_samples.push(SeriesSample {
                        time: end_time,
                        values: vec![palette_idx, clamped_brightness],
                        label: sample.label.clone(),
                    });
                }

                if color_samples.is_empty() {
                    continue;
                }

                let color_series = Series {
                    dim: 2,
                    labels: Some(vec!["palette_index".to_string(), "brightness".to_string()]),
                    samples: color_samples,
                };

                series_views.insert(node.id.clone(), color_series);
            }
            other => {
                println!("Encountered unknown node type '{}'", other);
            }
        }
    }

    Ok(RunResult {
        views: view_results,
        series_views,
        mel_specs,
        color_views,
    })
}

fn compute_chroma_series(samples: &[f32], sample_rate: u32) -> Series {
    if sample_rate == 0 || samples.is_empty() {
        return empty_harmony_series();
    }

    let mut chromagram = match Chromagram::builder()
        .frame_size(CHROMA_WINDOW)
        .sampling_rate(sample_rate as usize)
        .build()
    {
        Ok(chroma) => chroma,
        Err(err) => {
            eprintln!("[compute_chroma_series] failed to initialize chromagram: {err}");
            return empty_harmony_series();
        }
    };

    let mut detector = ChordDetector::builder().bleed(0.2).build();
    let mut frame_buffer = vec![0.0f32; CHROMA_WINDOW];

    let frame_count = if samples.len() <= CHROMA_WINDOW {
        1
    } else {
        (samples.len() - CHROMA_WINDOW) / CHROMA_HOP + 1
    };

    let center_offset = (CHROMA_WINDOW as f32 / 2.0) / sample_rate as f32;

    let mut series_samples = Vec::with_capacity(frame_count);
    for frame_index in 0..frame_count {
        let start = frame_index * CHROMA_HOP;
        for i in 0..CHROMA_WINDOW {
            frame_buffer[i] = samples.get(start + i).copied().unwrap_or(0.0);
        }

        let rms = (frame_buffer.iter().map(|&v| v * v).sum::<f32>() / CHROMA_WINDOW as f32).sqrt();
        if rms < RMS_THRESHOLD {
            continue;
        }

        let chroma_bins = match chromagram.next(&frame_buffer) {
            Ok(Some(bins)) => bins,
            Ok(None) => continue,
            Err(err) => {
                eprintln!(
                    "[compute_chroma_series] failed to compute chromagram for frame {}: {err}",
                    frame_index
                );
                continue;
            }
        };

        let chords = match detector.top_k(&chroma_bins, MAX_CHORD_CHOICES) {
            Ok(chords) if !chords.is_empty() => chords,
            Ok(_) => {
                continue;
            }
            Err(err) => {
                eprintln!(
                    "[compute_chroma_series] failed to rank chords for frame {}: {err}",
                    frame_index
                );
                continue;
            }
        };

        let label = chords.first().map(format_chord_label);

        let mut root_scores = [f32::INFINITY; CHROMA_DIM];
        for chord in &chords {
            if let Some(root_idx) = note_name_index(chord.root) {
                if chord.confidence < root_scores[root_idx] {
                    root_scores[root_idx] = chord.confidence;
                }
            }
        }
        let confidences = distances_to_distribution(&root_scores);

        let time = frame_index as f32 * CHROMA_HOP as f32 / sample_rate as f32 + center_offset;
        series_samples.push(SeriesSample {
            time,
            values: confidences,
            label,
        });
    }

    Series {
        dim: CHROMA_DIM,
        labels: Some(
            PITCH_CLASS_LABELS
                .iter()
                .map(|label| label.to_string())
                .collect(),
        ),
        samples: series_samples,
    }
}

fn empty_harmony_series() -> Series {
    Series {
        dim: CHROMA_DIM,
        labels: Some(
            PITCH_CLASS_LABELS
                .iter()
                .map(|label| label.to_string())
                .collect(),
        ),
        samples: Vec::new(),
    }
}

fn format_chord_label(chord: &Chord) -> String {
    let root = note_name_label(chord.root);
    let suffix = chord_kind_suffix(chord.quality);
    if suffix.is_empty() {
        root.to_string()
    } else {
        format!("{root}{suffix}")
    }
}

fn note_name_label(name: NoteName) -> &'static str {
    match name {
        NoteName::C => "C",
        NoteName::Cs => "C#",
        NoteName::D => "D",
        NoteName::Ds => "D#",
        NoteName::E => "E",
        NoteName::F => "F",
        NoteName::Fs => "F#",
        NoteName::G => "G",
        NoteName::Gs => "G#",
        NoteName::A => "A",
        NoteName::As => "A#",
        NoteName::B => "B",
        NoteName::Unknown => "Unknown",
    }
}

fn chord_kind_suffix(kind: ChordKind) -> &'static str {
    match kind {
        ChordKind::Major => "",
        ChordKind::Minor => "m",
        ChordKind::PowerFifth => "5",
        ChordKind::DominantSeventh => "7",
        ChordKind::MajorSeventh => "maj7",
        ChordKind::MinorSeventh => "m7",
        ChordKind::Diminished => "dim",
        ChordKind::Augmented => "aug",
        ChordKind::SuspendedSecond => "sus2",
        ChordKind::SuspendedFourth => "sus4",
    }
}

fn note_name_index(name: NoteName) -> Option<usize> {
    match name {
        NoteName::C => Some(0),
        NoteName::Cs => Some(1),
        NoteName::D => Some(2),
        NoteName::Ds => Some(3),
        NoteName::E => Some(4),
        NoteName::F => Some(5),
        NoteName::Fs => Some(6),
        NoteName::G => Some(7),
        NoteName::Gs => Some(8),
        NoteName::A => Some(9),
        NoteName::As => Some(10),
        NoteName::B => Some(11),
        NoteName::Unknown => None,
    }
}

fn distances_to_distribution(scores: &[f32; CHROMA_DIM]) -> Vec<f32> {
    let mut best = f32::INFINITY;
    for &score in scores.iter() {
        if score.is_finite() && score < best {
            best = score;
        }
    }

    let mut probs = vec![0.0f32; CHROMA_DIM];
    if best.is_finite() {
        let tau = 5.0;
        let mut denom = 0.0;
        for (idx, &score) in scores.iter().enumerate() {
            if !score.is_finite() {
                continue;
            }
            let w = (-(score - best) / tau).exp();
            probs[idx] = w;
            denom += w;
        }
        if denom > 0.0 {
            for val in &mut probs {
                *val /= denom;
            }
        }
    }
    probs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::f32::consts::PI;

    fn run(graph: Graph) -> RunResult {
        tauri::async_runtime::block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:")
                .await
                .expect("in-memory db");
            // Dummy context for tests that don't use audio_input nodes
            let context = GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 0.0,
                beat_grid: None,
            };
            run_graph_internal(&pool, graph, context)
                .await
                .expect("graph execution should succeed")
        })
    }

    #[test]
    fn sample_pattern_flows_to_view() {
        let sample_node = NodeInstance {
            id: "n1".into(),
            type_id: "sample_pattern".into(),
            params: HashMap::new(),
            position_x: None,
            position_y: None,
        };

        let view_node = NodeInstance {
            id: "n2".into(),
            type_id: "view_channel".into(),
            params: HashMap::new(),
            position_x: None,
            position_y: None,
        };

        let edge = Edge {
            id: "e1".into(),
            from_node: "n1".into(),
            from_port: "out".into(),
            to_node: "n2".into(),
            to_port: "in".into(),
        };

        let result = run(Graph {
            nodes: vec![sample_node, view_node],
            edges: vec![edge],
        });

        assert!(result.views.contains_key("n2"));
        let samples = &result.views["n2"];
        assert_eq!(samples[0], 1.0);
        assert_eq!(samples[1], 0.5);
        assert_eq!(samples[2], 0.2);
    }

    #[test]
    fn threshold_applies_binary_output() {
        let sample_node = NodeInstance {
            id: "n1".into(),
            type_id: "sample_pattern".into(),
            params: HashMap::new(),
            position_x: None,
            position_y: None,
        };

        let threshold_node = NodeInstance {
            id: "n2".into(),
            type_id: "threshold".into(),
            params: HashMap::from([(String::from("threshold"), json!(0.6))]),
            position_x: None,
            position_y: None,
        };

        let view_node = NodeInstance {
            id: "n3".into(),
            type_id: "view_channel".into(),
            params: HashMap::new(),
            position_x: None,
            position_y: None,
        };

        let edges = vec![
            Edge {
                id: "e1".into(),
                from_node: "n1".into(),
                from_port: "out".into(),
                to_node: "n2".into(),
                to_port: "in".into(),
            },
            Edge {
                id: "e2".into(),
                from_node: "n2".into(),
                from_port: "out".into(),
                to_node: "n3".into(),
                to_port: "in".into(),
            },
        ];

        let result = run(Graph {
            nodes: vec![sample_node, threshold_node, view_node],
            edges,
        });

        let samples = &result.views["n3"];
        assert_eq!(samples[0], 1.0);
        assert_eq!(samples[1], 0.0);
    }

    #[test]
    fn harmony_series_reports_detected_chords() {
        let sample_rate = 16_000;
        let duration_samples = sample_rate;
        let samples: Vec<f32> = (0..duration_samples)
            .map(|idx| {
                let t = idx as f32 / sample_rate as f32;
                (2.0 * PI * 440.0 * t).sin()
            })
            .collect();

        let series = compute_chroma_series(&samples, sample_rate);
        assert_eq!(series.dim, CHROMA_DIM);
        let labels = series.labels.as_ref().expect("labels are present");
        let expected_labels: Vec<String> = PITCH_CLASS_LABELS
            .iter()
            .map(|label| label.to_string())
            .collect();
        assert_eq!(labels, &expected_labels);
        assert!(!series.samples.is_empty());

        for sample in &series.samples {
            assert_eq!(sample.values.len(), CHROMA_DIM);
            for &confidence in &sample.values {
                assert!(confidence >= 0.0 && confidence <= 1.0);
            }
            assert!(
                sample
                    .label
                    .as_ref()
                    .map(|label| !label.is_empty())
                    .unwrap_or(false),
                "chord label should be emitted"
            );
        }
    }
}
