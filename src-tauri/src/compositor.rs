//! Track Compositor
//!
//! Composites multiple pattern layers on a track into a single LayerTimeSeries.
//! Creates a unified time-series that properly handles:
//! - Black (zero) output outside of any pattern's time range
//! - Pattern switching when annotations are sequential
//! - Z-index based override when patterns overlap

use std::collections::HashSet;
use std::path::PathBuf;
use tauri::{AppHandle, Manager, State};

use crate::database::{Db, ProjectDb};
use crate::host_audio::HostAudioState;
use crate::models::annotations::TrackAnnotation;
use crate::models::schema::{
    BeatGrid, Graph, GraphContext, LayerTimeSeries, PrimitiveTimeSeries, Series, SeriesSample,
};
use crate::schema::run_graph_internal;

/// Sampling rate for the composite buffer (samples per second)
const COMPOSITE_SAMPLE_RATE: f32 = 60.0;

/// Annotation with its executed layer
struct AnnotationLayer {
    start_time: f32,
    end_time: f32,
    z_index: i64,
    layer: LayerTimeSeries,
}

/// Composite all patterns on a track into a single layer and push to host audio
#[tauri::command]
pub async fn composite_track(
    app: AppHandle,
    db: State<'_, Db>,
    project_db: State<'_, ProjectDb>,
    host_audio: State<'_, HostAudioState>,
    track_id: i64,
) -> Result<(), String> {
    // 1. Fetch all annotations for the track (sorted by z_index)
    let annotations = fetch_annotations(&db.0, track_id).await?;

    if annotations.is_empty() {
        // No annotations - clear the active layer
        host_audio.set_active_layer(None);
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
    let mut annotation_layers: Vec<AnnotationLayer> = Vec::new();

    for annotation in &annotations {
        // Load pattern graph
        let graph_json = fetch_pattern_graph(&project_pool, annotation.pattern_id).await?;
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
        let (_result, layer) = run_graph_internal(
            &db.0,
            Some(&project_pool),
            final_path.clone(),
            graph,
            context,
            false,
        )
        .await?;

        if let Some(layer) = layer {
            annotation_layers.push(AnnotationLayer {
                start_time: annotation.start_time as f32,
                end_time: annotation.end_time as f32,
                z_index: annotation.z_index,
                layer,
            });
        }
    }

    if annotation_layers.is_empty() {
        host_audio.set_active_layer(None);
        return Ok(());
    }

    // 7. Create unified composite layer
    let composited = composite_layers_unified(annotation_layers, track_duration);

    println!(
        "[compositor] Composited {} primitives for track duration {:.1}s",
        composited.primitives.len(),
        track_duration
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
/// 2. Pick the one with highest z-index
/// 3. Sample that annotation's layer at this time
/// 4. If no annotation covers this time, output black (zero)
fn composite_layers_unified(
    mut layers: Vec<AnnotationLayer>,
    track_duration: f32,
) -> LayerTimeSeries {
    // Sort by z-index ascending (so we can pick highest by iterating in reverse)
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

            // Find the active layer at this time (highest z-index wins)
            let active_layer = layers
                .iter()
                .rev() // Reverse to get highest z-index first
                .find(|l| time >= l.start_time && time < l.end_time);

            // Sample from active layer or use black
            let (dimmer_val, color_val, strobe_val) = if let Some(layer) = active_layer {
                // Find this primitive in the layer
                let prim = layer
                    .layer
                    .primitives
                    .iter()
                    .find(|p| p.primitive_id == primitive_id);

                if let Some(prim) = prim {
                    let dimmer = prim
                        .dimmer
                        .as_ref()
                        .and_then(|s| sample_series(s, time))
                        .map(|v| v.first().copied().unwrap_or(0.0))
                        .unwrap_or(0.0);

                    let color = prim
                        .color
                        .as_ref()
                        .and_then(|s| sample_series(s, time))
                        .unwrap_or_else(|| vec![1.0, 1.0, 1.0]); // White default

                    let strobe = prim
                        .strobe
                        .as_ref()
                        .and_then(|s| sample_series(s, time))
                        .map(|v| v.first().copied().unwrap_or(0.0))
                        .unwrap_or(0.0);

                    (dimmer, color, strobe)
                } else {
                    // Primitive not in this layer - black
                    (0.0, vec![0.0, 0.0, 0.0], 0.0)
                }
            } else {
                // No active layer at this time - black
                (0.0, vec![0.0, 0.0, 0.0], 0.0)
            };

            dimmer_samples.push(SeriesSample {
                time,
                values: vec![dimmer_val],
                label: None,
            });

            color_samples.push(SeriesSample {
                time,
                values: color_val,
                label: None,
            });

            strobe_samples.push(SeriesSample {
                time,
                values: vec![strobe_val],
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
