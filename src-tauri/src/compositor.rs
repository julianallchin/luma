//! Track Compositor
//!
//! Composites multiple pattern layers on a track into a single LayerTimeSeries.
//! Creates a unified time-series that properly handles:
//! - Black (zero) output outside of any pattern's time range
//! - Pattern switching when annotations are sequential
//! - Z-index based override when patterns overlap

use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::collections::hash_map::DefaultHasher;
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
    BeatGrid, Graph, GraphContext, LayerTimeSeries, PrimitiveTimeSeries, Series, SeriesSample,
};
use crate::schema::{run_graph_internal, GraphExecutionConfig, SharedAudioContext};
use crate::tracks::TARGET_SAMPLE_RATE;

/// Sampling rate for the composite buffer (samples per second)
const COMPOSITE_SAMPLE_RATE: f32 = 60.0;

static COMPOSITION_CACHE: Lazy<Mutex<HashMap<i64, TrackCache>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
struct AnnotationLayer {
    start_time: f32,
    end_time: f32,
    z_index: i64,
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
    graph_hash: u64,
}

#[derive(Default)]
struct TrackCache {
    shared_audio: Option<SharedAudioContext>,
    annotations: HashMap<i64, CachedAnnotationLayer>,
}

impl AnnotationSignature {
    fn new(annotation: &TrackAnnotation, graph_hash: u64) -> Self {
        Self {
            pattern_id: annotation.pattern_id,
            z_index: annotation.z_index,
            start_time_bits: annotation.start_time.to_bits(),
            end_time_bits: annotation.end_time.to_bits(),
            graph_hash,
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
) -> Result<(), String> {
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
    let shared_audio =
        get_or_load_shared_audio(&db.0, track_id, &track_path, &track_hash).await?;

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

        if let Some(cached) = lookup_cached_layer(track_id, annotation.id, &signature) {
            reused_count += 1;
            layer_durations_ms.push(cached.graph_time_ms);
            annotation_layers.push(cached.layer);
            continue;
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
        created_at: String,
        updated_at: String,
    }

    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, track_id, pattern_id, start_time, end_time, z_index, created_at, updated_at
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
        .map(|r| TrackAnnotation {
            id: r.id,
            track_id: r.track_id,
            pattern_id: r.pattern_id,
            start_time: r.start_time,
            end_time: r.end_time,
            z_index: r.z_index,
            created_at: r.created_at,
            updated_at: r.updated_at,
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
        None => Ok("{\"nodes\":[],\"edges\":[]}".to_string()),
    }
}

/// Sample a Series at a specific time using linear interpolation
fn sample_series(series: &Series, time: f32) -> Option<Vec<f32>> {
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
        (Some(p), Some(n)) if (p.time - n.time).abs() > 0.0001 => {
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

            // Default values (Black/Zero)
            let mut current_dimmer = 0.0;
            let mut current_color = vec![0.0, 0.0, 0.0];
            let mut current_strobe = 0.0;

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
                        // If layer defines dimmer, override
                        if let Some(s) = &prim.dimmer {
                            if let Some(vals) = sample_series(s, time) {
                                if let Some(v) = vals.first() {
                                    current_dimmer = *v;
                                }
                            }
                        }

                        // If layer defines color, override
                        if let Some(s) = &prim.color {
                            if let Some(vals) = sample_series(s, time) {
                                if vals.len() >= 3 {
                                    current_color = vals;
                                }
                            }
                        }

                        // If layer defines strobe, override
                        if let Some(s) = &prim.strobe {
                            if let Some(vals) = sample_series(s, time) {
                                if let Some(v) = vals.first() {
                                    current_strobe = *v;
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

            color_samples.push(SeriesSample {
                time,
                values: current_color,
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
