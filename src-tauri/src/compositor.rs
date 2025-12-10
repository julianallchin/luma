//! Track Compositor
//!
//! Composites multiple pattern layers on a track into a single LayerTimeSeries.
//! Creates a unified time-series that properly handles:
//! - Black (zero) output outside of any pattern's time range
//! - Pattern switching when annotations are sequential
//! - Z-index based override when patterns overlap

use once_cell::sync::Lazy;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Manager, State};

use crate::audio::{load_or_decode_audio, StemCache};
use crate::database::{Db, ProjectDb};
use crate::host_audio::HostAudioState;
use crate::models::annotations::TrackAnnotation;
use crate::models::schema::{
    BeatGrid, BlendMode, Graph, GraphContext, LayerTimeSeries, PrimitiveTimeSeries, Series,
    SeriesSample,
};
use crate::schema::{run_graph_internal, GraphExecutionConfig, SharedAudioContext};
use crate::tracks::TARGET_SAMPLE_RATE;

/// Sampling rate for the composite buffer (samples per second)
const COMPOSITE_SAMPLE_RATE: f32 = 60.0;

/// Apply blending between base and top values based on blend mode
fn blend_values(base: f32, top: f32, mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Replace => top,
        BlendMode::Add => (base + top).min(1.0),
        BlendMode::Multiply => base * top,
        BlendMode::Screen => 1.0 - (1.0 - base) * (1.0 - top),
        BlendMode::Max => base.max(top),
        BlendMode::Min => base.min(top),
        BlendMode::Lighten => base.max(top), // Same as Max for single values
        BlendMode::Value => {
            // Treat the value itself as its own opacity
            // If top is 1.0, it fully overrides. If 0.0, base shows through.
            // out = top * top + base * (1 - top)
            top * top + base * (1.0 - top)
        }
    }
}

/// Apply blending for color (RGB) values
fn blend_color(base: &[f32], top: &[f32], mode: BlendMode) -> Vec<f32> {
    // Expect base and top to be RGBA (4 floats)
    let base_r = base.get(0).copied().unwrap_or(0.0);
    let base_g = base.get(1).copied().unwrap_or(0.0);
    let base_b = base.get(2).copied().unwrap_or(0.0);
    let base_a = base.get(3).copied().unwrap_or(1.0);

    let top_r = top.get(0).copied().unwrap_or(0.0);
    let top_g = top.get(1).copied().unwrap_or(0.0);
    let top_b = top.get(2).copied().unwrap_or(0.0);
    let top_a = top.get(3).copied().unwrap_or(1.0);

    // 1. Calculate blended RGB (as if opaque)
    let (blended_r, blended_g, blended_b) = if matches!(mode, BlendMode::Value) {
        // Value Mode: Luminance acts as opacity for the BLEND, before final alpha composition
        // Luminance of top color
        let top_lum = 0.299 * top_r + 0.587 * top_g + 0.114 * top_b;

        // Mix top over base using top_lum as factor
        let r = top_r * top_lum + base_r * (1.0 - top_lum);
        let g = top_g * top_lum + base_g * (1.0 - top_lum);
        let b = top_b * top_lum + base_b * (1.0 - top_lum);
        (r, g, b)
    } else {
        (
            blend_values(base_r, top_r, mode),
            blend_values(base_g, top_g, mode),
            blend_values(base_b, top_b, mode),
        )
    };

    // 2. Alpha composite
    // Result = Source * SourceAlpha + Dest * (1 - SourceAlpha)
    // Where "Source" is the result of the BlendMode application
    let out_r = blended_r * top_a + base_r * (1.0 - top_a);
    let out_g = blended_g * top_a + base_g * (1.0 - top_a);
    let out_b = blended_b * top_a + base_b * (1.0 - top_a);
    let out_a = top_a + base_a * (1.0 - top_a);

    vec![out_r, out_g, out_b, out_a]
}

static COMPOSITION_CACHE: Lazy<Mutex<HashMap<i64, TrackCache>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
struct AnnotationLayer {
    start_time: f32,
    end_time: f32,
    z_index: i64,
    blend_mode: BlendMode,
    layer: LayerTimeSeries,
}

#[derive(Clone)]
struct CachedAnnotationLayer {
    signature: AnnotationSignature,
    layer: AnnotationLayer,
    graph_time_ms: f64,
}

#[derive(Clone, PartialEq, Eq)]
struct AnnotationSignature {
    pattern_id: i64,
    z_index: i64,
    start_time_bits: u64,
    end_time_bits: u64,
    blend_mode: BlendMode,
    graph_hash: u64,
    args_hash: u64,
}

#[derive(Default)]
struct TrackCache {
    shared_audio: Option<SharedAudioContext>,
    annotations: HashMap<i64, CachedAnnotationLayer>,
}

impl AnnotationSignature {
    fn new(annotation: &TrackAnnotation, graph_hash: u64) -> Self {
        // Hash the args JSON to detect changes in pattern arguments
        let args_str = annotation.args.to_string();
        let mut hasher = DefaultHasher::new();
        args_str.hash(&mut hasher);
        let args_hash = hasher.finish();

        Self {
            pattern_id: annotation.pattern_id,
            z_index: annotation.z_index,
            start_time_bits: annotation.start_time.to_bits(),
            end_time_bits: annotation.end_time.to_bits(),
            blend_mode: annotation.blend_mode,
            graph_hash,
            args_hash,
        }
    }
}

fn hash_graph_json(graph_json: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    graph_json.hash(&mut hasher);
    hasher.finish()
}

fn lookup_cached_layer(
    track_id: i64,
    annotation_id: i64,
    signature: &AnnotationSignature,
) -> Option<CachedAnnotationLayer> {
    let cache_guard = COMPOSITION_CACHE
        .lock()
        .expect("composition cache mutex poisoned");
    cache_guard
        .get(&track_id)
        .and_then(|track_cache| track_cache.annotations.get(&annotation_id))
        .filter(|entry| entry.signature == *signature)
        .cloned()
}

fn cache_layer(
    track_id: i64,
    annotation_id: i64,
    signature: AnnotationSignature,
    layer: AnnotationLayer,
    graph_time_ms: f64,
) {
    let mut cache_guard = COMPOSITION_CACHE
        .lock()
        .expect("composition cache mutex poisoned");
    let entry = cache_guard.entry(track_id).or_default();
    entry.annotations.insert(
        annotation_id,
        CachedAnnotationLayer {
            signature,
            layer,
            graph_time_ms,
        },
    );
}

fn prune_track_cache(track_id: i64, valid_ids: &HashSet<i64>) {
    let mut cache_guard = COMPOSITION_CACHE
        .lock()
        .expect("composition cache mutex poisoned");
    if let Some(track_cache) = cache_guard.get_mut(&track_id) {
        track_cache
            .annotations
            .retain(|id, _| valid_ids.contains(id));
    }
}

async fn fetch_track_path_and_hash(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> Result<(String, String), String> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to fetch track info: {}", e))?;

    row.ok_or_else(|| format!("Track {} not found", track_id))
}

async fn get_or_load_shared_audio(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    track_path: &str,
    track_hash: &str,
) -> Result<SharedAudioContext, String> {
    if let Some(cached) = COMPOSITION_CACHE
        .lock()
        .expect("composition cache mutex poisoned")
        .get(&track_id)
        .and_then(|t| t.shared_audio.clone())
    {
        if cached.track_hash == track_hash {
            return Ok(cached);
        }
    }

    let (samples, sample_rate) =
        load_or_decode_audio(Path::new(track_path), track_hash, TARGET_SAMPLE_RATE)
            .map_err(|e| format!("Failed to load audio for track {}: {}", track_id, e))?;

    if samples.is_empty() || sample_rate == 0 {
        return Err(format!(
            "Audio for track {} is empty or has zero sample rate",
            track_id
        ));
    }

    let shared = SharedAudioContext {
        track_id,
        track_hash: track_hash.to_string(),
        samples: Arc::new(samples),
        sample_rate,
    };

    {
        let mut cache_guard = COMPOSITION_CACHE
            .lock()
            .expect("composition cache mutex poisoned");
        let entry = cache_guard.entry(track_id).or_default();
        entry.shared_audio = Some(shared.clone());
    }

    Ok(shared)
}

/// Composite all patterns on a track into a single layer and push to host audio
#[tauri::command]
pub async fn composite_track(
    app: AppHandle,
    db: State<'_, Db>,
    project_db: State<'_, ProjectDb>,
    host_audio: State<'_, HostAudioState>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, crate::audio::FftService>,
    track_id: i64,
    skip_cache: Option<bool>,
) -> Result<(), String> {
    let skip_cache = skip_cache.unwrap_or(false);
    let compose_start = Instant::now();
    // 1. Fetch all annotations for the track (sorted by z_index)
    let annotations = fetch_annotations(&db.0, track_id).await?;
    let annotation_ids: HashSet<i64> = annotations.iter().map(|a| a.id).collect();

    if annotations.is_empty() {
        // No annotations - clear the active layer
        prune_track_cache(track_id, &annotation_ids);
        host_audio.set_active_layer(None);
        let total_ms = compose_start.elapsed().as_secs_f64() * 1000.0;
        println!(
            "[compositor] pass track={} annotations=0 reused=0 executed=0 avg_graph_ms=0.00 avg_layer_ms=0.00 composite_ms=0.00 total_ms={:.2} primitives=0",
            track_id, total_ms
        );
        return Ok(());
    }

    // 2. Get project pool for pattern graphs
    let project_pool = {
        let guard = project_db.0.lock().await;
        guard.clone()
    };
    let Some(project_pool) = project_pool else {
        return Err("No project currently open".into());
    };

    // 3. Load beat grid for the track
    let beat_grid = load_beat_grid(&db.0, track_id).await?;

    // 4. Get track duration
    let track_duration = get_track_duration(&db.0, track_id).await?.unwrap_or(300.0);

    // 5. Preload audio once for all graph executions on this track
    let (track_path, track_hash) = fetch_track_path_and_hash(&db.0, track_id).await?;
    let shared_audio = get_or_load_shared_audio(&db.0, track_id, &track_path, &track_hash).await?;

    // 5. Resolve resource path for fixtures
    let resource_path = app
        .path()
        .resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

    let final_path = if resource_path.exists() {
        Some(resource_path)
    } else {
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

    // 6. Execute each pattern and collect layers with their time ranges
    let mut annotation_layers: Vec<AnnotationLayer> = Vec::with_capacity(annotations.len());
    let mut computed_durations_ms: Vec<f64> = Vec::new();
    let mut layer_durations_ms: Vec<f64> = Vec::new();
    let mut reused_count = 0usize;
    let mut executed_count = 0usize;

    for annotation in &annotations {
        // Load pattern graph
        let graph_json = fetch_pattern_graph(&project_pool, annotation.pattern_id).await?;

        let graph_hash = hash_graph_json(&graph_json);
        let signature = AnnotationSignature::new(annotation, graph_hash);

        if !skip_cache {
            if let Some(cached) = lookup_cached_layer(track_id, annotation.id, &signature) {
                reused_count += 1;
                layer_durations_ms.push(cached.graph_time_ms);
                annotation_layers.push(cached.layer);
                continue;
            }
        }

        let graph: Graph = serde_json::from_str(&graph_json)
            .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;

        if graph.nodes.is_empty() {
            continue; // Skip empty graphs
        }

        // Create context for this annotation's time range
        let context = GraphContext {
            track_id,
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
        };

        // Execute the graph
        let run_start = Instant::now();
        let (_result, layer) = run_graph_internal(
            &db.0,
            Some(&project_pool),
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
        let graph_time_ms = run_start.elapsed().as_secs_f64() * 1000.0;

        if let Some(layer) = layer {
            let ann_layer = AnnotationLayer {
                start_time: annotation.start_time as f32,
                end_time: annotation.end_time as f32,
                z_index: annotation.z_index,
                blend_mode: annotation.blend_mode,
                layer,
            };
            executed_count += 1;
            computed_durations_ms.push(graph_time_ms);
            layer_durations_ms.push(graph_time_ms);

            cache_layer(
                track_id,
                annotation.id,
                signature,
                ann_layer.clone(),
                graph_time_ms,
            );
            annotation_layers.push(ann_layer);
        }
    }

    prune_track_cache(track_id, &annotation_ids);

    if annotation_layers.is_empty() {
        host_audio.set_active_layer(None);
        let total_ms = compose_start.elapsed().as_secs_f64() * 1000.0;
        println!(
            "[compositor] pass track={} annotations=0 reused={} executed={} avg_graph_ms=0.00 avg_layer_ms=0.00 composite_ms=0.00 total_ms={:.2} primitives=0",
            track_id, reused_count, executed_count, total_ms
        );
        return Ok(());
    }

    // 7. Create unified composite layer
    let composite_start = Instant::now();
    let composited = composite_layers_unified(annotation_layers, track_duration);
    let composite_ms = composite_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = compose_start.elapsed().as_secs_f64() * 1000.0;

    let avg_graph_ms = if computed_durations_ms.is_empty() {
        0.0
    } else {
        computed_durations_ms.iter().sum::<f64>() / computed_durations_ms.len() as f64
    };

    let avg_layer_ms = if layer_durations_ms.is_empty() {
        0.0
    } else {
        layer_durations_ms.iter().sum::<f64>() / layer_durations_ms.len() as f64
    };

    println!(
        "[compositor] pass track={} annotations={} reused={} executed={} avg_graph_ms={:.2} avg_layer_ms={:.2} composite_ms={:.2} total_ms={:.2} primitives={}",
        track_id,
        annotations.len(),
        reused_count,
        executed_count,
        avg_graph_ms,
        avg_layer_ms,
        composite_ms,
        total_ms,
        composited.primitives.len()
    );

    // 8. Push to host audio
    host_audio.set_active_layer(Some(composited));

    Ok(())
}

/// Fetch annotations for a track, sorted by z_index ascending
async fn fetch_annotations(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> Result<Vec<TrackAnnotation>, String> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        track_id: i64,
        pattern_id: i64,
        start_time: f64,
        end_time: f64,
        z_index: i64,
        blend_mode: String,
        args_json: Option<String>,
        created_at: String,
        updated_at: String,
    }

    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, track_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at
         FROM track_annotations
         WHERE track_id = ?
         ORDER BY z_index ASC",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to fetch annotations: {}", e))?;

    Ok(rows
        .into_iter()
        .map(|r| {
            // Parse blend_mode from string, default to Replace if invalid
            let blend_mode = serde_json::from_str::<BlendMode>(&format!("\"{}\"", r.blend_mode))
                .unwrap_or(BlendMode::Replace);

            TrackAnnotation {
                id: r.id,
                track_id: r.track_id,
                pattern_id: r.pattern_id,
                start_time: r.start_time,
                end_time: r.end_time,
                z_index: r.z_index,
                blend_mode,
                args: r
                    .args_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or_else(|| serde_json::json!({})),
                created_at: r.created_at,
                updated_at: r.updated_at,
            }
        })
        .collect())
}

/// Load beat grid for a track
async fn load_beat_grid(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> Result<Option<BeatGrid>, String> {
    let row = sqlx::query_as::<_, (String, String, Option<f64>, Option<f64>, Option<i64>)>(
        "SELECT beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar
         FROM track_beats WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load beat grid: {}", e))?;

    match row {
        Some((beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)) => {
            let beats: Vec<f32> = serde_json::from_str(&beats_json)
                .map_err(|e| format!("Failed to parse beats: {}", e))?;
            let downbeats: Vec<f32> = serde_json::from_str(&downbeats_json)
                .map_err(|e| format!("Failed to parse downbeats: {}", e))?;
            let (fallback_bpm, fallback_offset, fallback_bpb) =
                crate::tracks::infer_grid_metadata(&beats, &downbeats);

            Ok(Some(BeatGrid {
                beats,
                downbeats,
                bpm: bpm.unwrap_or(fallback_bpm as f64) as f32,
                downbeat_offset: downbeat_offset.unwrap_or(fallback_offset as f64) as f32,
                beats_per_bar: beats_per_bar.unwrap_or(fallback_bpb as i64) as i32,
            }))
        }
        None => Ok(None),
    }
}

/// Get track duration in seconds
async fn get_track_duration(pool: &sqlx::SqlitePool, track_id: i64) -> Result<Option<f32>, String> {
    let row: Option<(Option<f64>,)> =
        sqlx::query_as("SELECT duration_seconds FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to get track duration: {}", e))?;

    Ok(row.and_then(|(d,)| d.map(|v| v as f32)))
}

/// Fetch pattern graph JSON from project DB
async fn fetch_pattern_graph(pool: &sqlx::SqlitePool, pattern_id: i64) -> Result<String, String> {
    let result: Option<(String,)> =
        sqlx::query_as("SELECT graph_json FROM implementations WHERE pattern_id = ?")
            .bind(pattern_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to fetch pattern graph: {}", e))?;

    match result {
        Some((json,)) => Ok(json),
        None => Ok("{\"nodes\":[],\"edges\":[],\"args\":[]}".to_string()),
    }
}

/// Sample a Series at a specific time. Optionally interpolate between points.
fn sample_series(series: &Series, time: f32, interpolate: bool) -> Option<Vec<f32>> {
    if series.samples.is_empty() {
        return None;
    }

    // Find surrounding samples
    let mut prev: Option<&SeriesSample> = None;
    let mut next: Option<&SeriesSample> = None;

    for sample in &series.samples {
        if sample.time <= time {
            prev = Some(sample);
        }
        if sample.time >= time && next.is_none() {
            next = Some(sample);
        }
    }

    match (prev, next) {
        (Some(p), Some(n)) if interpolate && (p.time - n.time).abs() > 0.0001 => {
            // Interpolate between prev and next
            let t = (time - p.time) / (n.time - p.time);
            let t = t.clamp(0.0, 1.0);
            let values: Vec<f32> = p
                .values
                .iter()
                .zip(n.values.iter())
                .map(|(a, b)| a + (b - a) * t)
                .collect();
            Some(values)
        }
        (Some(p), _) => Some(p.values.clone()),
        (_, Some(n)) => Some(n.values.clone()),
        _ => None,
    }
}

/// Create a unified composite layer that covers the entire track duration
///
/// For each time sample:
/// 1. Find all annotations that contain this time
/// 2. Sort by z-index (lowest to highest)
/// 3. Apply values from each layer in order (Painter's Algorithm)
/// 4. If a layer defines a value, it overrides the previous value.
/// 5. If no layer defines a value, it remains at default (0/black).
fn composite_layers_unified(
    mut layers: Vec<AnnotationLayer>,
    track_duration: f32,
) -> LayerTimeSeries {
    // Sort by z-index ascending (Painter's Algorithm: draw bottom up)
    layers.sort_by_key(|l| l.z_index);

    // Collect all unique primitive IDs across all layers
    let all_primitive_ids: HashSet<String> = layers
        .iter()
        .flat_map(|l| l.layer.primitives.iter().map(|p| p.primitive_id.clone()))
        .collect();

    // Calculate sample points
    let num_samples = (track_duration * COMPOSITE_SAMPLE_RATE).ceil() as usize;
    let num_samples = num_samples.max(2); // At least 2 samples

    // Build composite for each primitive
    let mut composited_primitives: Vec<PrimitiveTimeSeries> = Vec::new();

    for primitive_id in all_primitive_ids {
        let mut dimmer_samples: Vec<SeriesSample> = Vec::with_capacity(num_samples);
        let mut color_samples: Vec<SeriesSample> = Vec::with_capacity(num_samples);
        let mut strobe_samples: Vec<SeriesSample> = Vec::with_capacity(num_samples);

        for i in 0..num_samples {
            let time = (i as f32 / (num_samples - 1) as f32) * track_duration;

            // Default values (Transparent Black/Zero)
            let mut current_dimmer = 0.0;
            let mut current_color = vec![0.0, 0.0, 0.0, 0.0];
            let mut current_strobe = 0.0;
            // Track inherited color from color-only layers (no dimmer)
            // Dimmer-only layers above can "reveal" this color
            let mut available_color: Option<Vec<f32>> = None;

            // Iterate all layers from bottom (lowest Z) to top (highest Z)
            for layer in &layers {
                // Check if this layer is active at this time
                if time >= layer.start_time && time < layer.end_time {
                    // Find this primitive in the layer
                    if let Some(prim) = layer
                        .layer
                        .primitives
                        .iter()
                        .find(|p| p.primitive_id == primitive_id)
                    {
                        // Track this layer's dimmer at the current time so we can gate color by it
                        let mut layer_dimmer_sample: Option<f32> = None;

                        // If layer defines dimmer, blend it
                        if let Some(s) = &prim.dimmer {
                            if let Some(vals) = sample_series(s, time, true) {
                                if let Some(v) = vals.first() {
                                    layer_dimmer_sample = Some(*v);
                                    current_dimmer =
                                        blend_values(current_dimmer, *v, layer.blend_mode);
                                }
                            }
                        }

                        // Resolve this layer's color: own definition or inherited from below
                        let layer_color: Option<Vec<f32>> =
                            if let Some(s) = &prim.color {
                                sample_series(s, time, true)
                                    .filter(|v| v.len() >= 3)
                                    .map(|v| {
                                        if v.len() >= 4 {
                                            v
                                        } else {
                                            vec![v[0], v[1], v[2], 1.0]
                                        }
                                    })
                            } else {
                                available_color.clone()
                            };

                        // Dimmer acts as alpha: defaults to 0 (invisible) if not defined
                        let layer_alpha = layer_dimmer_sample.unwrap_or(0.0);

                        // Blend: color × alpha
                        if let Some(ref color) = layer_color {
                            let premultiplied = vec![
                                color[0] * layer_alpha,
                                color[1] * layer_alpha,
                                color[2] * layer_alpha,
                                color[3] * layer_alpha,
                            ];
                            current_color =
                                blend_color(&current_color, &premultiplied, layer.blend_mode);
                        }

                        // Update inherited color for layers above
                        if prim.color.is_some() {
                            available_color = layer_color;
                        }

                        // If layer defines strobe, blend it
                        if let Some(s) = &prim.strobe {
                            // Strobe values are discrete; hold the last sample rather than interpolate
                            if let Some(vals) = sample_series(s, time, false) {
                                if let Some(v) = vals.first() {
                                    current_strobe =
                                        blend_values(current_strobe, *v, layer.blend_mode);
                                }
                            }
                        }
                    }
                }
            }

            dimmer_samples.push(SeriesSample {
                time,
                values: vec![current_dimmer],
                label: None,
            });

            // Strip alpha for final output (DMX uses RGB)
            let rgb_color = if current_color.len() >= 3 {
                current_color[0..3].to_vec()
            } else {
                current_color
            };

            color_samples.push(SeriesSample {
                time,
                values: rgb_color,
                label: None,
            });

            strobe_samples.push(SeriesSample {
                time,
                values: vec![current_strobe],
                label: None,
            });
        }

        composited_primitives.push(PrimitiveTimeSeries {
            primitive_id,
            dimmer: Some(Series {
                dim: 1,
                labels: None,
                samples: dimmer_samples,
            }),
            color: Some(Series {
                dim: 3,
                labels: None,
                samples: color_samples,
            }),
            position: None,
            strobe: Some(Series {
                dim: 1,
                labels: None,
                samples: strobe_samples,
            }),
        });
    }

    LayerTimeSeries {
        primitives: composited_primitives,
    }
}
