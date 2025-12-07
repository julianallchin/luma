use crate::audio::{
    calculate_frequency_amplitude, generate_melspec, highpass_filter, load_or_decode_audio,
    lowpass_filter, StemCache, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH,
};
use crate::database::Db;
use crate::fixtures::layout::compute_head_offsets;
use crate::fixtures::models::PatchedFixture;
use crate::fixtures::parser::parse_definition;
pub use crate::models::schema::*;
use crate::models::tracks::MelSpec;
use crate::tracks::TARGET_SAMPLE_RATE;
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde_json;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tauri::{AppHandle, Manager, State};

const CHROMA_DIM: usize = 12;
static RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

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
            id: "select".into(),
            name: "Select".into(),
            description: Some("Selects specific fixtures or primitives.".into()),
            category: Some("Selection".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            params: vec![ParamDef {
                id: "selected_ids".into(),
                name: "Selected IDs".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("[]".into()), // JSON array of strings
            }],
        },
        NodeTypeDef {
            id: "get_attribute".into(),
            name: "Get Attribute".into(),
            description: Some("Extracts spatial attributes from a selection into a Signal.".into()),
            category: Some("Selection".into()),
            inputs: vec![PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "attribute".into(),
                name: "Attribute".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("index".into()), // index, normalized_index, pos_x, pos_y, pos_z, rel_x, rel_y, rel_z
            }],
        },
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
                default_text: Some("add".into()), // add, subtract, multiply, divide, max, min, abs_diff
            }],
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
            id: "apply_dimmer".into(),
            name: "Apply Dimmer".into(),
            description: Some("Applies intensity signal to selected primitives.".into()),
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
            id: "apply_color".into(),
            name: "Apply Color".into(),
            description: Some("Applies RGB signal to selected primitives.".into()),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "signal".into(),
                    name: "Signal (3ch)".into(),
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
                    name: "Attack (Beats)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.1),
                    default_text: None,
                },
                ParamDef {
                    id: "sustain".into(),
                    name: "Sustain (Beats)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "decay".into(),
                    name: "Decay (Beats)".into(),
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
            ],
        },
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
        NodeTypeDef {
            id: "harmony_color_visualizer".into(),
            name: "Harmony to Color".into(),
            description: Some("Maps harmony signal to colors using a generated palette.".into()),
            category: Some("Transform".into()),
            inputs: vec![
                PortDef {
                    id: "signal_in".into(),
                    name: "Harmony (Signal)".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "color_in".into(),
                    name: "Base Color".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "audio_in".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color Signal".into(),
                port_type: PortType::Signal,
            }],
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
    app: AppHandle,
    db: State<'_, Db>,
    host_audio: State<'_, crate::host_audio::HostAudioState>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, crate::audio::FftService>,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    let project_db_state: Option<State<'_, crate::database::ProjectDb>> = app.try_state();

    // SqlitePool is cheap to clone (Arc), so we clone it out of the mutex.
    let project_pool = if let Some(state) = project_db_state {
        let guard = state.0.lock().await;
        guard.clone()
    } else {
        None
    };

    // Check if we need project pool (if we have Select nodes)
    let has_select = graph.nodes.iter().any(|n| n.type_id == "select");
    if has_select && project_pool.is_none() {
        return Err("Cannot run graph with Select node: No project open".into());
    }

    // Resolve resource path for fixtures
    let resource_path = app
        .path()
        .resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

    let final_path = if resource_path.exists() {
        Some(resource_path)
    } else {
        // Fallback to cwd logic
        let cwd = std::env::current_dir().unwrap_or_default();
        let dev_path = cwd.join("../resources/fixtures/2511260420");
        if dev_path.exists() {
            Some(dev_path)
        } else {
            let local = cwd.join("resources/fixtures/2511260420");
            if local.exists() {
                Some(local)
            } else {
                None
            }
        }
    };

    let (result, layer) = run_graph_internal(
        &db.0,
        project_pool.as_ref(),
        &stem_cache,
        &fft_service,
        final_path,
        graph,
        context,
        GraphExecutionConfig {
            compute_visualizations: true,
            log_summary: true,
            log_primitives: false,
            shared_audio: None,
        },
    )
    .await?;

    // Push the calculated plan to the Host Audio engine for real-time playback
    host_audio.set_active_layer(layer);

    Ok(result)
}

#[derive(Clone)]
pub struct SharedAudioContext {
    pub track_id: i64,
    pub track_hash: String,
    pub samples: Arc<Vec<f32>>,
    pub sample_rate: u32,
}

#[derive(Clone)]
pub struct GraphExecutionConfig {
    pub compute_visualizations: bool,
    pub log_summary: bool,
    pub log_primitives: bool,
    pub shared_audio: Option<SharedAudioContext>,
}

impl Default for GraphExecutionConfig {
    fn default() -> Self {
        Self {
            compute_visualizations: true,
            log_summary: true,
            log_primitives: false,
            shared_audio: None,
        }
    }
}

impl GraphExecutionConfig {
    pub fn quiet_with_shared(shared_audio: Option<SharedAudioContext>) -> Self {
        Self {
            compute_visualizations: false,
            log_summary: false,
            log_primitives: false,
            shared_audio,
        }
    }
}

pub async fn run_graph_internal(
    pool: &SqlitePool,
    project_pool: Option<&SqlitePool>,
    stem_cache: &StemCache,
    fft_service: &crate::audio::FftService,
    resource_path_root: Option<PathBuf>,
    graph: Graph,
    context: GraphContext,
    config: GraphExecutionConfig,
) -> Result<(RunResult, Option<LayerTimeSeries>), String> {
    let compute_visualizations = config.compute_visualizations;
    let run_id = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_start = Instant::now();

    if config.log_summary {
        println!("[run_graph #{run_id}] start nodes={}", graph.nodes.len());
    }

    if graph.nodes.is_empty() {
        return Ok((
            RunResult {
                views: HashMap::new(),
                series_views: HashMap::new(),
                mel_specs: HashMap::new(),
                color_views: HashMap::new(),
                universe_state: None,
            },
            None,
        ));
    }

    let arg_defs = graph.args.clone();
    let arg_values: HashMap<String, serde_json::Value> =
        context.arg_values.clone().unwrap_or_default();

    const PREVIEW_LENGTH: usize = 256;
    const SIMULATION_RATE: f32 = 60.0; // 60Hz resolution for control signals

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

    let mut audio_buffers: HashMap<(String, String), AudioBuffer> = HashMap::new();
    let mut beat_grids: HashMap<(String, String), BeatGrid> = HashMap::new();
    let mut selections: HashMap<(String, String), Selection> = HashMap::new();
    let mut signal_outputs: HashMap<(String, String), Signal> = HashMap::new();

    // Collects outputs from all Apply nodes to be merged
    let mut apply_outputs: Vec<LayerTimeSeries> = Vec::new();

    #[derive(Clone)]
    struct NodeTiming {
        id: String,
        type_id: String,
        ms: f64,
    }
    let mut node_timings: Vec<NodeTiming> = Vec::new();

    // Resolve a color value into normalized RGBA tuple
    let parse_color_value = |value: &serde_json::Value| -> (f32, f32, f32, f32) {
        let obj = value.as_object();
        let r = obj
            .and_then(|o| o.get("r"))
            .and_then(|v| v.as_f64())
            .unwrap_or(255.0) as f32
            / 255.0;
        let g = obj
            .and_then(|o| o.get("g"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32
            / 255.0;
        let b = obj
            .and_then(|o| o.get("b"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32
            / 255.0;
        let a = obj
            .and_then(|o| o.get("a"))
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32;
        (r, g, b, a)
    };

    // === Lazy context loading ===

    // === Lazy context loading ===
    // Only load audio if graph contains nodes that need it (audio_input, beat_clock, etc.)
    let needs_context = graph.nodes.iter().any(|n| {
        matches!(
            n.type_id.as_str(),
            "audio_input"
                | "beat_clock"
                | "stem_splitter"
                | "harmony_analysis"
                | "lowpass_filter"
                | "highpass_filter"
        )
    });

    // Context data - loaded lazily
    let context_load_start = Instant::now();
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

        let (context_full_samples, sample_rate, track_hash): (Vec<f32>, u32, String) =
            if let Some(shared) = config.shared_audio.as_ref() {
                if shared.track_id != context.track_id {
                    return Err(format!(
                        "Shared audio provided for track {} but context track is {}",
                        shared.track_id, context.track_id
                    ));
                }
                (shared.samples.as_ref().clone(), shared.sample_rate, shared.track_hash.clone())
            } else {
                let context_path = Path::new(&context_file_path);
                let (samples, sample_rate) =
                    load_or_decode_audio(context_path, &track_hash, TARGET_SAMPLE_RATE)
                        .map_err(|e| format!("Failed to decode track: {}", e))?;

                if samples.is_empty() || sample_rate == 0 {
                    return Err("Context track has no audio data".into());
                }

                (samples, sample_rate, track_hash)
            };

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
    let context_load_ms = context_load_start.elapsed().as_secs_f64() * 1000.0;

    let mut color_outputs: HashMap<(String, String), String> = HashMap::new();
    let mut root_caches: HashMap<i64, RootCache> = HashMap::new();
    let mut view_results: HashMap<String, Signal> = HashMap::new();
    let mut mel_specs: HashMap<String, MelSpec> = HashMap::new();
    let mut series_views: HashMap<String, Series> = HashMap::new();
    let mut color_views: HashMap<String, String> = HashMap::new();

    let nodes_exec_start = Instant::now();
    for node_idx in sorted {
        let node_id = dependency_graph[node_idx];
        let node = nodes_by_id
            .get(node_id)
            .copied()
            .ok_or_else(|| format!("Node '{}' not found during execution", node_id))?;

        let node_start = Instant::now();

        match node.type_id.as_str() {
            "pattern_args" => {
                for arg in &arg_defs {
                    let value = arg_values
                        .get(&arg.id)
                        .unwrap_or(&arg.default_value);

                    match arg.arg_type {
                        PatternArgType::Color => {
                            let (r, g, b, _a) = parse_color_value(value);
                            signal_outputs.insert(
                                (node.id.clone(), arg.id.clone()),
                                Signal {
                                    n: 1,
                                    t: 1,
                                    c: 3,
                                    data: vec![r, g, b],
                                },
                            );

                            let color_json = serde_json::json!({
                                "r": (r * 255.0).round() as i32,
                                "g": (g * 255.0).round() as i32,
                                "b": (b * 255.0).round() as i32,
                                "a": 1.0,
                            })
                            .to_string();
                            color_outputs.insert((node.id.clone(), arg.id.clone()), color_json.clone());
                            color_views.insert(format!("{}:{}", node.id, arg.id), color_json);
                        }
                    }
                }
            }
            "select" => {
                // 1. Parse selected IDs
                let ids_json = node
                    .params
                    .get("selected_ids")
                    .and_then(|v| v.as_str())
                    .unwrap_or("[]");
                let selected_ids: Vec<String> = serde_json::from_str(ids_json).unwrap_or_default();

                if let Some(proj_pool) = project_pool {
                    let fixtures = sqlx::query_as::<_, PatchedFixture>(
                        "SELECT id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z FROM fixtures"
                    )
                    .fetch_all(proj_pool)
                    .await
                    .map_err(|e| format!("Select node failed to fetch fixtures: {}", e))?;

                    let mut selected_items = Vec::new();

                    // Pre-process selection set for O(1) lookup
                    // We need to handle "FixtureID" (all heads) vs "FixtureID:HeadIdx" (specific head)

                    for fixture in fixtures {
                        // 2. Load definition to get layout
                        let def_path = if let Some(root) = &resource_path_root {
                            root.join(&fixture.fixture_path)
                        } else {
                            PathBuf::from(&fixture.fixture_path)
                        };

                        let offsets = if let Ok(def) = parse_definition(&def_path) {
                            compute_head_offsets(&def, &fixture.mode_name)
                        } else {
                            // If missing def, assume single head at 0,0,0
                            vec![crate::fixtures::layout::HeadLayout {
                                x: 0.0,
                                y: 0.0,
                                z: 0.0,
                            }]
                        };

                        // Check if this fixture is involved in selection
                        let fixture_selected = selected_ids.contains(&fixture.id);

                        for (i, offset) in offsets.iter().enumerate() {
                            let head_id = format!("{}:{}", fixture.id, i);
                            let head_selected = selected_ids.contains(&head_id);

                            // Include if whole fixture selected OR specific head selected
                            if fixture_selected || head_selected {
                                // Apply rotation (Euler ZYX convention typically)
                                // Local offset in mm
                                let lx = offset.x / 1000.0;
                                let ly = offset.y / 1000.0;
                                let lz = offset.z / 1000.0;

                                let rx = fixture.rot_x;
                                let ry = fixture.rot_y;
                                let rz = fixture.rot_z;

                                // Rotate around X
                                // y' = y*cos(rx) - z*sin(rx)
                                // z' = y*sin(rx) + z*cos(rx)
                                let (ly_x, lz_x) = (
                                    ly * rx.cos() as f32 - lz * rx.sin() as f32,
                                    ly * rx.sin() as f32 + lz * rx.cos() as f32,
                                );
                                let lx_x = lx;

                                // Rotate around Y
                                // x'' = x'*cos(ry) + z'*sin(ry)
                                // z'' = -x'*sin(ry) + z'*cos(ry)
                                let (lx_y, lz_y) = (
                                    lx_x * ry.cos() as f32 + lz_x * ry.sin() as f32,
                                    -lx_x * ry.sin() as f32 + lz_x * ry.cos() as f32,
                                );
                                let ly_y = ly_x;

                                // Rotate around Z
                                // x''' = x''*cos(rz) - y''*sin(rz)
                                // y''' = x''*sin(rz) + y''*cos(rz)
                                let (lx_z, ly_z) = (
                                    lx_y * rz.cos() as f32 - ly_y * rz.sin() as f32,
                                    lx_y * rz.sin() as f32 + ly_y * rz.cos() as f32,
                                );
                                let lz_z = lz_y;

                                let gx = fixture.pos_x as f32 + lx_z;
                                let gy = fixture.pos_y as f32 + ly_z;
                                let gz = fixture.pos_z as f32 + lz_z;

                                selected_items.push(SelectableItem {
                                    id: head_id,
                                    fixture_id: fixture.id.clone(),
                                    head_index: i,
                                    pos: (gx, gy, gz),
                                });
                            }
                        }
                    }

                    selections.insert(
                        (node.id.clone(), "out".into()),
                        Selection {
                            items: selected_items,
                        },
                    );
                }
            }
            "get_attribute" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");

                if let Some(edge) = selection_edge {
                    if let Some(selection) =
                        selections.get(&(edge.from_node.clone(), edge.from_port.clone()))
                    {
                        let attr = node
                            .params
                            .get("attribute")
                            .and_then(|v| v.as_str())
                            .unwrap_or("index");

                        let n = selection.items.len();
                        let mut data = Vec::with_capacity(n);

                        // Compute bounds for normalization if needed
                        let (min_x, max_x, min_y, max_y, min_z, max_z) = if n > 0 {
                            let first = &selection.items[0];
                            let mut bounds = (
                                first.pos.0,
                                first.pos.0,
                                first.pos.1,
                                first.pos.1,
                                first.pos.2,
                                first.pos.2,
                            );
                            for item in &selection.items {
                                if item.pos.0 < bounds.0 {
                                    bounds.0 = item.pos.0;
                                }
                                if item.pos.0 > bounds.1 {
                                    bounds.1 = item.pos.0;
                                }
                                if item.pos.1 < bounds.2 {
                                    bounds.2 = item.pos.1;
                                }
                                if item.pos.1 > bounds.3 {
                                    bounds.3 = item.pos.1;
                                }
                                if item.pos.2 < bounds.4 {
                                    bounds.4 = item.pos.2;
                                }
                                if item.pos.2 > bounds.5 {
                                    bounds.5 = item.pos.2;
                                }
                            }
                            bounds
                        } else {
                            (0.0, 1.0, 0.0, 1.0, 0.0, 1.0)
                        };

                        let range_x = (max_x - min_x).max(0.001); // Avoid div by zero
                        let range_y = (max_y - min_y).max(0.001);
                        let range_z = (max_z - min_z).max(0.001);

                        for (i, item) in selection.items.iter().enumerate() {
                            let val = match attr {
                                "index" => i as f32,
                                "normalized_index" => {
                                    if n > 1 {
                                        i as f32 / (n - 1) as f32
                                    } else {
                                        0.0
                                    }
                                }
                                "pos_x" => item.pos.0,
                                "pos_y" => item.pos.1,
                                "pos_z" => item.pos.2,
                                "rel_x" => (item.pos.0 - min_x) / range_x,
                                "rel_y" => (item.pos.1 - min_y) / range_y,
                                "rel_z" => (item.pos.2 - min_z) / range_z,
                                _ => 0.0,
                            };
                            data.push(val);
                        }

                        signal_outputs.insert(
                            (node.id.clone(), "out".into()),
                            Signal {
                                n,
                                t: 1, // Static over time
                                c: 1, // Scalar attribute
                                data,
                            },
                        );
                    }
                }
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

                let signal_a = a_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));
                let signal_b = b_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

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
                                _ => val_a + val_b,
                            };

                            data.push(res);
                        }
                    }
                }

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: out_n,
                        t: out_t,
                        c: out_c,
                        data,
                    },
                );
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
                    continue;
                };

                let Some(signal) = signal_outputs
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                else {
                    eprintln!(
                        "[run_graph] threshold '{}' input signal unavailable; skipping",
                        node.id
                    );
                    continue;
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

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: signal.n,
                        t: signal.t,
                        c: signal.c,
                        data,
                    },
                );
            }
            "apply_dimmer" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
                let signal_edge = input_edges.iter().find(|e| e.to_port == "signal");

                if let (Some(sel_e), Some(sig_e)) = (selection_edge, signal_edge) {
                    if let (Some(selection), Some(signal)) = (
                        selections.get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                        signal_outputs.get(&(sig_e.from_node.clone(), sig_e.from_port.clone())),
                    ) {
                        let mut primitives = Vec::new();

                        for (i, item) in selection.items.iter().enumerate() {
                            // Broadcast N: get corresponding row from signal
                            let sig_idx = if signal.n == 1 { 0 } else { i % signal.n };

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
                            });
                        }

                        apply_outputs.push(LayerTimeSeries { primitives });
                    }
                }
            }
            "apply_color" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
                let signal_edge = input_edges.iter().find(|e| e.to_port == "signal");

                if let (Some(sel_e), Some(sig_e)) = (selection_edge, signal_edge) {
                    if let (Some(selection), Some(signal)) = (
                        selections.get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                        signal_outputs.get(&(sig_e.from_node.clone(), sig_e.from_port.clone())),
                    ) {
                        let mut primitives = Vec::new();

                        for (i, item) in selection.items.iter().enumerate() {
                            // Broadcast N
                            let sig_idx = if signal.n == 1 { 0 } else { i % signal.n };

                            let mut samples = Vec::new();

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

                                samples.push(SeriesSample {
                                    time: context.start_time,
                                    values: vec![r, g, b],
                                    label: None,
                                });
                                samples.push(SeriesSample {
                                    time: context.end_time,
                                    values: vec![r, g, b],
                                    label: None,
                                });
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

                                    let time = context.start_time
                                        + (t as f32 / (signal.t - 1).max(1) as f32) * duration;
                                    samples.push(SeriesSample {
                                        time,
                                        values: vec![r, g, b],
                                        label: None,
                                    });
                                }
                            }

                            primitives.push(PrimitiveTimeSeries {
                                primitive_id: item.id.clone(),
                                color: Some(Series {
                                    dim: 3,
                                    labels: None,
                                    samples,
                                }),
                                dimmer: None,
                                position: None,
                                strobe: None,
                            });
                        }

                        apply_outputs.push(LayerTimeSeries { primitives });
                    }
                }
            }
            "apply_strobe" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
                let signal_edge = input_edges.iter().find(|e| e.to_port == "signal");

                if let (Some(sel_e), Some(sig_e)) = (selection_edge, signal_edge) {
                    if let (Some(selection), Some(signal)) = (
                        selections.get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                        signal_outputs.get(&(sig_e.from_node.clone(), sig_e.from_port.clone())),
                    ) {
                        let mut primitives = Vec::new();

                        for (i, item) in selection.items.iter().enumerate() {
                            // Broadcast N
                            let sig_idx = if signal.n == 1 { 0 } else { i % signal.n };

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
                            });
                        }

                        apply_outputs.push(LayerTimeSeries { primitives });
                    }
                }
            }
            "beat_envelope" => {
                // Get inputs
                let grid_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
                let grid = if let Some(edge) = grid_edge {
                    beat_grids
                        .get(&(edge.from_node.clone(), edge.from_port.clone()))
                        .or(context.beat_grid.as_ref())
                } else {
                    context.beat_grid.as_ref()
                };

                if let Some(grid) = grid {
                    // Params
                    let subdivision = node
                        .params
                        .get("subdivision")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0) as f32;
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
                        .unwrap_or(0.1) as f32;
                    let sustain = node
                        .params
                        .get("sustain")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32;
                    let decay = node
                        .params
                        .get("decay")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.5) as f32;
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

                    // Generate Pulses
                    let mut pulse_times = Vec::new();
                    let source_beats = if only_downbeats {
                        &grid.downbeats
                    } else {
                        &grid.beats
                    };

                    // Beat duration (approx for beat->time conversion)
                    let beat_len = if grid.bpm > 0.0 { 60.0 / grid.bpm } else { 0.5 };

                    // Subdivision logic
                    // Iterate beats pairs to interpolate subdivisions
                    // Or just generate grid points if regular?
                    // BeatGrid can be irregular. Best to interpolate between adjacent beats.
                    if source_beats.len() >= 2 {
                        for i in 0..source_beats.len() - 1 {
                            let t0 = source_beats[i];
                            let t1 = source_beats[i + 1];
                            let dur = t1 - t0;

                            let sub_count = subdivision.max(1.0).round() as usize;
                            for s in 0..sub_count {
                                let t = t0 + (dur * s as f32 / sub_count as f32);
                                // Apply offset (in beats)
                                // Approx offset in seconds = offset * beat_len
                                pulse_times.push(t + offset * beat_len);
                            }
                        }
                        // Handle last beat?
                        if let Some(last) = source_beats.last() {
                            pulse_times.push(*last + offset * beat_len);
                        }
                    } else if let Some(t) = source_beats.first() {
                        pulse_times.push(*t + offset * beat_len);
                    }

                    // Generate Signal
                    // Use Duration-Dependent Resolution for smooth control signals
                    let duration = (context.end_time - context.start_time).max(0.001);
                    let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
                    // Enforce minimum resolution for very short clips
                    let t_steps = t_steps.max(PREVIEW_LENGTH);

                    let mut data = Vec::with_capacity(t_steps);

                    // Convert envelope params from Beats to Seconds
                    let att_s = attack * beat_len;
                    let sus_s = sustain * beat_len;
                    let dec_s = decay * beat_len;

                    for i in 0..t_steps {
                        let t = context.start_time
                            + (i as f32 / (t_steps - 1).max(1) as f32) * duration;
                        let mut val = 0.0;

                        // Sum overlapping pulses
                        for &peak in &pulse_times {
                            // Optimization: skip if too far
                            if t < peak - att_s || t > peak + sus_s + dec_s {
                                continue;
                            }

                            val += calc_envelope(t, peak, att_s, sus_s, dec_s, a_curve, d_curve);
                        }

                        data.push(val * amp);
                    }

                    signal_outputs.insert(
                        (node.id.clone(), "out".into()),
                        Signal {
                            n: 1,
                            t: t_steps,
                            c: 1,
                            data,
                        },
                    );
                }
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

                // Check if we have all stems in cache
                let mut all_cached = true;
                for (stem_name, _) in STEM_OUTPUTS {
                    if stem_cache.get(track_id, stem_name).is_none() {
                        all_cached = false;
                        break;
                    }
                }

                let stems_map = if !all_cached {
                    let stems: Vec<(String, String)> = sqlx::query_as(
                        "SELECT stem_name, file_path FROM track_stems WHERE track_id = ?",
                    )
                    .bind(track_id)
                    .fetch_all(pool)
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
                            let (loaded_samples, loaded_rate) = load_or_decode_audio(
                                Path::new(file_path),
                                &cache_tag,
                                target_rate,
                            )
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
            "beat_clock" => {
                // Beat clock now reads from pre-loaded context (host responsibility)
                if let Some(ref grid) = context_beat_grid {
                    beat_grids.insert((node.id.clone(), "grid_out".into()), grid.clone());
                }
            }
            "view_signal" => {
                if compute_visualizations {
                    let input_edge = incoming_edges
                        .get(node.id.as_str())
                        .and_then(|edges| edges.first())
                        .ok_or_else(|| format!("View Signal node '{}' missing input", node.id))?;

                    let input_signal = signal_outputs
                        .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                        .ok_or_else(|| {
                            format!("View Signal node '{}' input signal not found", node.id)
                        })?;

                    view_results.insert(node.id.clone(), input_signal.clone());
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
                }

                // Re-do signal generation using `root_caches` if available
                if let Some(track_id) = audio_buffer.track_id {
                    if let Some(cache) = root_caches.get(&track_id) {
                        let duration = (context.end_time - context.start_time).max(0.001);
                        let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
                        let t_steps = t_steps.max(PREVIEW_LENGTH);
                        let mut signal_data = vec![0.0; t_steps * CHROMA_DIM];

                        // Naive O(T * S) rasterization - optimization possible but S is small (~100 sections)
                        for section in &cache.sections {
                            // Transform section time to local time
                            // Note: we don't need to snap to grid here necessarily, but keeping it consistent with Series is good.
                            // For raw signal, maybe precision is better? Let's use raw times.

                            let start_idx = ((section.start - context.start_time) / duration
                                * t_steps as f32)
                                .floor() as isize;
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

                        signal_outputs.insert(
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
                            beat_grids
                                .get(&(grid_edge.from_node.clone(), grid_edge.from_port.clone()))
                        })
                        .cloned();

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
            }
            "scalar" => {
                let value = node
                    .params
                    .get("value")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0) as f32;

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: 1,
                        t: 1,
                        c: 1,
                        data: vec![value],
                    },
                );
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

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: 1,
                        t: 1,
                        c: 3,
                        data: vec![r, g, b],
                    },
                );

                // Keep string output for legacy view if needed, but port type is Signal now.
                color_outputs.insert((node.id.clone(), "out".into()), color_json.to_string());
            }
            "harmony_color_visualizer" => {
                let signal_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "signal_in"))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' missing signal input",
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

                let harmony_signal = signal_outputs
                    .get(&(signal_edge.from_node.clone(), signal_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' harmony signal unavailable",
                            node.id
                        )
                    })?;

                let base_color_signal = signal_outputs
                    .get(&(color_edge.from_node.clone(), color_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' color input unavailable",
                            node.id
                        )
                    })?;

                // Extract base color (assume constant 1x1x3 signal or take first sample)
                let base_r = base_color_signal.data.get(0).copied().unwrap_or(1.0);
                let base_g = base_color_signal.data.get(1).copied().unwrap_or(1.0);
                let base_b = base_color_signal.data.get(2).copied().unwrap_or(1.0);

                let audio_buffer = audio_buffers
                    .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!(
                            "Harmony Color Visualizer '{}' audio input unavailable",
                            node.id
                        )
                    })?;

                // color_views.insert(node.id.clone(), base_color_json); // Removed logic for color_views based on JSON string
                // If we need to populate color_views for the UI to show the "base color" swatch, we might need to reconstruct the string or change UI to accept array.
                // For now, let's skip color_views population or put a dummy if needed. The UI might rely on it?
                // The UI node HarmonyColorVisualizerNode uses `baseColor` from `colorViews`.
                // Let's reconstruct a JSON string for it to keep UI happy.
                let base_color_json = format!(
                    r#"{{"r":{},"g":{},"b":{}}}"#,
                    (base_r * 255.0) as u8,
                    (base_g * 255.0) as u8,
                    (base_b * 255.0) as u8
                );
                color_views.insert(node.id.clone(), base_color_json);

                let palette_size = node
                    .params
                    .get("palette_size")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as usize)
                    .unwrap_or(4)
                    .max(2);

                // Process Signal to Color Signal
                // Output N=1, T=input.t, C=3 (RGB)
                let t_steps = harmony_signal.t;
                let mut rgb_data = Vec::with_capacity(t_steps * 3);
                let mut color_samples: Vec<SeriesSample> = Vec::with_capacity(t_steps);

                let duration = (context.end_time - context.start_time).max(0.001);

                // Naive Palette Generation (HSL shift) based on base color?
                // For now, hardcoded hue shift logic based on palette_size

                for t in 0..t_steps {
                    let time =
                        context.start_time + (t as f32 / (t_steps - 1).max(1) as f32) * duration;

                    // Find dominant pitch
                    let mut max_val = -1.0;
                    let mut max_idx = 0;
                    for c in 0..CHROMA_DIM {
                        // Assuming N=1 for harmony signal
                        let idx = t * CHROMA_DIM + c;
                        let val = harmony_signal.data.get(idx).copied().unwrap_or(0.0);
                        if val > max_val {
                            max_val = val;
                            max_idx = c;
                        }
                    }

                    // If signal is silence/empty
                    if max_val <= 0.001 {
                        rgb_data.push(0.0);
                        rgb_data.push(0.0);
                        rgb_data.push(0.0);
                        continue;
                    }

                    let palette_idx = max_idx % palette_size;

                    // Compute Brightness from Audio (short window)
                    // Window size: duration of 1 sample
                    let dt = duration / t_steps as f32;
                    let brightness = estimate_segment_brightness(
                        &audio_buffer.samples,
                        audio_buffer.sample_rate,
                        time,
                        time + dt,
                    );

                    // Compute RGB using Base Color and Palette Rotation
                    // Palette logic: Cycle channels R->G->B based on palette_idx
                    let (r, g, b) = match (palette_idx as usize) % 3 {
                        0 => (base_r, base_g, base_b),
                        1 => (base_g, base_b, base_r),
                        _ => (base_b, base_r, base_g),
                    };

                    let final_r = r * brightness;
                    let final_g = g * brightness;
                    let final_b = b * brightness;

                    rgb_data.push(final_r);
                    rgb_data.push(final_g);
                    rgb_data.push(final_b);

                    // Populate View Data (Series)
                    // Downsample for view if needed, but SeriesSample is efficient enough for modest T
                    if t % 4 == 0 {
                        // Downsample view 4x
                        color_samples.push(SeriesSample {
                            time,
                            values: vec![palette_idx as f32, brightness],
                            label: None,
                        });
                    }
                }

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: 1,
                        t: t_steps,
                        c: 3,
                        data: rgb_data,
                    },
                );

                let color_series = Series {
                    dim: 2,
                    labels: Some(vec!["palette_index".to_string(), "brightness".to_string()]),
                    samples: color_samples,
                };

                series_views.insert(node.id.clone(), color_series);
            }
            "lowpass_filter" | "highpass_filter" => {
                let audio_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                    .ok_or_else(|| {
                        format!("{} node '{}' missing audio input", node.type_id, node.id)
                    })?;

                let audio_buffer = audio_buffers
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

                audio_buffers.insert(
                    (node.id.clone(), "audio_out".into()),
                    AudioBuffer {
                        samples: filtered,
                        sample_rate: audio_buffer.sample_rate,
                        crop: audio_buffer.crop,
                        track_id: audio_buffer.track_id,
                        track_hash: audio_buffer.track_hash.clone(),
                    },
                );
            }
            "frequency_amplitude" => {
                let audio_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "audio_in"))
                    .ok_or_else(|| {
                        format!("Frequency Amplitude node '{}' missing audio input", node.id)
                    })?;

                let audio_buffer = audio_buffers
                    .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!(
                            "Frequency Amplitude node '{}' audio input unavailable",
                            node.id
                        )
                    })?;

                // Parse selected_frequency_ranges JSON
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
                    &frequency_ranges, // Pass the parsed ranges
                );

                // Raw Output
                signal_outputs.insert(
                    (node.id.clone(), "amplitude_out".into()),
                    Signal {
                        n: 1,
                        t: raw.len(),
                        c: 1,
                        data: raw,
                    },
                );
            }
            other => {
                println!("Encountered unknown node type '{}'", other);
            }
        }

        let node_ms = node_start.elapsed().as_secs_f64() * 1000.0;
        node_timings.push(NodeTiming {
            id: node.id.clone(),
            type_id: node.type_id.clone(),
            ms: node_ms,
        });
    }
    let nodes_exec_ms = nodes_exec_start.elapsed().as_secs_f64() * 1000.0;

    // Merge all Apply outputs into a single LayerTimeSeries
    let merge_start = Instant::now();
    let merged_layer = if !apply_outputs.is_empty() {
        let mut merged_primitives: HashMap<String, PrimitiveTimeSeries> = HashMap::new();

        for layer in apply_outputs {
            for prim in layer.primitives {
                let entry = merged_primitives
                    .entry(prim.primitive_id.clone())
                    .or_insert_with(|| PrimitiveTimeSeries {
                        primitive_id: prim.primitive_id.clone(),
                        color: None,
                        dimmer: None,
                        position: None,
                        strobe: None,
                    });

                // Simple merge (last write wins or union) - TODO: Conflict detection
                if prim.color.is_some() {
                    entry.color = prim.color;
                }
                if prim.dimmer.is_some() {
                    entry.dimmer = prim.dimmer;
                }
                if prim.position.is_some() {
                    entry.position = prim.position;
                }
                if prim.strobe.is_some() {
                    entry.strobe = prim.strobe;
                }
            }
        }

        Some(LayerTimeSeries {
            primitives: merged_primitives.into_values().collect(),
        })
    } else {
        None
    };
    let merge_ms = merge_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = run_start.elapsed().as_secs_f64() * 1000.0;

    if let Some(l) = &merged_layer {
        if config.log_summary {
            println!(
                "[run_graph #{run_id}] done primitives={} context_ms={:.2} node_exec_ms={:.2} merge_ms={:.2} total_ms={:.2}",
                l.primitives.len(),
                context_load_ms,
                nodes_exec_ms,
                merge_ms,
                total_ms
            );
            let mut top_nodes = node_timings.clone();
            top_nodes.sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap_or(std::cmp::Ordering::Equal));
            let top_nodes: Vec<String> = top_nodes
                .into_iter()
                .take(5)
                .map(|n| format!("{} ({}) {:.2}ms", n.id, n.type_id, n.ms))
                .collect();
            if !top_nodes.is_empty() {
                println!("[run_graph #{run_id}] slowest_nodes: {}", top_nodes.join(", "));
            }
        }
        if config.log_primitives {
            for p in &l.primitives {
                println!("  - Primitive: {}", p.primitive_id);
            }
        }
    } else if config.log_summary {
        println!(
            "[run_graph #{run_id}] No layer generated (empty apply outputs) context_ms={:.2} node_exec_ms={:.2} merge_ms={:.2} total_ms={:.2}",
            context_load_ms, nodes_exec_ms, merge_ms, total_ms
        );
    }

    // Render one frame at start_time for preview (or 0.0 if start is negative/unset)
    // In a real app, the "Engine" loop would call render_frame(layer, t) continuously.
    // Here, we just snapshot the start state so the frontend visualizer sees something immediately.
    let universe_state = if let Some(layer) = &merged_layer {
        Some(crate::engine::render_frame(layer, context.start_time))
    } else {
        None
    };

    Ok((
        RunResult {
            views: view_results,
            series_views,
            mel_specs,
            color_views,
            universe_state,
        },
        merged_layer,
    ))
}

fn calc_envelope(
    t: f32,
    peak: f32,
    attack: f32,
    sustain: f32,
    decay: f32,
    a_curve: f32,
    d_curve: f32,
) -> f32 {
    if t < peak - attack {
        0.0
    } else if t < peak {
        // Attack phase
        if attack <= 0.0 {
            return 1.0;
        }
        let x = (t - (peak - attack)) / attack;
        shape_curve(x, a_curve)
    } else if t < peak + sustain {
        // Sustain phase
        1.0
    } else if t < peak + sustain + decay {
        // Decay phase
        if decay <= 0.0 {
            return 0.0;
        }
        let x = (t - (peak + sustain)) / decay;
        shape_curve(1.0 - x, d_curve)
    } else {
        0.0
    }
}

fn shape_curve(x: f32, curve: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if curve.abs() < 0.001 {
        x // Linear
    } else if curve > 0.0 {
        // Convex / Snappy (Power > 1)
        // Map 0..1 to Power 1..6
        let p = 1.0 + curve * 5.0;
        x.powf(p)
    } else {
        // Concave / Swell (Inverse Power)
        // y = 1 - (1-x)^p
        let p = 1.0 + (-curve) * 5.0;
        1.0 - (1.0 - x).powf(p)
    }
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
                arg_values: None,
            };
            let stem_cache = StemCache::new();
            let fft_service = crate::audio::FftService::new();
            // Ignore the layer output for this test wrapper
            let (result, _) = run_graph_internal(
                &pool,
                None,
                &stem_cache,
                &fft_service,
                None,
                graph,
                context,
                GraphExecutionConfig::default(),
            )
            .await
            .expect("graph execution should succeed");
            result
        })
    }

    #[test]
    fn test_tensor_broadcasting_logic() {
        // Signal A: Spatial (N=4, T=1, C=1) -> [0, 1, 2, 3]
        let sig_a = Signal {
            n: 4,
            t: 1,
            c: 1,
            data: vec![0.0, 1.0, 2.0, 3.0],
        };

        // Signal B: Temporal (N=1, T=2, C=1) -> [10, 20]
        let sig_b = Signal {
            n: 1,
            t: 2,
            c: 1,
            data: vec![10.0, 20.0],
        };

        // Emulate the Math node logic
        let out_n = sig_a.n.max(sig_b.n);
        let out_t = sig_a.t.max(sig_b.t);
        let out_c = sig_a.c.max(sig_b.c);

        let mut result_data = Vec::new();

        for i in 0..out_n {
            let idx_a_n = if sig_a.n == 1 { 0 } else { i % sig_a.n };
            let idx_b_n = if sig_b.n == 1 { 0 } else { i % sig_b.n };

            for j in 0..out_t {
                let idx_a_t = if sig_a.t == 1 { 0 } else { j % sig_a.t };
                let idx_b_t = if sig_b.t == 1 { 0 } else { j % sig_b.t };

                for k in 0..out_c {
                    let idx_a_c = if sig_a.c == 1 { 0 } else { k % sig_a.c };
                    let idx_b_c = if sig_b.c == 1 { 0 } else { k % sig_b.c };

                    let flat_a = idx_a_n * (sig_a.t * sig_a.c) + idx_a_t * sig_a.c + idx_a_c;
                    let flat_b = idx_b_n * (sig_b.t * sig_b.c) + idx_b_t * sig_b.c + idx_b_c;

                    let val_a = sig_a.data.get(flat_a).copied().unwrap_or(0.0);
                    let val_b = sig_b.data.get(flat_b).copied().unwrap_or(0.0);

                    result_data.push(val_a + val_b);
                }
            }
        }

        assert_eq!(
            result_data,
            vec![10.0, 20.0, 11.0, 21.0, 12.0, 22.0, 13.0, 23.0]
        );
    }
}
