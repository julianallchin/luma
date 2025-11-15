use crate::database::Db;
use crate::playback::{PatternPlaybackState, PlaybackEntryData};
use crate::tracks::{
    generate_melspec, load_or_decode_audio, MelSpec, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH,
    TARGET_SAMPLE_RATE,
};
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
use tauri::State;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum PortType {
    Intensity,
    Audio,
    BeatGrid,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub enum ParamType {
    Number,
    Text,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PortDef {
    pub id: String,
    pub name: String,
    pub port_type: PortType,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ParamDef {
    pub id: String,
    pub name: String,
    pub param_type: ParamType,
    pub default_number: Option<f32>,
    pub default_text: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct NodeTypeDef {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub inputs: Vec<PortDef>,
    pub outputs: Vec<PortDef>,
    pub params: Vec<ParamDef>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct NodeInstance {
    pub id: String,
    pub type_id: String,
    #[ts(type = "Record<string, unknown>")]
    pub params: HashMap<String, Value>,
    pub position_x: Option<f64>,
    pub position_y: Option<f64>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Graph {
    pub nodes: Vec<NodeInstance>,
    pub edges: Vec<Edge>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct BeatGrid {
    pub beats: Vec<f32>,
    pub downbeats: Vec<f32>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct PatternEntrySummary {
    pub duration_seconds: f32,
    pub sample_rate: u32,
    pub sample_count: u32,
    pub beat_grid: Option<BeatGrid>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    pub views: HashMap<String, Vec<f32>>,
    pub mel_specs: HashMap<String, MelSpec>,
    pub pattern_entries: HashMap<String, PatternEntrySummary>,
}

struct RunArtifacts {
    result: RunResult,
    playback_entries: Vec<PlaybackEntryData>,
}

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
            id: "audio_source".into(),
            name: "Audio Source".into(),
            description: Some("Reads a selected track and publishes it into the graph.".into()),
            category: Some("Sources".into()),
            inputs: vec![],
            outputs: vec![
                PortDef {
                    id: "out".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
            ],
            params: vec![ParamDef {
                id: "trackId".into(),
                name: "Track".into(),
                param_type: ParamType::Number,
                default_number: Some(0.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "crop_downbeats".into(),
            name: "Crop Beat Grid".into(),
            description: Some("Trims audio and beat grid to a downbeat range.".into()),
            category: Some("Modifiers".into()),
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
            params: vec![
                ParamDef {
                    id: "startDownbeat".into(),
                    name: "Start Downbeat".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "endDownbeat".into(),
                    name: "End Downbeat".into(),
                    param_type: ParamType::Number,
                    default_number: Some(2.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "pattern_entry".into(),
            name: "Pattern Entry".into(),
            description: Some("Marks the audio segment used to preview this pattern.".into()),
            category: Some("Preview".into()),
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
            id: "view_channel".into(),
            name: "View Channel".into(),
            description: Some("Displays the incoming intensity channel.".into()),
            category: Some("Utilities".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Channel".into(),
                port_type: PortType::Intensity,
            }],
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
    ]
}

#[tauri::command]
pub async fn run_graph(
    db: State<'_, Db>,
    playback: State<'_, PatternPlaybackState>,
    graph: Graph,
) -> Result<RunResult, String> {
    let artifacts = run_graph_internal(&db.0, graph).await?;
    playback.update_entries(artifacts.playback_entries);
    Ok(artifacts.result)
}

async fn run_graph_internal(pool: &SqlitePool, graph: Graph) -> Result<RunArtifacts, String> {
    println!("Received graph with {} nodes to run.", graph.nodes.len());

    if graph.nodes.is_empty() {
        return Ok(RunArtifacts {
            result: RunResult {
                views: HashMap::new(),
                mel_specs: HashMap::new(),
                pattern_entries: HashMap::new(),
            },
            playback_entries: Vec::new(),
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
    }

    let mut output_buffers: HashMap<(String, String), Vec<f32>> = HashMap::new();
    let mut audio_buffers: HashMap<(String, String), AudioBuffer> = HashMap::new();
    let mut beat_grids: HashMap<(String, String), BeatGrid> = HashMap::new();
    let mut view_results: HashMap<String, Vec<f32>> = HashMap::new();
    let mut mel_specs: HashMap<String, MelSpec> = HashMap::new();
    let mut pattern_entries: HashMap<String, PatternEntrySummary> = HashMap::new();
    let mut playback_entries: Vec<PlaybackEntryData> = Vec::new();

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
            "crop_downbeats" => {
                let start_param = node
                    .params
                    .get("startDownbeat")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(1.0);
                let end_param = node
                    .params
                    .get("endDownbeat")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(2.0);

                let start_index = start_param.floor().max(1.0) as usize;
                let end_index = end_param.floor().max(1.0) as usize;

                if end_index <= start_index {
                    return Err(format!(
                        "Crop node '{}' requires endDownbeat > startDownbeat",
                        node.id
                    ));
                }

                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();

                let audio_edge = input_edges
                    .iter()
                    .find(|edge| edge.to_port == "audio_in")
                    .ok_or_else(|| format!("Crop node '{}' missing audio input", node.id))?;
                let beat_edge = input_edges
                    .iter()
                    .find(|edge| edge.to_port == "grid_in")
                    .ok_or_else(|| format!("Crop node '{}' missing beat grid input", node.id))?;

                let audio_buffer = audio_buffers
                    .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                    .ok_or_else(|| format!("Crop node '{}' audio input unavailable", node.id))?;
                let beat_grid = beat_grids
                    .get(&(beat_edge.from_node.clone(), beat_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!("Crop node '{}' beat grid input unavailable", node.id)
                    })?;

                if beat_grid.downbeats.is_empty() {
                    return Err(format!(
                        "Crop node '{}' received an empty downbeat sequence",
                        node.id
                    ));
                }

                if end_index > beat_grid.downbeats.len() {
                    return Err(format!(
                        "Crop node '{}' endDownbeat {} exceeds available downbeats ({})",
                        node.id,
                        end_index,
                        beat_grid.downbeats.len()
                    ));
                }

                let start_time = beat_grid.downbeats[start_index - 1];
                let end_time = beat_grid.downbeats[end_index - 1];

                if end_time <= start_time {
                    return Err(format!(
                        "Crop node '{}' computed invalid time range [{}, {}]",
                        node.id, start_time, end_time
                    ));
                }

                let sample_rate = audio_buffer.sample_rate;
                let mut start_sample = (start_time * sample_rate as f32).floor().max(0.0) as usize;
                start_sample = start_sample.min(audio_buffer.samples.len().saturating_sub(1));
                let mut end_sample = (end_time * sample_rate as f32).ceil() as usize;
                end_sample = end_sample.min(audio_buffer.samples.len());
                if end_sample <= start_sample {
                    return Err(format!(
                        "Crop node '{}' produced empty audio segment",
                        node.id
                    ));
                }

                let cropped_audio = audio_buffer.samples[start_sample..end_sample].to_vec();

                if cropped_audio.is_empty() {
                    return Err(format!(
                        "Crop node '{}' produced empty audio after trimming",
                        node.id
                    ));
                }

                // Normalize beats and downbeats relative to the start time.
                let beat_start = start_time;
                let beat_end = end_time;

                let mut cropped_beats = Vec::new();
                for &beat in &beat_grid.beats {
                    if beat >= beat_start && beat < beat_end {
                        cropped_beats.push(beat - beat_start);
                    }
                }

                let mut cropped_downbeats = Vec::new();
                for idx in (start_index - 1)..end_index {
                    let value = beat_grid.downbeats[idx] - beat_start;
                    cropped_downbeats.push(value.max(0.0));
                }

                if cropped_downbeats.is_empty() {
                    return Err(format!(
                        "Crop node '{}' produced no downbeats after trimming",
                        node.id
                    ));
                }

                audio_buffers.insert(
                    (node.id.clone(), "audio_out".into()),
                    AudioBuffer {
                        samples: cropped_audio,
                        sample_rate,
                    },
                );

                beat_grids.insert(
                    (node.id.clone(), "grid_out".into()),
                    BeatGrid {
                        beats: cropped_beats,
                        downbeats: cropped_downbeats,
                    },
                );
            }
            "pattern_entry" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();

                let audio_edge = input_edges
                    .iter()
                    .find(|edge| edge.to_port == "audio_in")
                    .ok_or_else(|| {
                        format!("Pattern entry node '{}' missing audio input", node.id)
                    })?;

                let audio_buffer = audio_buffers
                    .get(&(audio_edge.from_node.clone(), audio_edge.from_port.clone()))
                    .ok_or_else(|| {
                        format!("Pattern entry node '{}' audio input unavailable", node.id)
                    })?
                    .clone();

                let beat_edge = input_edges.iter().find(|edge| edge.to_port == "grid_in");

                let beat_grid = if let Some(edge) = beat_edge {
                    Some(
                        beat_grids
                            .get(&(edge.from_node.clone(), edge.from_port.clone()))
                            .ok_or_else(|| {
                                format!(
                                    "Pattern entry node '{}' beat grid input unavailable",
                                    node.id
                                )
                            })?
                            .clone(),
                    )
                } else {
                    None
                };

                audio_buffers.insert((node.id.clone(), "audio_out".into()), audio_buffer.clone());
                if let Some(grid) = beat_grid.clone() {
                    beat_grids.insert((node.id.clone(), "grid_out".into()), grid);
                }

                let duration_seconds = if audio_buffer.sample_rate == 0 {
                    0.0
                } else {
                    audio_buffer.samples.len() as f32 / audio_buffer.sample_rate as f32
                };

                let sample_count = audio_buffer.samples.len().min(u32::MAX as usize) as u32;

                playback_entries.push(PlaybackEntryData {
                    node_id: node.id.clone(),
                    samples: audio_buffer.samples.clone(),
                    sample_rate: audio_buffer.sample_rate,
                    beat_grid: beat_grid.clone(),
                });

                pattern_entries.insert(
                    node.id.clone(),
                    PatternEntrySummary {
                        duration_seconds,
                        sample_rate: audio_buffer.sample_rate,
                        sample_count,
                        beat_grid,
                    },
                );
            }
            "audio_source" => {
                let track_id = node
                    .params
                    .get("trackId")
                    .and_then(|value| value.as_f64())
                    .map(|value| value as i64)
                    .ok_or_else(|| {
                        format!("Audio source node '{}' requires a track selection", node.id)
                    })?;

                eprintln!(
                    "[run_graph] node '{}' resolved track_id {}",
                    node.id, track_id
                );

                let track_row: Option<(String, String)> =
                    sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
                        .bind(track_id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| format!("Failed to fetch track path: {}", e))?;

                let (file_path, track_hash) =
                    track_row.ok_or_else(|| format!("Track {} not found", track_id))?;

                eprintln!("[run_graph] node '{}' loading '{}'", node.id, file_path);

                let decode_start = std::time::Instant::now();
                let path = Path::new(&file_path);
                let (samples, sample_rate) =
                    load_or_decode_audio(path, &track_hash, TARGET_SAMPLE_RATE)
                        .map_err(|e| format!("Failed to decode track: {}", e))?;
                println!(
                    "[run_graph] decoded '{}' ({} samples @ {} Hz) in {:.2?}",
                    file_path,
                    samples.len(),
                    sample_rate,
                    decode_start.elapsed()
                );

                if samples.is_empty() {
                    return Err(format!(
                        "Audio source node '{}' produced no samples",
                        node.id
                    ));
                }

                audio_buffers.insert(
                    (node.id.clone(), "out".into()),
                    AudioBuffer {
                        samples,
                        sample_rate,
                    },
                );

                if let Some((beats_json, downbeats_json)) = sqlx::query_as::<_, (String, String)>(
                    "SELECT beats_json, downbeats_json FROM track_beats WHERE track_id = ?",
                )
                .bind(track_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("Failed to load beat data: {}", e))?
                {
                    let beats: Vec<f32> = serde_json::from_str(&beats_json)
                        .map_err(|e| format!("Failed to parse beats data: {}", e))?;
                    let downbeats: Vec<f32> = serde_json::from_str(&downbeats_json)
                        .map_err(|e| format!("Failed to parse downbeats data: {}", e))?;
                    beat_grids.insert(
                        (node.id.clone(), "grid".into()),
                        BeatGrid { beats, downbeats },
                    );
                } else {
                    eprintln!(
                        "[run_graph] no beat data stored for track {}; beat outputs unavailable",
                        track_id
                    );
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
            other => {
                println!("Encountered unknown node type '{}'", other);
            }
        }
    }

    Ok(RunArtifacts {
        result: RunResult {
            views: view_results,
            mel_specs,
            pattern_entries,
        },
        playback_entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(graph: Graph) -> RunResult {
        tauri::async_runtime::block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:")
                .await
                .expect("in-memory db");
            run_graph_internal(&pool, graph)
                .await
                .expect("graph execution should succeed")
                .result
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
}
