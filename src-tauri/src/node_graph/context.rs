use crate::audio::load_or_decode_audio;
use crate::models::node_graph::{AudioCrop, BeatGrid, GraphContext, NodeInstance};
use crate::services::tracks::{self, TARGET_SAMPLE_RATE};
use sqlx::SqlitePool;
use std::path::Path;

#[derive(Clone)]
pub struct AudioBuffer {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub crop: Option<AudioCrop>,
    pub track_id: Option<i64>,
    pub track_hash: Option<String>,
}

#[derive(Clone)]
pub struct LoadedContext {
    pub audio_buffer: Option<AudioBuffer>,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub duration: f32,
    pub beat_grid: Option<BeatGrid>,
    pub track_hash: Option<String>,
    pub load_ms: f64,
}

/// Whether any node requires host-provided context audio/beat data.
pub fn needs_context(nodes: &[NodeInstance]) -> bool {
    nodes.iter().any(|n| {
        matches!(
            n.type_id.as_str(),
            "audio_input"
                | "beat_clock"
                | "stem_splitter"
                | "harmony_analysis"
                | "lowpass_filter"
                | "highpass_filter"
        )
    })
}

/// Parse a color object value into normalized RGBA tuple.
pub fn parse_color_value(value: &serde_json::Value) -> (f32, f32, f32, f32) {
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
}

/// Parse hex color string into normalized RGBA tuple.
pub fn parse_hex_color(hex: &str) -> (f32, f32, f32, f32) {
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
}

/// Crop samples to requested range and length.
pub fn crop_samples_to_range(
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

/// Shift beat grid relative to an audio crop window.
pub fn beat_grid_relative_to_crop(grid: &BeatGrid, crop: Option<&AudioCrop>) -> BeatGrid {
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

pub async fn load_context(
    pool: &SqlitePool,
    graph_context: &GraphContext,
    config_shared_audio: Option<&crate::node_graph::SharedAudioContext>,
    nodes: &[NodeInstance],
) -> Result<LoadedContext, String> {
    let needs_ctx = needs_context(nodes);
    let context_load_start = std::time::Instant::now();

    if !needs_ctx {
        return Ok(LoadedContext {
            audio_buffer: None,
            samples: Vec::new(),
            sample_rate: 0,
            duration: 0.0,
            beat_grid: None,
            track_hash: None,
            load_ms: 0.0,
        });
    }

    let info = crate::database::local::tracks::get_track_path_and_hash(pool, graph_context.track_id)
        .await
        .map_err(|e| format!("Failed to fetch track path: {}", e))?;
    let context_file_path = info.file_path;
    let track_hash = info.track_hash;

    let (context_full_samples, sample_rate, track_hash): (Vec<f32>, u32, String) =
        if let Some(shared) = config_shared_audio {
            if shared.track_id != graph_context.track_id {
                return Err(format!(
                    "Shared audio provided for track {} but context track is {}",
                    shared.track_id, graph_context.track_id
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

    let ctx_start_sample = (graph_context.start_time * sample_rate as f32)
        .floor()
        .max(0.0) as usize;
    let ctx_end_sample = if graph_context.end_time > 0.0 {
        (graph_context.end_time * sample_rate as f32).ceil() as usize
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
            start_seconds: graph_context.start_time,
            end_seconds: graph_context
                .end_time
                .max(graph_context.start_time + duration),
        }),
        track_id: Some(graph_context.track_id),
        track_hash: Some(track_hash.clone()),
    };

    let beat_grid: Option<BeatGrid> = if let Some(grid) = graph_context.beat_grid.clone() {
        Some(grid)
    } else {
        tracks::get_track_beats(pool, graph_context.track_id)
            .await
            .map_err(|e| format!("Failed to load beat data: {}", e))?
    };

    Ok(LoadedContext {
        audio_buffer: Some(audio_buffer),
        samples,
        sample_rate,
        duration,
        beat_grid,
        track_hash: Some(track_hash),
        load_ms: context_load_start.elapsed().as_secs_f64() * 1000.0,
    })
}
