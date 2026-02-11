//! Annotation Preview Generator
//!
//! Generates space-time heatmap thumbnails for timeline annotations.
//! Each preview is a small RGBA image where rows = fixtures, columns = time steps,
//! and pixel color = fixture RGB × dimmer.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, State};

use crate::audio::{FftService, StemCache};
use crate::compositor::{
    fetch_pattern_graph, fetch_scores, fetch_track_path_and_hash, get_or_load_shared_audio,
    hash_graph_json, load_beat_grid, sample_series, AnnotationSignature,
};
use crate::database::Db;
use crate::models::node_graph::{BeatGrid, Graph, GraphContext};
use crate::models::patterns::AnnotationPreview;
use crate::node_graph::{run_graph_internal, GraphExecutionConfig};

/// Columns per beat in the preview thumbnail
const STEPS_PER_BEAT: u32 = 16;
const MIN_PREVIEW_WIDTH: u32 = 8;
const MAX_PREVIEW_WIDTH: u32 = 512;
const MAX_PREVIEW_HEIGHT: u32 = 32;

struct CachedPreview {
    signature: AnnotationSignature,
    preview: AnnotationPreview,
}

static PREVIEW_CACHE: Lazy<Mutex<HashMap<i64, CachedPreview>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[tauri::command]
pub async fn generate_annotation_previews(
    app: AppHandle,
    db: State<'_, Db>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    track_id: i64,
    venue_id: i64,
) -> Result<Vec<AnnotationPreview>, String> {
    let gen_start = Instant::now();

    // 1. Fetch annotations
    let annotations = fetch_scores(&db.0, track_id).await?;
    if annotations.is_empty() {
        return Ok(vec![]);
    }

    // 2. Load beat grid
    let beat_grid = load_beat_grid(&db.0, track_id).await?;

    // 3. Load shared audio
    let (track_path, track_hash) = fetch_track_path_and_hash(&db.0, track_id).await?;
    let shared_audio = get_or_load_shared_audio(track_id, &track_path, &track_hash).await?;

    // 4. Resolve fixture path
    let final_path = crate::services::fixtures::resolve_fixtures_root(&app).ok();

    // 5. Process each annotation
    let mut previews = Vec::with_capacity(annotations.len());
    let mut cache_hits = 0usize;
    let mut generated = 0usize;

    for annotation in &annotations {
        let graph_json = fetch_pattern_graph(&db.0, annotation.pattern_id).await?;
        let graph_hash = hash_graph_json(&graph_json);
        let signature = AnnotationSignature::new(annotation, graph_hash, 0);

        // Check cache
        {
            let cache = PREVIEW_CACHE.lock().expect("preview cache mutex poisoned");
            if let Some(cached) = cache.get(&annotation.id) {
                if cached.signature.matches_ignoring_seed(&signature) {
                    previews.push(cached.preview.clone());
                    cache_hits += 1;
                    continue;
                }
            }
        }

        // Parse and execute graph
        let graph: Graph = serde_json::from_str(&graph_json)
            .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;

        if graph.nodes.is_empty() {
            let preview = AnnotationPreview {
                annotation_id: annotation.id,
                width: 1,
                height: 1,
                pixels: vec![0, 0, 0, 0],
                dominant_color: [0.0; 3],
            };
            previews.push(preview);
            continue;
        }

        let instance_seed = rand::random::<u64>();
        let context = GraphContext {
            track_id,
            venue_id,
            start_time: annotation.start_time as f32,
            end_time: annotation.end_time as f32,
            beat_grid: beat_grid.clone(),
            arg_values: Some(
                annotation
                    .args
                    .as_object()
                    .cloned()
                    .unwrap_or_else(|| serde_json::Map::new())
                    .into_iter()
                    .collect(),
            ),
            instance_seed: Some(instance_seed),
        };

        let (_result, layer) = run_graph_internal(
            &db.0,
            Some(&db.0),
            &stem_cache,
            &fft_service,
            final_path.clone(),
            graph,
            context,
            GraphExecutionConfig {
                compute_visualizations: false,
                log_summary: false,
                log_primitives: false,
                shared_audio: Some(shared_audio.clone()),
            },
        )
        .await?;

        let preview = if let Some(ref layer) = layer {
            render_preview(
                annotation.id,
                layer,
                annotation.start_time as f32,
                annotation.end_time as f32,
                beat_grid.as_ref(),
            )
        } else {
            AnnotationPreview {
                annotation_id: annotation.id,
                width: 1,
                height: 1,
                pixels: vec![0, 0, 0, 0],
                dominant_color: [0.0; 3],
            }
        };

        // Store in cache
        {
            let mut cache = PREVIEW_CACHE.lock().expect("preview cache mutex poisoned");
            cache.insert(
                annotation.id,
                CachedPreview {
                    signature,
                    preview: preview.clone(),
                },
            );
        }

        generated += 1;
        previews.push(preview);
    }

    let total_ms = gen_start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "[annotation_preview] track={} annotations={} cache_hits={} generated={} total_ms={:.2}",
        track_id,
        annotations.len(),
        cache_hits,
        generated,
        total_ms
    );

    Ok(previews)
}

#[tauri::command]
pub fn invalidate_annotation_previews() {
    PREVIEW_CACHE
        .lock()
        .expect("preview cache mutex poisoned")
        .clear();
}

/// Compute preview width from beat grid: count beats in [start, end), multiply by STEPS_PER_BEAT.
/// Falls back to duration-based estimate if no beat grid.
fn compute_preview_width(beat_grid: Option<&BeatGrid>, start_time: f32, end_time: f32) -> u32 {
    let duration = end_time - start_time;
    let beat_count = if let Some(bg) = beat_grid {
        // Count beats that fall within the annotation range
        let count = bg
            .beats
            .iter()
            .filter(|&&b| b >= start_time && b < end_time)
            .count() as u32;
        if count > 0 {
            count
        } else {
            // No beats in range — estimate from BPM
            let bps = bg.bpm / 60.0;
            (duration * bps).round().max(1.0) as u32
        }
    } else {
        // No beat grid — assume 120 BPM
        (duration * 2.0).round().max(1.0) as u32
    };

    (beat_count * STEPS_PER_BEAT).clamp(MIN_PREVIEW_WIDTH, MAX_PREVIEW_WIDTH)
}

fn render_preview(
    annotation_id: i64,
    layer: &crate::models::node_graph::LayerTimeSeries,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<&BeatGrid>,
) -> AnnotationPreview {
    let primitives = &layer.primitives;
    if primitives.is_empty() {
        return AnnotationPreview {
            annotation_id,
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 0],
            dominant_color: [0.0; 3],
        };
    }

    // Sort primitives by brightness-weighted center of mass in time.
    // This orders rows so that spatial patterns (chases, sweeps) appear as
    // clear diagonals in the thumbnail regardless of fixture IDs.
    let width = compute_preview_width(beat_grid, start_time, end_time);
    let time_span = end_time - start_time;
    let width_divisor = (width - 1).max(1) as f32;

    let mut prims_with_com: Vec<_> = primitives
        .iter()
        .map(|prim| {
            let mut weighted_time = 0.0f64;
            let mut total_brightness = 0.0f64;

            // Sample brightness at each time column
            for col in 0..width {
                let t = start_time + (col as f32 / width_divisor) * time_span;
                let dimmer = prim
                    .dimmer
                    .as_ref()
                    .and_then(|s| sample_series(s, t, true))
                    .and_then(|v| v.first().copied())
                    .unwrap_or(0.0) as f64;

                weighted_time += col as f64 * dimmer;
                total_brightness += dimmer;
            }

            let com = if total_brightness > 0.0 {
                weighted_time / total_brightness
            } else {
                f64::MAX // dark primitives sort to the end
            };
            (prim, com)
        })
        .collect();

    prims_with_com.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let sorted_prims: Vec<_> = prims_with_com.into_iter().map(|(p, _)| p).collect();

    let height = (sorted_prims.len() as u32).min(MAX_PREVIEW_HEIGHT);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let mut color_sum = [0.0f64; 3];
    let mut weight_sum = 0.0f64;

    for (row, prim) in sorted_prims.iter().take(height as usize).enumerate() {
        for col in 0..width {
            let t = start_time + (col as f32 / width_divisor) * time_span;

            // Sample color
            let color = prim
                .color
                .as_ref()
                .and_then(|s| sample_series(s, t, true))
                .unwrap_or_else(|| vec![1.0, 1.0, 1.0]);

            // Sample dimmer
            let dimmer = prim
                .dimmer
                .as_ref()
                .and_then(|s| sample_series(s, t, true))
                .and_then(|v| v.first().copied())
                .unwrap_or(0.0);

            let r = (color.get(0).copied().unwrap_or(1.0) * dimmer * 255.0).clamp(0.0, 255.0) as u8;
            let g = (color.get(1).copied().unwrap_or(1.0) * dimmer * 255.0).clamp(0.0, 255.0) as u8;
            let b = (color.get(2).copied().unwrap_or(1.0) * dimmer * 255.0).clamp(0.0, 255.0) as u8;

            let idx = ((row as u32 * width + col) * 4) as usize;
            pixels[idx] = r;
            pixels[idx + 1] = g;
            pixels[idx + 2] = b;
            pixels[idx + 3] = 255;

            color_sum[0] += r as f64;
            color_sum[1] += g as f64;
            color_sum[2] += b as f64;
            weight_sum += 1.0;
        }
    }

    let dominant_color = if weight_sum > 0.0 {
        [
            (color_sum[0] / weight_sum / 255.0) as f32,
            (color_sum[1] / weight_sum / 255.0) as f32,
            (color_sum[2] / weight_sum / 255.0) as f32,
        ]
    } else {
        [0.0; 3]
    };

    AnnotationPreview {
        annotation_id,
        width,
        height,
        pixels,
        dominant_color,
    }
}
