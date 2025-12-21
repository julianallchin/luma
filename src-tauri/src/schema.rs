use crate::audio::{
    calculate_frequency_amplitude, generate_melspec, highpass_filter, load_or_decode_audio,
    lowpass_filter, StemCache, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH,
};
use crate::database::Db;
use crate::fixtures::layout::compute_head_offsets;
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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

// Graph execution returns preview data (channels, mel specs, series, colors).
#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "gradient".into(),
            name: "Gradient".into(),
            description: Some("Interpolates between start and end colors based on a signal (0..1).".into()),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Signal".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "start_color".into(),
                    name: "Start Color".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "end_color".into(),
                    name: "End Color".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "start_color".into(),
                    name: "Start Color".into(),
                    param_type: ParamType::Text,
                    default_number: None,
                    default_text: Some("#000000".into()),
                },
                ParamDef {
                    id: "end_color".into(),
                    name: "End Color".into(),
                    param_type: ParamType::Text,
                    default_number: None,
                    default_text: Some("#ffffff".into()),
                },
            ],
        },
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
            id: "random_select_mask".into(),
            name: "Random Select Mask".into(),
            description: Some("Randomly selects N items based on a trigger signal.".into()),
            category: Some("Selection".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "trigger".into(),
                    name: "Trigger".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Mask".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "count".into(),
                    name: "Count".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "avoid_repeat".into(),
                    name: "Avoid Repeat".into(),
                    param_type: ParamType::Number, // 0 or 1
                    default_number: Some(1.0),
                    default_text: None,
                },
            ],
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
            id: "chroma_palette".into(),
            name: "Harmonic Palette".into(),
            description: Some("Maps the 12 chroma pitches to colors.".into()),
            category: Some("Color".into()),
            inputs: vec![PortDef {
                id: "chroma".into(),
                name: "Chroma".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "palette".into(),
                name: "Palette JSON".into(),
                param_type: ParamType::Text,
                default_text: Some("Rainbow".into()),
                default_number: None,
            }],
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
        NodeTypeDef {
            id: "spectral_shift".into(),
            name: "Spectral Shift".into(),
            description: Some("Rotates color hue based on the dominant musical key.".into()),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Base Color".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "chroma".into(),
                    name: "Chroma".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "strength".into(),
                name: "Strength".into(),
                param_type: ParamType::Number,
                default_number: Some(1.0),
                default_text: None,
            }],
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
    let project_pool = Some(&db.0);

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
        project_pool,
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
        logits_path: Option<String>,
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

    fn beat_grid_relative_to_crop(grid: &BeatGrid, crop: Option<&AudioCrop>) -> BeatGrid {
        if let Some(crop) = crop {
            let start = crop.start_seconds;
            let end = crop.end_seconds.max(start);

            let beats: Vec<f32> = grid
                .beats
                .iter()
                .copied()
                .filter(|t| *t >= start && *t <= end)
                .map(|t| t - start)
                .collect();
            let downbeats: Vec<f32> = grid
                .downbeats
                .iter()
                .copied()
                .filter(|t| *t >= start && *t <= end)
                .map(|t| t - start)
                .collect();

            BeatGrid {
                beats,
                downbeats,
                bpm: grid.bpm,
                downbeat_offset: grid.downbeat_offset - start,
                beats_per_bar: grid.beats_per_bar,
            }
        } else {
            grid.clone()
        }
    }

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

    // Parse a hex color string (e.g., "#ff0000") into normalized RGBA tuple
    let parse_hex_color = |hex: &str| -> (f32, f32, f32, f32) {
        let hex = hex.trim_start_matches('#');
        if hex.len() >= 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f32 / 255.0;
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f32 / 255.0;
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f32 / 255.0;
            let a = if hex.len() >= 8 {
                u8::from_str_radix(&hex[6..8], 16).unwrap_or(255) as f32 / 255.0
            } else {
                1.0
            };
            (r, g, b, a)
        } else {
            (0.0, 0.0, 0.0, 1.0)
        }
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
        let (context_file_path, track_hash) =
            crate::database::local::tracks::get_track_path_and_hash(pool, context.track_id)
                .await
                .map_err(|e| format!("Failed to fetch track path: {}", e))?;

        let (context_full_samples, sample_rate, track_hash): (Vec<f32>, u32, String) =
            if let Some(shared) = config.shared_audio.as_ref() {
                if shared.track_id != context.track_id {
                    return Err(format!(
                        "Shared audio provided for track {} but context track is {}",
                        shared.track_id, context.track_id
                    ));
                }
                (
                    shared.samples.as_ref().clone(),
                    shared.sample_rate,
                    shared.track_hash.clone(),
                )
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
            crate::database::local::tracks::get_track_beats_pool(pool, context.track_id)
                .await
                .map_err(|e| format!("Failed to load beat data: {}", e))?
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
                    let value = arg_values.get(&arg.id).unwrap_or(&arg.default_value);

                    match arg.arg_type {
                        PatternArgType::Color => {
                            let (r, g, b, a) = parse_color_value(value);
                            signal_outputs.insert(
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
                            color_outputs
                                .insert((node.id.clone(), arg.id.clone()), color_json.clone());
                            color_views.insert(format!("{}:{}", node.id, arg.id), color_json);
                        }
                        PatternArgType::Scalar => {
                            let scalar_value = value.as_f64().unwrap_or(0.0) as f32;
                            signal_outputs.insert(
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
                    let fixtures = crate::database::local::fixtures::get_all_fixtures(proj_pool)
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
            "round" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

                let Some(input_edge) = input_edge else {
                    eprintln!(
                        "[run_graph] round '{}' missing signal input; skipping",
                        node.id
                    );
                    continue;
                };

                let Some(signal) = signal_outputs
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                else {
                    eprintln!(
                        "[run_graph] round '{}' input signal unavailable; skipping",
                        node.id
                    );
                    continue;
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
            "ramp" => {
                let grid_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|e| e.iter().find(|x| x.to_port == "grid"));
                let grid = grid_edge.and_then(|edge| {
                    beat_grids.get(&(edge.from_node.clone(), edge.from_port.clone()))
                });

                // Beat grid input is required
                let Some(grid) = grid else {
                    continue;
                };

                let bpm = grid.bpm;

                // Determine simulation steps
                let duration = (context.end_time - context.start_time).max(0.001);
                let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
                let t_steps = t_steps.max(PREVIEW_LENGTH);

                let mut data = Vec::with_capacity(t_steps);

                for i in 0..t_steps {
                    let time =
                        context.start_time + (i as f32 / (t_steps - 1).max(1) as f32) * duration;

                    // Beat position relative to pattern start (0 to n_beats)
                    let time_in_pattern = time - context.start_time;
                    let beat_in_pattern = time_in_pattern * (bpm / 60.0);
                    data.push(beat_in_pattern);
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
            "ramp_between" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let grid_edge = input_edges.iter().find(|x| x.to_port == "grid");
                let start_edge = input_edges.iter().find(|x| x.to_port == "start");
                let end_edge = input_edges.iter().find(|x| x.to_port == "end");

                let grid = grid_edge.and_then(|edge| {
                    beat_grids.get(&(edge.from_node.clone(), edge.from_port.clone()))
                });
                let start_signal = start_edge.and_then(|edge| {
                    signal_outputs.get(&(edge.from_node.clone(), edge.from_port.clone()))
                });
                let end_signal = end_edge.and_then(|edge| {
                    signal_outputs.get(&(edge.from_node.clone(), edge.from_port.clone()))
                });

                // All inputs are required
                let (Some(grid), Some(start_signal), Some(end_signal)) =
                    (grid, start_signal, end_signal)
                else {
                    continue;
                };

                let bpm = grid.bpm;

                // Determine simulation steps
                let duration = (context.end_time - context.start_time).max(0.001);
                let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
                let t_steps = t_steps.max(PREVIEW_LENGTH);
                let total_beats = (duration * (bpm / 60.0)).max(0.0001);

                let mut data = Vec::with_capacity(t_steps);
                for i in 0..t_steps {
                    let time =
                        context.start_time + (i as f32 / (t_steps - 1).max(1) as f32) * duration;

                    let time_in_pattern = time - context.start_time;
                    let beat_in_pattern = time_in_pattern * (bpm / 60.0);
                    let progress = (beat_in_pattern / total_beats).clamp(0.0, 1.0);

                    let start_idx = (i.min(start_signal.data.len().saturating_sub(1))) as usize;
                    let end_idx = (i.min(end_signal.data.len().saturating_sub(1))) as usize;
                    let start_val = start_signal.data.get(start_idx).copied().unwrap_or(0.0);
                    let end_val = end_signal.data.get(end_idx).copied().unwrap_or(0.0);

                    data.push(start_val + (end_val - start_val) * progress);
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
            "random_select_mask" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let sel_edge = input_edges.iter().find(|e| e.to_port == "selection");
                let trig_edge = input_edges.iter().find(|e| e.to_port == "trigger");

                let selection_opt = sel_edge
                    .and_then(|e| selections.get(&(e.from_node.clone(), e.from_port.clone())));
                let trigger_opt = trig_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

                if let (Some(selection), Some(trigger)) = (selection_opt, trigger_opt) {
                    let count = node
                        .params
                        .get("count")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0) as usize;
                    let avoid_repeat = node
                        .params
                        .get("avoid_repeat")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0)
                        > 0.5;

                    let n = selection.items.len();
                    let t_steps = trigger.t;

                    let mut mask_data = vec![0.0; n * t_steps];

                    // Helper for hashing
                    fn hash_combine(seed: u64, v: u64) -> u64 {
                        let mut x = seed ^ v;
                        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                        x ^ (x >> 31)
                    }

                    // Node ID hash
                    let mut node_hasher = std::collections::hash_map::DefaultHasher::new();
                    std::hash::Hash::hash(&node.id, &mut node_hasher);
                    let node_seed = std::hash::Hasher::finish(&node_hasher);

                    // Track previous selection for avoid_repeat
                    let mut prev_selected: Vec<usize> = Vec::new();
                    let mut prev_trig_seed: Option<i64> = None;
                    // Use system time for true randomness across pattern executions
                    let time_seed = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    // Counter for additional randomness on each trigger change within this execution
                    let mut selection_counter: u64 = 0;

                    for t in 0..t_steps {
                        // Get trigger value at this time step.
                        // Broadcast Trigger N: use index 0 since it's likely a control signal.
                        let trig_val = trigger.data.get(t * trigger.c).copied().unwrap_or(0.0);

                        // Seed combines: node_id + time + trigger_value + counter
                        let trig_seed = (trig_val * 1000.0) as i64; // Sensitivity 0.001
                        let step_seed = hash_combine(
                            hash_combine(hash_combine(node_seed, time_seed), trig_seed as u64),
                            selection_counter,
                        );

                        // Check if trigger changed (new selection event)
                        let trigger_changed = prev_trig_seed.is_none_or(|prev| prev != trig_seed);

                        // Generate scores for each item
                        let mut scores: Vec<(usize, u64)> = (0..n)
                            .map(|i| {
                                let item_seed = hash_combine(step_seed, i as u64);
                                (i, item_seed)
                            })
                            .collect();

                        // Sort by score (random shuffle)
                        scores.sort_by_key(|&(_, s)| s);

                        // Determine selection based on trigger state
                        let selected: Vec<usize> = if !trigger_changed && !prev_selected.is_empty()
                        {
                            // Trigger unchanged - reuse previous selection
                            prev_selected.clone()
                        } else if avoid_repeat && trigger_changed && !prev_selected.is_empty() {
                            // Trigger changed with avoid_repeat - filter out previous selection
                            let mut available: Vec<(usize, u64)> = scores
                                .iter()
                                .filter(|(idx, _)| !prev_selected.contains(idx))
                                .copied()
                                .collect();

                            // If not enough available, add back from prev_selected by score
                            if available.len() < count {
                                let mut from_prev: Vec<(usize, u64)> = scores
                                    .iter()
                                    .filter(|(idx, _)| prev_selected.contains(idx))
                                    .copied()
                                    .collect();
                                available.append(&mut from_prev);
                            }

                            let new_selected: Vec<usize> = available
                                .into_iter()
                                .take(count)
                                .map(|(idx, _)| idx)
                                .collect();
                            prev_selected = new_selected.clone();
                            prev_trig_seed = Some(trig_seed);
                            selection_counter += 1;
                            new_selected
                        } else {
                            // First selection or avoid_repeat disabled
                            let new_selected: Vec<usize> =
                                scores.into_iter().take(count).map(|(idx, _)| idx).collect();
                            prev_selected = new_selected.clone();
                            prev_trig_seed = Some(trig_seed);
                            selection_counter += 1;
                            new_selected
                        };

                        // Set 1.0 for selected items
                        for idx in &selected {
                            let out_idx = idx * t_steps + t;
                            mask_data[out_idx] = 1.0;
                        }
                    }

                    signal_outputs.insert(
                        (node.id.clone(), "out".into()),
                        Signal {
                            n,
                            t: t_steps,
                            c: 1,
                            data: mask_data,
                        },
                    );
                }
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
            "normalize" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

                let Some(input_edge) = input_edge else {
                    eprintln!(
                        "[run_graph] normalize '{}' missing signal input; skipping",
                        node.id
                    );
                    continue;
                };

                let Some(signal) = signal_outputs
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                else {
                    eprintln!(
                        "[run_graph] normalize '{}' input signal unavailable; skipping",
                        node.id
                    );
                    continue;
                };

                let mut min_val = f32::INFINITY;
                let mut max_val = f32::NEG_INFINITY;
                let mut saw_finite = false;

                for &val in &signal.data {
                    if !val.is_finite() {
                        continue;
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
                                continue;
                            }
                            data.push(((val - min_val) / range).clamp(0.0, 1.0));
                        }
                    }
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
            "falloff" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

                let Some(input_edge) = input_edge else {
                    eprintln!(
                        "[run_graph] falloff '{}' missing signal input; skipping",
                        node.id
                    );
                    continue;
                };

                let Some(signal) = signal_outputs
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                else {
                    eprintln!(
                        "[run_graph] falloff '{}' input signal unavailable; skipping",
                        node.id
                    );
                    continue;
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
            "invert" => {
                let input_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|e| e.to_port == "in"));

                let Some(input_edge) = input_edge else {
                    eprintln!(
                        "[run_graph] invert '{}' missing signal input; skipping",
                        node.id
                    );
                    continue;
                };

                let Some(signal) = signal_outputs
                    .get(&(input_edge.from_node.clone(), input_edge.from_port.clone()))
                else {
                    eprintln!(
                        "[run_graph] invert '{}' input signal unavailable; skipping",
                        node.id
                    );
                    continue;
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
                    continue;
                }

                let mid = (max_v + min_v) * 0.5;

                let mut data = Vec::with_capacity(signal.data.len());
                for &v in &signal.data {
                    // Reflect around midpoint; clamp to observed range to avoid numeric overshoot.
                    let reflected = 2.0 * mid - v;
                    data.push(reflected.clamp(min_v, max_v));
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
                                speed: None,
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

                                samples.push(SeriesSample {
                                    time: context.start_time,
                                    values: vec![r, g, b, a],
                                    label: None,
                                });
                                samples.push(SeriesSample {
                                    time: context.end_time,
                                    values: vec![r, g, b, a],
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

                                    let time = context.start_time
                                        + (t as f32 / (signal.t - 1).max(1) as f32) * duration;
                                    samples.push(SeriesSample {
                                        time,
                                        values: vec![r, g, b, a],
                                        label: None,
                                    });
                                }
                            }

                            primitives.push(PrimitiveTimeSeries {
                                primitive_id: item.id.clone(),
                                color: Some(Series {
                                    dim: 4,
                                    labels: None,
                                    samples,
                                }),
                                dimmer: None,
                                position: None,
                                strobe: None,
                                speed: None,
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
                                speed: None,
                            });
                        }

                        apply_outputs.push(LayerTimeSeries { primitives });
                    }
                }
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
                    continue;
                };
                let Some(selection) =
                    selections.get(&(sel_e.from_node.clone(), sel_e.from_port.clone()))
                else {
                    continue;
                };

                // Pan and/or tilt may be disconnected; treat missing axis as "hold" by writing NaN.
                let pan_signal = pan_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));
                let tilt_signal = tilt_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

                if pan_signal.is_none() && tilt_signal.is_none() {
                    continue;
                }

                let t_steps = pan_signal
                    .map(|s| s.t)
                    .unwrap_or(1)
                    .max(tilt_signal.map(|s| s.t).unwrap_or(1))
                    .max(1);
                let duration = (context.end_time - context.start_time).max(0.001);

                let mut primitives = Vec::new();
                for (i, item) in selection.items.iter().enumerate() {
                    let (pan_n, pan_t_max) = if let Some(pan) = pan_signal {
                        (if pan.n == 1 { 0 } else { i % pan.n }, pan.t)
                    } else {
                        (0, 1)
                    };
                    let (tilt_n, tilt_t_max) = if let Some(tilt) = tilt_signal {
                        (if tilt.n == 1 { 0 } else { i % tilt.n }, tilt.t)
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
                }

                apply_outputs.push(LayerTimeSeries { primitives });
            }
            "apply_speed" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
                let speed_edge = input_edges.iter().find(|e| e.to_port == "speed");

                if let (Some(sel_e), Some(spd_e)) = (selection_edge, speed_edge) {
                    if let (Some(selection), Some(signal)) = (
                        selections.get(&(sel_e.from_node.clone(), sel_e.from_port.clone())),
                        signal_outputs.get(&(spd_e.from_node.clone(), spd_e.from_port.clone())),
                    ) {
                        let mut primitives = Vec::new();
                        let duration = (context.end_time - context.start_time).max(0.001);

                        for (i, item) in selection.items.iter().enumerate() {
                            let sig_idx = if signal.n == 1 { 0 } else { i % signal.n };
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
                        }

                        apply_outputs.push(LayerTimeSeries { primitives });
                    }
                }
            }
            "orbit" => {
                // Get beat grid for timing
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

                // Get phase offset input (optional)
                let phase_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|e| e.iter().find(|x| x.to_port == "phase"));
                let phase_signal = phase_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

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
                        let angle =
                            2.0 * std::f32::consts::PI * (speed_cycles * beats + phase_offset);

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

                signal_outputs.insert(
                    (node.id.clone(), "x".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: x_data,
                    },
                );
                signal_outputs.insert(
                    (node.id.clone(), "y".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: y_data,
                    },
                );
                signal_outputs.insert(
                    (node.id.clone(), "z".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: z_data,
                    },
                );
            }
            "random_position" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let trigger_edge = input_edges.iter().find(|e| e.to_port == "trigger");

                let trigger_opt = trigger_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

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
                            let rand_x =
                                (hash_combine(step_seed, 0) as f64 / u64::MAX as f64) as f32;
                            let rand_y =
                                (hash_combine(step_seed, 1) as f64 / u64::MAX as f64) as f32;
                            let rand_z =
                                (hash_combine(step_seed, 2) as f64 / u64::MAX as f64) as f32;

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

                    signal_outputs.insert(
                        (node.id.clone(), "x".into()),
                        Signal {
                            n: 1,
                            t: t_steps,
                            c: 1,
                            data: x_data,
                        },
                    );
                    signal_outputs.insert(
                        (node.id.clone(), "y".into()),
                        Signal {
                            n: 1,
                            t: t_steps,
                            c: 1,
                            data: y_data,
                        },
                    );
                    signal_outputs.insert(
                        (node.id.clone(), "z".into()),
                        Signal {
                            n: 1,
                            t: t_steps,
                            c: 1,
                            data: z_data,
                        },
                    );
                }
            }
            "gradient" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();

                let signal_edge = input_edges.iter().find(|e| e.to_port == "in");
                let start_color_edge = input_edges.iter().find(|e| e.to_port == "start_color");
                let end_color_edge = input_edges.iter().find(|e| e.to_port == "end_color");

                let Some(signal_edge) = signal_edge else {
                    continue;
                };
                let signal = signal_outputs
                    .get(&(signal_edge.from_node.clone(), signal_edge.from_port.clone()));

                // If input signal is missing, skip
                let Some(signal) = signal else {
                    continue;
                };

                // Get start color from connected edge or params
                let start_color = if let Some(edge) = start_color_edge {
                    signal_outputs
                        .get(&(edge.from_node.clone(), edge.from_port.clone()))
                        .map(|s| {
                            // Extract RGBA from signal (expects c=4)
                            let r = s.data.first().copied().unwrap_or(0.0);
                            let g = s.data.get(1).copied().unwrap_or(0.0);
                            let b = s.data.get(2).copied().unwrap_or(0.0);
                            let a = s.data.get(3).copied().unwrap_or(1.0);
                            (r, g, b, a)
                        })
                        .unwrap_or((0.0, 0.0, 0.0, 1.0))
                } else {
                    // Parse from param (hex color string)
                    let hex = node
                        .params
                        .get("start_color")
                        .and_then(|v| v.as_str())
                        .unwrap_or("#000000");
                    parse_hex_color(hex)
                };

                // Get end color from connected edge or params
                let end_color = if let Some(edge) = end_color_edge {
                    signal_outputs
                        .get(&(edge.from_node.clone(), edge.from_port.clone()))
                        .map(|s| {
                            // Extract RGBA from signal (expects c=4)
                            let r = s.data.first().copied().unwrap_or(1.0);
                            let g = s.data.get(1).copied().unwrap_or(1.0);
                            let b = s.data.get(2).copied().unwrap_or(1.0);
                            let a = s.data.get(3).copied().unwrap_or(1.0);
                            (r, g, b, a)
                        })
                        .unwrap_or((1.0, 1.0, 1.0, 1.0))
                } else {
                    // Parse from param (hex color string)
                    let hex = node
                        .params
                        .get("end_color")
                        .and_then(|v| v.as_str())
                        .unwrap_or("#ffffff");
                    parse_hex_color(hex)
                };

                let mut data = Vec::with_capacity(signal.n * signal.t * 4);

                // Process each sample - interpolate between start and end color
                // Input signal might have c > 1, take 1st channel as the mix factor
                for chunk in signal.data.chunks(signal.c) {
                    let mix = chunk.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);

                    // Linear interpolation between start and end colors
                    let r = start_color.0 + (end_color.0 - start_color.0) * mix;
                    let g = start_color.1 + (end_color.1 - start_color.1) * mix;
                    let b = start_color.2 + (end_color.2 - start_color.2) * mix;
                    let a = start_color.3 + (end_color.3 - start_color.3) * mix;

                    data.push(r);
                    data.push(g);
                    data.push(b);
                    data.push(a);
                }

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: signal.n,
                        t: signal.t,
                        c: 4,
                        data,
                    },
                );
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

                // Check for subdivision signal input
                let subdivision_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|e| e.iter().find(|x| x.to_port == "subdivision"));
                let subdivision_signal = subdivision_edge.and_then(|edge| {
                    signal_outputs.get(&(edge.from_node.clone(), edge.from_port.clone()))
                });

                if let Some(grid) = grid {
                    // Params - use signal input if connected, otherwise use parameter
                    let subdivision = if let Some(sig) = subdivision_signal {
                        // Sample the signal at midpoint of the time range
                        // Use the signal value directly as subdivision (0.25, 0.5, 1, 2, 4, etc.)
                        let mid_t =
                            (context.start_time + context.end_time) / 2.0 - context.start_time;
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

                    // Generate Pulses
                    let mut pulse_times = Vec::new();
                    let source_beats = if only_downbeats {
                        &grid.downbeats
                    } else {
                        &grid.beats
                    };

                    // Beat duration (approx for beat->time conversion)
                    let beat_len = if grid.bpm > 0.0 { 60.0 / grid.bpm } else { 0.5 };
                    let beat_step_beats = if subdivision.abs() < 1e-3 {
                        1.0
                    } else {
                        (1.0 / subdivision).abs()
                    };

                    // Subdivision logic
                    // Interpret subdivision as pulses-per-beat (e.g. 0.5 = every 2 beats).
                    // Walk fractional beat positions and interpolate between grid points.
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
                            beat_pos += beat_step.max(1e-4); // guard against zero/denormals
                        }
                    }

                    // Generate Signal
                    // Use Duration-Dependent Resolution for smooth control signals
                    let duration = (context.end_time - context.start_time).max(0.001);
                    let t_steps = (duration * SIMULATION_RATE).ceil() as usize;
                    // Enforce minimum resolution for very short clips
                    let t_steps = t_steps.max(PREVIEW_LENGTH);

                    let mut data = Vec::with_capacity(t_steps);

                    // Use the actual pulse spacing (derived from the grid/subdivision) so
                    // envelopes span the full distance between pulses, including downbeats.
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

                    // The frontend often loops the preview segment. To keep the generated signal
                    // stable at segment boundaries (no one-sample spikes), we:
                    // - sample time in [start, end) (end-exclusive), and
                    // - snap pulses that land within one sample of the start/end to avoid
                    //   floating-point edge misses.
                    let sample_dt = duration / (t_steps.max(1) as f32);
                    let snap_eps = (sample_dt * 1.1).max(1e-6);

                    if !pulse_times.is_empty() {
                        pulse_times
                            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

                        // If we have a non-zero attack AND no post-peak phases, a pulse exactly
                        // at the segment start produces an immediate 1->0 drop (peak at t=start,
                        // then zero on the next sample). That shows up as a stutter when
                        // looping/chaining segments.
                        //
                        // Only apply this fix for "attack-only" shapes; for attack+decay/sustain/
                        // release, we need the start pulse so the segment doesn't flatline at 0.
                        let post_peak_span = dec_s + sus_s + rel_s;
                        if att_s > 1e-6 && post_peak_span <= 1e-6 {
                            let has_later_pulse = pulse_times
                                .iter()
                                .any(|p| *p > context.start_time + snap_eps);
                            if has_later_pulse {
                                pulse_times.retain(|p| (p - context.start_time).abs() > snap_eps);
                            }
                        }

                        // Keep pulses at the segment end. We sample end-exclusive, so we won't
                        // evaluate exactly at end_time, but an end pulse is still needed to shape
                        // the ramp leading up to the boundary (e.g. attack-only patterns).
                    }

                    for i in 0..t_steps {
                        // End-exclusive sampling to avoid sampling exactly at end_time.
                        let t = context.start_time + (i as f32 / t_steps.max(1) as f32) * duration;
                        let mut val = 0.0;

                        // Sum overlapping pulses
                        for &peak in &pulse_times {
                            // Optimization: skip if too far
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
                    let stems = crate::database::local::tracks::get_track_stems(pool, track_id)
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

                            root_caches.insert(
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

                // Re-do signal generation using `root_caches` if available
                if let Some(track_id) = audio_buffer.track_id {
                    if let Some(cache) = root_caches.get(&track_id) {
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
            "chroma_palette" => {
                let chroma_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "chroma"))
                    .ok_or_else(|| {
                        format!("Chroma Palette node '{}' missing chroma input", node.id)
                    })?;

                if let Some(chroma_sig) = signal_outputs
                    .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()))
                {
                    if chroma_sig.c != 12 {
                        eprintln!("[chroma_palette] Input signal is not 12-channel chroma");
                        continue;
                    }

                    // Define palettes (Simple Rainbow for now)
                    // C, C#, D, D#, E, F, F#, G, G#, A, A#, B
                    let rainbow: [[f32; 3]; 12] = [
                        [1.0, 0.0, 0.0], // C: Red
                        [1.0, 0.5, 0.0], // C#: Orange-Red
                        [1.0, 0.8, 0.0], // D: Orange
                        [1.0, 1.0, 0.0], // D#: Yellow
                        [0.5, 1.0, 0.0], // E: Lime
                        [0.0, 1.0, 0.0], // F: Green
                        [0.0, 1.0, 0.5], // F#: Mint
                        [0.0, 1.0, 1.0], // G: Cyan
                        [0.0, 0.5, 1.0], // G#: Azure
                        [0.0, 0.0, 1.0], // A: Blue
                        [0.5, 0.0, 1.0], // A#: Purple
                        [1.0, 0.0, 0.5], // B: Magenta
                    ];

                    let mut out_data = vec![0.0; chroma_sig.t * 3];

                    for t in 0..chroma_sig.t {
                        let mut r_sum = 0.0;
                        let mut g_sum = 0.0;
                        let mut b_sum = 0.0;

                        for c in 0..12 {
                            let prob = chroma_sig.data[t * 12 + c];
                            r_sum += prob * rainbow[c][0];
                            g_sum += prob * rainbow[c][1];
                            b_sum += prob * rainbow[c][2];
                        }

                        // Boost saturation slightly since averaging desaturates
                        let max_val = r_sum.max(g_sum).max(b_sum).max(0.001);
                        let scale = 1.0 / max_val; // Auto-gain

                        out_data[t * 3 + 0] = (r_sum * scale).clamp(0.0, 1.0);
                        out_data[t * 3 + 1] = (g_sum * scale).clamp(0.0, 1.0);
                        out_data[t * 3 + 2] = (b_sum * scale).clamp(0.0, 1.0);
                    }

                    signal_outputs.insert(
                        (node.id.clone(), "out".into()),
                        Signal {
                            n: 1,
                            t: chroma_sig.t,
                            c: 3,
                            data: out_data,
                        },
                    );
                }
            }
            "harmonic_tension" => {
                let chroma_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "chroma"))
                    .ok_or_else(|| {
                        format!("Harmonic Tension node '{}' missing chroma input", node.id)
                    })?;

                if let Some(chroma_sig) = signal_outputs
                    .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()))
                {
                    if chroma_sig.c != 12 {
                        continue;
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

                    signal_outputs.insert(
                        (node.id.clone(), "tension".into()),
                        Signal {
                            n: 1,
                            t: chroma_sig.t,
                            c: 1,
                            data: out_data,
                        },
                    );
                }
            }
            "spectral_shift" => {
                let in_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "in"))
                    .ok_or_else(|| {
                        format!("Spectral Shift node '{}' missing 'in' input", node.id)
                    })?;

                let chroma_edge = incoming_edges
                    .get(node.id.as_str())
                    .and_then(|edges| edges.iter().find(|edge| edge.to_port == "chroma"))
                    .ok_or_else(|| {
                        format!("Spectral Shift node '{}' missing chroma input", node.id)
                    })?;

                // Need both signals
                let in_sig_opt =
                    signal_outputs.get(&(in_edge.from_node.clone(), in_edge.from_port.clone()));
                let chroma_sig_opt = signal_outputs
                    .get(&(chroma_edge.from_node.clone(), chroma_edge.from_port.clone()));

                if let (Some(in_sig), Some(chroma_sig)) = (in_sig_opt, chroma_sig_opt) {
                    // Match lengths (simple resampling/clamping to min length)
                    let len = in_sig.t.min(chroma_sig.t);
                    let mut out_data = vec![0.0; len * 3];

                    for t in 0..len {
                        // 1. Get input RGB
                        let r = in_sig.data.get(t * in_sig.c + 0).copied().unwrap_or(0.0);
                        let g = in_sig.data.get(t * in_sig.c + 1).copied().unwrap_or(0.0);
                        let b = in_sig.data.get(t * in_sig.c + 2).copied().unwrap_or(0.0);

                        // 2. Determine shift amount from dominant chroma
                        let mut max_p = -1.0;
                        let mut dominant_idx = 0;
                        for c in 0..12 {
                            let p = chroma_sig.data[t * 12 + c];
                            if p > max_p {
                                max_p = p;
                                dominant_idx = c;
                            }
                        }
                        let hue_shift_deg = (dominant_idx as f32 / 12.0) * 360.0;

                        // 3. RGB -> HSL
                        let max_c = r.max(g).max(b);
                        let min_c = r.min(g).min(b);
                        let delta = max_c - min_c;

                        let l = (max_c + min_c) / 2.0;
                        let mut s = 0.0;
                        let mut h = 0.0;

                        if delta > 0.00001 {
                            s = if l > 0.5 {
                                delta / (2.0 - max_c - min_c)
                            } else {
                                delta / (max_c + min_c)
                            };

                            if max_c == r {
                                h = (g - b) / delta + (if g < b { 6.0 } else { 0.0 });
                            } else if max_c == g {
                                h = (b - r) / delta + 2.0;
                            } else {
                                h = (r - g) / delta + 4.0;
                            }
                            h /= 6.0; // 0..1
                        }

                        // 4. Apply Shift
                        h = (h + hue_shift_deg / 360.0).fract();
                        if h < 0.0 {
                            h += 1.0;
                        }

                        // 5. HSL -> RGB
                        let q = if l < 0.5 {
                            l * (1.0 + s)
                        } else {
                            l + s - l * s
                        };
                        let p = 2.0 * l - q;

                        fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
                            if t < 0.0 {
                                t += 1.0;
                            }
                            if t > 1.0 {
                                t -= 1.0;
                            }
                            if t < 1.0 / 6.0 {
                                return p + (q - p) * 6.0 * t;
                            }
                            if t < 1.0 / 2.0 {
                                return q;
                            }
                            if t < 2.0 / 3.0 {
                                return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
                            }
                            return p;
                        }

                        let r_out = hue_to_rgb(p, q, h + 1.0 / 3.0);
                        let g_out = hue_to_rgb(p, q, h);
                        let b_out = hue_to_rgb(p, q, h - 1.0 / 3.0);

                        out_data[t * 3 + 0] = r_out;
                        out_data[t * 3 + 1] = g_out;
                        out_data[t * 3 + 2] = b_out;
                    }

                    signal_outputs.insert(
                        (node.id.clone(), "out".into()),
                        Signal {
                            n: 1,
                            t: len,
                            c: 3,
                            data: out_data,
                        },
                    );
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
                        .cloned()
                        .as_ref()
                        .map(|grid| beat_grid_relative_to_crop(grid, audio_buffer.crop.as_ref()));

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
            "remap" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let input_edge = input_edges.iter().find(|e| e.to_port == "in");
                let Some(edge) = input_edge else { continue };
                let Some(signal) =
                    signal_outputs.get(&(edge.from_node.clone(), edge.from_port.clone()))
                else {
                    eprintln!(
                        "[run_graph] remap '{}' input signal unavailable; skipping",
                        node.id
                    );
                    continue;
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

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: signal.n,
                        t: signal.t,
                        c: 1,
                        data,
                    },
                );
            }
            "smooth_movement" => {
                let input_edges = incoming_edges
                    .get(node.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let pan_edge = input_edges.iter().find(|e| e.to_port == "pan_in");
                let tilt_edge = input_edges.iter().find(|e| e.to_port == "tilt_in");

                let pan = pan_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));
                let tilt = tilt_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

                if pan.is_none() && tilt.is_none() {
                    continue;
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
                            prev_pan +=
                                (target_pan - prev_pan).clamp(-max_pan_delta, max_pan_delta);
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

                signal_outputs.insert(
                    (node.id.clone(), "pan".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: pan_data,
                    },
                );
                signal_outputs.insert(
                    (node.id.clone(), "tilt".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: tilt_data,
                    },
                );
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
                    continue;
                };
                let Some(selection) =
                    selections.get(&(sel_e.from_node.clone(), sel_e.from_port.clone()))
                else {
                    continue;
                };

                let x_sig = x_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));
                let y_sig = y_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));
                let z_sig = z_edge
                    .and_then(|e| signal_outputs.get(&(e.from_node.clone(), e.from_port.clone())));

                if x_sig.is_none() && y_sig.is_none() && z_sig.is_none() {
                    continue;
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
                    let fixtures = crate::database::local::fixtures::get_all_fixtures(proj_pool)
                        .await
                        .map_err(|e| {
                            format!("LookAtPosition node failed to fetch fixtures: {}", e)
                        })?;

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
                            continue;
                        }

                        let Some(fixture_path) = fixture_path_by_id.get(&item.fixture_id).cloned()
                        else {
                            continue;
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

                        pan_tilt_max_by_fixture
                            .insert(item.fixture_id.clone(), (pan_max, tilt_max));
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

                signal_outputs.insert(
                    (node.id.clone(), "pan".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: pan_data,
                    },
                );
                signal_outputs.insert(
                    (node.id.clone(), "tilt".into()),
                    Signal {
                        n,
                        t: t_steps,
                        c: 1,
                        data: tilt_data,
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
                let a = parsed.get("a").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;

                signal_outputs.insert(
                    (node.id.clone(), "out".into()),
                    Signal {
                        n: 1,
                        t: 1,
                        c: 4,
                        data: vec![r, g, b, a],
                    },
                );

                // Keep string output for legacy view if needed, but port type is Signal now.
                color_outputs.insert((node.id.clone(), "out".into()), color_json.to_string());
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
                        speed: None,
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
                if prim.speed.is_some() {
                    entry.speed = prim.speed;
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
                println!(
                    "[run_graph #{run_id}] slowest_nodes: {}",
                    top_nodes.join(", ")
                );
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
            mel_specs,
            color_views,
            universe_state,
        },
        merged_layer,
    ))
}

fn adsr_durations(
    span_sec: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
) -> (f32, f32, f32, f32) {
    let a_w = attack.clamp(0.0, 1.0);
    let d_w = decay.clamp(0.0, 1.0);
    let s_w = sustain.clamp(0.0, 1.0);
    let r_w = release.clamp(0.0, 1.0);
    let weight_sum = a_w + d_w + s_w + r_w;

    if weight_sum < 1e-6 {
        return (0.0, 0.0, 0.0, 0.0);
    }

    let scale = span_sec / weight_sum;
    (a_w * scale, d_w * scale, s_w * scale, r_w * scale)
}

fn calc_envelope(
    t: f32,
    peak: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    sustain_level: f32,
    a_curve: f32,
    d_curve: f32,
) -> f32 {
    if t < peak - attack {
        return 0.0;
    }

    // Attack: ramp 0 -> 1
    if t <= peak {
        if attack <= 0.0 {
            return 1.0;
        }
        let x = (t - (peak - attack)) / attack;
        return shape_curve(x, a_curve);
    }

    let decay_end = peak + decay;
    // Decay: 1 -> sustain_level
    if t <= decay_end {
        if decay <= 0.0 {
            return sustain_level;
        }
        let x = (t - peak) / decay;
        let shaped = shape_curve(1.0 - x, d_curve);
        return sustain_level + (1.0 - sustain_level) * shaped;
    }

    let sustain_end = decay_end + sustain;
    // Sustain: hold sustain_level
    if t <= sustain_end {
        return sustain_level;
    }

    let release_end = sustain_end + release;
    // Release: sustain_level -> 0
    if t <= release_end {
        if release <= 0.0 {
            return 0.0;
        }
        let x = (t - sustain_end) / release;
        let shaped = shape_curve(1.0 - x, d_curve);
        return sustain_level * shaped;
    }

    0.0
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
    fn adsr_durations_span_fills_full_interval() {
        // Attack of 1.0 with no other phases should span the full interval.
        let (att, dec, sus, rel) = adsr_durations(2.0, 1.0, 0.0, 0.0, 0.0);
        assert!((att - 2.0).abs() < 1e-6);
        assert!(dec.abs() < 1e-6 && sus.abs() < 1e-6 && rel.abs() < 1e-6);
    }

    #[test]
    fn calc_envelope_peak_is_not_a_drop_to_zero() {
        // If we sample exactly at the peak and later phases are 0 duration,
        // we should not fall through to 0.0.
        let peak = 10.0;
        let attack = 2.0;
        let decay = 0.0;
        let sustain = 0.0;
        let release = 0.0;
        let sustain_level = 0.0;
        let a_curve = 0.0;
        let d_curve = 0.0;

        let just_before = calc_envelope(
            peak - 1e-3,
            peak,
            attack,
            decay,
            sustain,
            release,
            sustain_level,
            a_curve,
            d_curve,
        );
        let at_peak = calc_envelope(
            peak,
            peak,
            attack,
            decay,
            sustain,
            release,
            sustain_level,
            a_curve,
            d_curve,
        );

        assert!(just_before > 0.99, "just_before={just_before}");
        assert!((at_peak - 1.0).abs() < 1e-6, "at_peak={at_peak}");
    }

    fn run_with_context(graph: Graph, context: GraphContext) -> RunResult {
        tauri::async_runtime::block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:")
                .await
                .expect("in-memory db");
            let stem_cache = StemCache::new();
            let fft_service = crate::audio::FftService::new();
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
    fn beat_envelope_drops_start_pulse_for_attack_to_avoid_initial_peak_drop() {
        // When attack is non-zero, a pulse at exactly start_time creates a visible 1->0 drop
        // at the beginning of the segment. If another pulse exists later, we drop the start pulse
        // so the envelope ramps toward the next one.
        let beat_grid = BeatGrid {
            beats: vec![0.0, 1.0],
            downbeats: vec![0.0, 1.0],
            bpm: 60.0,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        };

        let mut params = std::collections::HashMap::new();
        params.insert("subdivision".into(), json!(1.0));
        params.insert("only_downbeats".into(), json!(0.0));
        params.insert("offset".into(), json!(0.0));
        params.insert("attack".into(), json!(1.0));
        params.insert("decay".into(), json!(0.0));
        params.insert("sustain".into(), json!(0.0));
        params.insert("release".into(), json!(0.0));
        params.insert("sustain_level".into(), json!(0.0));
        params.insert("attack_curve".into(), json!(0.0));
        params.insert("decay_curve".into(), json!(0.0));
        params.insert("amplitude".into(), json!(1.0));

        let graph = Graph {
            nodes: vec![
                NodeInstance {
                    id: "env".into(),
                    type_id: "beat_envelope".into(),
                    params,
                    position_x: None,
                    position_y: None,
                },
                NodeInstance {
                    id: "view".into(),
                    type_id: "view_signal".into(),
                    params: std::collections::HashMap::new(),
                    position_x: None,
                    position_y: None,
                },
            ],
            edges: vec![Edge {
                id: "e1".into(),
                from_node: "env".into(),
                from_port: "out".into(),
                to_node: "view".into(),
                to_port: "in".into(),
            }],
            args: vec![],
        };

        let result = run_with_context(
            graph,
            GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 1.0,
                beat_grid: Some(beat_grid),
                arg_values: None,
            },
        );

        let sig = result.views.get("view").expect("view signal exists");
        let first = sig.data.first().copied().unwrap_or(0.0);
        let last = sig.data.last().copied().unwrap_or(0.0);
        assert!(
            first.abs() < 1e-6,
            "expected first sample to start low (0.0), got {first}"
        );
        assert!(
            last > 0.9,
            "expected last sample to be near peak (1.0), got {last}"
        );
    }

    #[test]
    fn beat_envelope_does_not_spike_at_segment_end_for_decay_only() {
        // If a beat lands exactly at end_time, we sample end-exclusive and drop the end pulse
        // so the last sample doesn't jump back to 1.0.
        let beat_grid = BeatGrid {
            beats: vec![0.0, 1.0],
            downbeats: vec![0.0, 1.0],
            bpm: 60.0,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        };

        let mut params = std::collections::HashMap::new();
        params.insert("subdivision".into(), json!(1.0));
        params.insert("only_downbeats".into(), json!(0.0));
        params.insert("offset".into(), json!(0.0));
        params.insert("attack".into(), json!(0.0));
        params.insert("decay".into(), json!(1.0));
        params.insert("sustain".into(), json!(0.0));
        params.insert("release".into(), json!(0.0));
        params.insert("sustain_level".into(), json!(0.5));
        params.insert("attack_curve".into(), json!(0.0));
        params.insert("decay_curve".into(), json!(0.0));
        params.insert("amplitude".into(), json!(1.0));

        let graph = Graph {
            nodes: vec![
                NodeInstance {
                    id: "env".into(),
                    type_id: "beat_envelope".into(),
                    params,
                    position_x: None,
                    position_y: None,
                },
                NodeInstance {
                    id: "view".into(),
                    type_id: "view_signal".into(),
                    params: std::collections::HashMap::new(),
                    position_x: None,
                    position_y: None,
                },
            ],
            edges: vec![Edge {
                id: "e1".into(),
                from_node: "env".into(),
                from_port: "out".into(),
                to_node: "view".into(),
                to_port: "in".into(),
            }],
            args: vec![],
        };

        let result = run_with_context(
            graph,
            GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 1.0,
                beat_grid: Some(beat_grid),
                arg_values: None,
            },
        );

        let sig = result.views.get("view").expect("view signal exists");
        let last = sig.data.last().copied().unwrap_or(0.0);
        assert!(
            last < 0.75,
            "expected last sample to remain near sustain (0.5), got {last}"
        );
    }

    #[test]
    fn beat_envelope_attack_decay_does_not_flatline_at_segment_start() {
        // Regression: the "drop start pulse" fix should only apply to attack-only shapes.
        // For attack+decay, the start pulse is needed so the segment starts in decay, not 0.
        let beat_grid = BeatGrid {
            beats: vec![0.0, 1.0],
            downbeats: vec![0.0, 1.0],
            bpm: 60.0,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        };

        let mut params = std::collections::HashMap::new();
        params.insert("subdivision".into(), json!(1.0));
        params.insert("only_downbeats".into(), json!(0.0));
        params.insert("offset".into(), json!(0.0));
        params.insert("attack".into(), json!(0.5));
        params.insert("decay".into(), json!(0.5));
        params.insert("sustain".into(), json!(0.0));
        params.insert("release".into(), json!(0.0));
        params.insert("sustain_level".into(), json!(0.0));
        params.insert("attack_curve".into(), json!(0.0));
        params.insert("decay_curve".into(), json!(0.0));
        params.insert("amplitude".into(), json!(1.0));

        let graph = Graph {
            nodes: vec![
                NodeInstance {
                    id: "env".into(),
                    type_id: "beat_envelope".into(),
                    params,
                    position_x: None,
                    position_y: None,
                },
                NodeInstance {
                    id: "view".into(),
                    type_id: "view_signal".into(),
                    params: std::collections::HashMap::new(),
                    position_x: None,
                    position_y: None,
                },
            ],
            edges: vec![Edge {
                id: "e1".into(),
                from_node: "env".into(),
                from_port: "out".into(),
                to_node: "view".into(),
                to_port: "in".into(),
            }],
            args: vec![],
        };

        let result = run_with_context(
            graph,
            GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 1.0,
                beat_grid: Some(beat_grid),
                arg_values: None,
            },
        );

        let sig = result.views.get("view").expect("view signal exists");
        let first = sig.data.first().copied().unwrap_or(0.0);
        assert!(
            first > 0.9,
            "expected segment to start near peak (decay from start pulse), got {first}"
        );
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
