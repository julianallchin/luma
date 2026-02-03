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

use crate::audio::{load_or_decode_audio, stereo_to_mono, StemCache};
use crate::database::Db;
use crate::host_audio::HostAudioState;
use crate::models::node_graph::{
    BeatGrid, BlendMode, Graph, GraphContext, LayerTimeSeries, PrimitiveTimeSeries, Series,
    SeriesSample,
};
use crate::models::scores::TrackScore;
use crate::node_graph::{run_graph_internal, GraphExecutionConfig, SharedAudioContext};
use crate::services::tracks::TARGET_SAMPLE_RATE;

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
    instance_seed: u64,
}

#[derive(Default)]
struct TrackCache {
    shared_audio: Option<SharedAudioContext>,
    annotations: HashMap<i64, CachedAnnotationLayer>,
    /// Cached final composite for fast "nothing changed" path
    composite_cache: Option<CachedComposite>,
}

/// Cached final composite with signature for change detection
#[derive(Clone)]
struct CachedComposite {
    /// Hash of all (annotation_id, signature) pairs - for quick "nothing changed" check
    full_signature_hash: u64,
    /// Per-annotation signatures for incremental diff
    annotation_signatures: HashMap<i64, AnnotationSignature>,
    /// The cached composite result
    composite: LayerTimeSeries,
    /// Track duration used for this composite
    track_duration: f32,
}

impl AnnotationSignature {
    fn new(annotation: &TrackScore, graph_hash: u64, instance_seed: u64) -> Self {
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
            instance_seed,
        }
    }

    /// Compare signatures for cache lookup, ignoring instance_seed.
    /// instance_seed is only used for stochastic patterns and shouldn't
    /// invalidate the cache when the pattern definition hasn't changed.
    fn matches_ignoring_seed(&self, other: &AnnotationSignature) -> bool {
        self.pattern_id == other.pattern_id
            && self.z_index == other.z_index
            && self.start_time_bits == other.start_time_bits
            && self.end_time_bits == other.end_time_bits
            && self.blend_mode == other.blend_mode
            && self.graph_hash == other.graph_hash
            && self.args_hash == other.args_hash
    }
}

fn hash_graph_json(graph_json: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    graph_json.hash(&mut hasher);
    hasher.finish()
}

/// Compute a hash of all annotation signatures for quick "nothing changed" detection
fn compute_full_signature_hash(signatures: &HashMap<i64, AnnotationSignature>) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Sort by annotation_id for deterministic hashing
    let mut sorted: Vec<_> = signatures.iter().collect();
    sorted.sort_by_key(|(id, _)| *id);
    for (id, sig) in sorted {
        id.hash(&mut hasher);
        sig.pattern_id.hash(&mut hasher);
        sig.z_index.hash(&mut hasher);
        sig.start_time_bits.hash(&mut hasher);
        sig.end_time_bits.hash(&mut hasher);
        sig.graph_hash.hash(&mut hasher);
        sig.args_hash.hash(&mut hasher);
        // Note: we intentionally exclude instance_seed from the full hash
        // because it changes every time but doesn't affect the composite
        // if the underlying pattern hasn't changed
    }
    hasher.finish()
}

/// Represents a time interval that needs recomputation
#[derive(Debug, Clone, Copy)]
struct DirtyInterval {
    start: f32,
    end: f32,
}

/// Merge overlapping intervals into a minimal set
fn merge_intervals(mut intervals: Vec<DirtyInterval>) -> Vec<DirtyInterval> {
    if intervals.is_empty() {
        return intervals;
    }
    intervals.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    let mut merged = vec![intervals[0]];
    for interval in intervals.into_iter().skip(1) {
        let last = merged.last_mut().unwrap();
        if last.end >= interval.start {
            last.end = last.end.max(interval.end);
        } else {
            merged.push(interval);
        }
    }
    merged
}

/// Compute which time intervals need recomputation based on changed annotations
fn compute_dirty_intervals(
    old_signatures: &HashMap<i64, AnnotationSignature>,
    new_signatures: &HashMap<i64, AnnotationSignature>,
) -> Vec<DirtyInterval> {
    let mut dirty = Vec::new();

    // Find removed annotations (in old but not in new)
    for (id, old_sig) in old_signatures {
        if !new_signatures.contains_key(id) {
            dirty.push(DirtyInterval {
                start: f64::from_bits(old_sig.start_time_bits) as f32,
                end: f64::from_bits(old_sig.end_time_bits) as f32,
            });
        }
    }

    // Find added or modified annotations
    for (id, new_sig) in new_signatures {
        match old_signatures.get(id) {
            None => {
                // Added annotation
                dirty.push(DirtyInterval {
                    start: f64::from_bits(new_sig.start_time_bits) as f32,
                    end: f64::from_bits(new_sig.end_time_bits) as f32,
                });
            }
            Some(old_sig) => {
                // Check if signature changed (excluding instance_seed)
                let changed = old_sig.pattern_id != new_sig.pattern_id
                    || old_sig.z_index != new_sig.z_index
                    || old_sig.start_time_bits != new_sig.start_time_bits
                    || old_sig.end_time_bits != new_sig.end_time_bits
                    || old_sig.blend_mode != new_sig.blend_mode
                    || old_sig.graph_hash != new_sig.graph_hash
                    || old_sig.args_hash != new_sig.args_hash;

                if changed {
                    // Mark both old and new time ranges as dirty
                    dirty.push(DirtyInterval {
                        start: f64::from_bits(old_sig.start_time_bits) as f32,
                        end: f64::from_bits(old_sig.end_time_bits) as f32,
                    });
                    dirty.push(DirtyInterval {
                        start: f64::from_bits(new_sig.start_time_bits) as f32,
                        end: f64::from_bits(new_sig.end_time_bits) as f32,
                    });
                }
            }
        }
    }

    // Also need to mark dirty any regions where z-index ordering changed
    // This is handled by checking all overlapping layers in dirty regions

    merge_intervals(dirty)
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
        .filter(|entry| entry.signature.matches_ignoring_seed(signature))
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
    let info = crate::database::local::tracks::get_track_path_and_hash(pool, track_id)
        .await
        .map_err(|e| format!("Failed to fetch track info: {}", e))?;
    Ok((info.file_path, info.track_hash))
}

async fn get_or_load_shared_audio(
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

    let audio = load_or_decode_audio(Path::new(track_path), track_hash, TARGET_SAMPLE_RATE)
        .map_err(|e| format!("Failed to load audio for track {}: {}", track_id, e))?;

    if audio.samples.is_empty() || audio.sample_rate == 0 {
        return Err(format!(
            "Audio for track {} is empty or has zero sample rate",
            track_id
        ));
    }

    // Convert stereo to mono for SharedAudioContext (used by node graph analysis)
    let mono_samples = stereo_to_mono(&audio.samples);

    let shared = SharedAudioContext {
        track_id,
        track_hash: track_hash.to_string(),
        samples: Arc::new(mono_samples),
        sample_rate: audio.sample_rate,
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
    host_audio: State<'_, HostAudioState>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, crate::audio::FftService>,
    track_id: i64,
    venue_id: i64,
    skip_cache: Option<bool>,
) -> Result<(), String> {
    let skip_cache = skip_cache.unwrap_or(false);
    let compose_start = Instant::now();
    // 1. Fetch all annotations for the track (sorted by z_index)
    let annotations = fetch_scores(&db.0, track_id).await?;
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

    // 2. Load beat grid for the track
    let beat_grid = load_beat_grid(&db.0, track_id).await?;

    // 3. Get track duration
    let track_duration = get_track_duration(&db.0, track_id).await?.unwrap_or(300.0);

    // 4. Preload audio once for all graph executions on this track
    let (track_path, track_hash) = fetch_track_path_and_hash(&db.0, track_id).await?;
    let shared_audio = get_or_load_shared_audio(track_id, &track_path, &track_hash).await?;

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

    // 6. Check if we can use cached composite without fetching graph JSONs
    // This is the fastest path - if annotation metadata matches, we can skip everything
    let (cached_composite, old_signatures) = {
        let cache_guard = COMPOSITION_CACHE
            .lock()
            .expect("composition cache mutex poisoned");
        cache_guard
            .get(&track_id)
            .and_then(|tc| tc.composite_cache.as_ref())
            .map(|cc| (Some(cc.clone()), cc.annotation_signatures.clone()))
            .unwrap_or((None, HashMap::new()))
    };

    // Try fast path: check if we can return cached composite without fetching any graphs
    // This works if: same annotations, same metadata (z_index, times, args, blend_mode)
    // and we have cached graph_hashes to compare
    if !skip_cache {
        if let Some(ref cached) = cached_composite {
            if (cached.track_duration - track_duration).abs() < 0.001 {
                // Check if annotation set matches and metadata is unchanged
                let mut can_use_cached_hashes = old_signatures.len() == annotations.len();
                if can_use_cached_hashes {
                    for ann in &annotations {
                        if let Some(old_sig) = old_signatures.get(&ann.id) {
                            // Build a temp signature with the OLD graph_hash to compare metadata
                            let temp_sig = AnnotationSignature::new(ann, old_sig.graph_hash, 0);
                            if !temp_sig.matches_ignoring_seed(old_sig) {
                                can_use_cached_hashes = false;
                                break;
                            }
                        } else {
                            can_use_cached_hashes = false;
                            break;
                        }
                    }
                }

                if can_use_cached_hashes {
                    // All metadata matches - return cached composite immediately
                    let total_ms = compose_start.elapsed().as_secs_f64() * 1000.0;
                    println!(
                        "[compositor] cache_hit track={} annotations={} total_ms={:.2} primitives={}",
                        track_id,
                        annotations.len(),
                        total_ms,
                        cached.composite.primitives.len()
                    );
                    host_audio.set_active_layer(Some(cached.composite.clone()));
                    return Ok(());
                }
            }
        }
    }

    // 7. Slow path: need to fetch graph JSONs to compute signatures
    // Also store the graph JSON to avoid fetching twice during execution
    let mut annotation_graphs: HashMap<i64, (u64, String)> = HashMap::new();
    for annotation in &annotations {
        let graph_json = fetch_pattern_graph(&db.0, annotation.pattern_id).await?;
        let graph_hash = hash_graph_json(&graph_json);
        annotation_graphs.insert(annotation.id, (graph_hash, graph_json));
    }

    // Build current signatures map (excluding instance_seed for composite cache comparison)
    let current_signatures: HashMap<i64, AnnotationSignature> = annotations
        .iter()
        .filter_map(|ann| {
            annotation_graphs.get(&ann.id).map(|(graph_hash, _)| {
                (ann.id, AnnotationSignature::new(ann, *graph_hash, 0)) // instance_seed=0 for comparison
            })
        })
        .collect();

    let current_full_hash = compute_full_signature_hash(&current_signatures);

    // 8. Execute each pattern and collect layers with their time ranges
    let mut annotation_layers: Vec<AnnotationLayer> = Vec::with_capacity(annotations.len());
    let mut computed_durations_ms: Vec<f64> = Vec::new();
    let mut layer_durations_ms: Vec<f64> = Vec::new();
    let mut reused_count = 0usize;
    let mut executed_count = 0usize;

    for annotation in &annotations {
        let (graph_hash, graph_json) = annotation_graphs.get(&annotation.id).unwrap();
        let instance_seed = rand::random::<u64>();
        let signature = AnnotationSignature::new(annotation, *graph_hash, instance_seed);

        if !skip_cache {
            if let Some(cached) = lookup_cached_layer(track_id, annotation.id, &signature) {
                reused_count += 1;
                layer_durations_ms.push(cached.graph_time_ms);
                annotation_layers.push(cached.layer);
                continue;
            }
        }

        let graph: Graph = serde_json::from_str(graph_json)
            .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;

        if graph.nodes.is_empty() {
            continue; // Skip empty graphs
        }

        // Create context for this annotation's time range
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

        // Execute the graph
        let run_start = Instant::now();
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

    // 9. Create composite layer - use incremental build if possible
    let composite_start = Instant::now();
    let mut composited: LayerTimeSeries;
    let incremental_samples: usize;
    let total_samples: usize;

    let dirty_intervals = compute_dirty_intervals(&old_signatures, &current_signatures);
    let can_do_incremental = !skip_cache
        && cached_composite.is_some()
        && !dirty_intervals.is_empty()
        && (cached_composite.as_ref().unwrap().track_duration - track_duration).abs() < 0.001;

    // Track dirty ranges and affected primitives for incremental prepositioning
    let mut dirty_ranges_for_preposition: Option<Vec<(usize, usize)>> = None;
    let mut affected_primitives_for_preposition: Option<HashSet<String>> = None;

    if can_do_incremental {
        // Incremental build: only recompute dirty regions
        let cached = cached_composite.as_ref().unwrap();

        // Count how many samples were in dirty regions (using efficient range calculation)
        let num_samples = (track_duration * COMPOSITE_SAMPLE_RATE).ceil() as usize;
        total_samples = num_samples.max(2);
        let dirty_ranges =
            intervals_to_sample_ranges(&dirty_intervals, total_samples, track_duration);
        incremental_samples = dirty_ranges.iter().map(|(s, e)| e - s).sum();

        composited = composite_layers_incremental(
            &cached.composite,
            &annotation_layers,
            &dirty_intervals,
            track_duration,
        );

        // Save for incremental prepositioning
        dirty_ranges_for_preposition = Some(dirty_ranges);

        // Compute affected primitives for prepositioning
        let affected: HashSet<String> = annotation_layers
            .iter()
            .filter(|layer| {
                dirty_intervals
                    .iter()
                    .any(|iv| layer.start_time < iv.end && layer.end_time > iv.start)
            })
            .flat_map(|l| l.layer.primitives.iter().map(|p| p.primitive_id.clone()))
            .collect();
        affected_primitives_for_preposition = Some(affected);
    } else if dirty_intervals.is_empty() && cached_composite.is_some() && !skip_cache {
        // No dirty intervals means nothing actually changed (signatures matched but hash didn't?)
        // This shouldn't happen normally, but fall back to cached
        composited = cached_composite.as_ref().unwrap().composite.clone();
        total_samples = composited
            .primitives
            .first()
            .and_then(|p| p.dimmer.as_ref())
            .map(|s| s.samples.len())
            .unwrap_or(0);
        incremental_samples = 0; // Nothing recomputed
    } else {
        // Full rebuild
        composited = composite_layers_unified(annotation_layers.clone(), track_duration);
        total_samples = composited
            .primitives
            .first()
            .and_then(|p| p.dimmer.as_ref())
            .map(|s| s.samples.len())
            .unwrap_or(0);
        incremental_samples = total_samples; // All samples computed
    }

    // 10. Pre-position fixtures: during gaps between patterns, move to next pattern's start position
    // Use incremental prepositioning if we have dirty ranges
    preposition_fixtures_with_ranges(
        &mut composited,
        &annotation_layers,
        track_duration,
        dirty_ranges_for_preposition.as_deref(),
        affected_primitives_for_preposition.as_ref(),
    );
    let composite_ms = composite_start.elapsed().as_secs_f64() * 1000.0;

    // 11. Update composite cache
    {
        let mut cache_guard = COMPOSITION_CACHE
            .lock()
            .expect("composition cache mutex poisoned");
        let entry = cache_guard.entry(track_id).or_default();
        entry.composite_cache = Some(CachedComposite {
            full_signature_hash: current_full_hash,
            annotation_signatures: current_signatures,
            composite: composited.clone(),
            track_duration,
        });
    }

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

    let incremental_pct = if total_samples > 0 {
        (incremental_samples as f64 / total_samples as f64) * 100.0
    } else {
        100.0
    };

    println!(
        "[compositor] pass track={} annotations={} reused={} executed={} avg_graph_ms={:.2} avg_layer_ms={:.2} composite_ms={:.2} total_ms={:.2} primitives={} incremental={:.1}%",
        track_id,
        annotations.len(),
        reused_count,
        executed_count,
        avg_graph_ms,
        avg_layer_ms,
        composite_ms,
        total_ms,
        composited.primitives.len(),
        incremental_pct
    );

    // 12. Push to host audio
    host_audio.set_active_layer(Some(composited));

    Ok(())
}

/// Fetch annotations for a track, sorted by z_index ascending
async fn fetch_scores(pool: &sqlx::SqlitePool, track_id: i64) -> Result<Vec<TrackScore>, String> {
    crate::database::local::scores::get_scores_for_track(pool, track_id)
        .await
        .map_err(|e| format!("Failed to fetch scores: {}", e))
}

/// Load beat grid for a track
async fn load_beat_grid(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> Result<Option<BeatGrid>, String> {
    crate::services::tracks::get_track_beats(pool, track_id)
        .await
        .map_err(|e| format!("Failed to load beat grid: {}", e))
}

/// Get track duration in seconds
async fn get_track_duration(pool: &sqlx::SqlitePool, track_id: i64) -> Result<Option<f32>, String> {
    crate::database::local::tracks::get_track_duration(pool, track_id)
        .await
        .map(|opt| opt.map(|v| v as f32))
}

/// Fetch pattern graph JSON from project DB
async fn fetch_pattern_graph(pool: &sqlx::SqlitePool, pattern_id: i64) -> Result<String, String> {
    crate::database::local::patterns::get_pattern_graph_pool(pool, pattern_id).await
}

/// Sample a Series at a specific time. Optionally interpolate between points.
/// Uses binary search for O(log n) lookup instead of O(n) linear scan.
fn sample_series(series: &Series, time: f32, interpolate: bool) -> Option<Vec<f32>> {
    if series.samples.is_empty() {
        return None;
    }

    let samples = &series.samples;

    // Binary search: find the first sample where sample.time > time
    // This gives us the index of 'next', and 'prev' is at index - 1
    let next_idx = samples.partition_point(|s| s.time <= time);

    let prev = if next_idx > 0 {
        Some(&samples[next_idx - 1])
    } else {
        None
    };

    let next = if next_idx < samples.len() {
        Some(&samples[next_idx])
    } else {
        None
    };

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

/// Convert time intervals to sample index ranges
fn intervals_to_sample_ranges(
    intervals: &[DirtyInterval],
    num_samples: usize,
    track_duration: f32,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for interval in intervals {
        // Convert time to sample index
        let start_idx =
            ((interval.start / track_duration) * (num_samples - 1) as f32).floor() as usize;
        let end_idx = ((interval.end / track_duration) * (num_samples - 1) as f32).ceil() as usize;
        let start_idx = start_idx.min(num_samples);
        let end_idx = end_idx.min(num_samples);
        if start_idx < end_idx {
            ranges.push((start_idx, end_idx));
        }
    }
    // Merge overlapping ranges
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|(s, _)| *s);
    let mut merged = vec![ranges[0]];
    for (start, end) in ranges.into_iter().skip(1) {
        let last = merged.last_mut().unwrap();
        if start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

/// Incrementally update a cached composite by only recomputing dirty time intervals.
/// Returns the updated composite.
fn composite_layers_incremental(
    cached: &LayerTimeSeries,
    layers: &[AnnotationLayer],
    dirty_intervals: &[DirtyInterval],
    track_duration: f32,
) -> LayerTimeSeries {
    // Sort layers by z-index for compositing
    let mut sorted_layers: Vec<_> = layers.to_vec();
    sorted_layers.sort_by_key(|l| l.z_index);

    let num_samples = (track_duration * COMPOSITE_SAMPLE_RATE).ceil() as usize;
    let num_samples = num_samples.max(2);

    // Convert dirty time intervals to sample index ranges (computed once, not per-sample)
    let dirty_ranges = intervals_to_sample_ranges(dirty_intervals, num_samples, track_duration);

    // Pre-compute which primitives are affected by dirty layers
    // A primitive is affected if any layer in a dirty interval contains it
    let affected_primitives: HashSet<String> = sorted_layers
        .iter()
        .filter(|layer| {
            dirty_intervals.iter().any(|iv| {
                // Layer overlaps with dirty interval
                layer.start_time < iv.end && layer.end_time > iv.start
            })
        })
        .flat_map(|l| l.layer.primitives.iter().map(|p| p.primitive_id.clone()))
        .collect();

    // Build a map of cached primitives for fast lookup
    let cached_map: HashMap<&str, &PrimitiveTimeSeries> = cached
        .primitives
        .iter()
        .map(|p| (p.primitive_id.as_str(), p))
        .collect();

    let mut composited_primitives: Vec<PrimitiveTimeSeries> = Vec::new();

    // First, add all cached primitives that are NOT affected (no cloning of samples!)
    for cached_prim in &cached.primitives {
        if !affected_primitives.contains(&cached_prim.primitive_id) {
            composited_primitives.push(cached_prim.clone());
        }
    }

    // Now process only affected primitives
    for primitive_id in &affected_primitives {
        // Start with cached data if available, otherwise create empty series
        let (
            mut dimmer_samples,
            mut color_samples,
            mut position_samples,
            mut strobe_samples,
            mut speed_samples,
        ) = if let Some(cached_prim) = cached_map.get(primitive_id.as_str()) {
            (
                cached_prim
                    .dimmer
                    .as_ref()
                    .map(|s| s.samples.clone())
                    .unwrap_or_default(),
                cached_prim
                    .color
                    .as_ref()
                    .map(|s| s.samples.clone())
                    .unwrap_or_default(),
                cached_prim
                    .position
                    .as_ref()
                    .map(|s| s.samples.clone())
                    .unwrap_or_default(),
                cached_prim
                    .strobe
                    .as_ref()
                    .map(|s| s.samples.clone())
                    .unwrap_or_default(),
                cached_prim
                    .speed
                    .as_ref()
                    .map(|s| s.samples.clone())
                    .unwrap_or_default(),
            )
        } else {
            // New primitive - create empty samples that will be filled in dirty regions
            let empty: Vec<SeriesSample> = (0..num_samples)
                .map(|i| {
                    let time = (i as f32 / (num_samples - 1) as f32) * track_duration;
                    SeriesSample {
                        time,
                        values: vec![0.0],
                        label: None,
                    }
                })
                .collect();
            (
                empty.clone(),
                empty.clone(),
                empty.clone(),
                empty.clone(),
                empty,
            )
        };

        // Ensure we have the right number of samples
        while dimmer_samples.len() < num_samples {
            let time = (dimmer_samples.len() as f32 / (num_samples - 1) as f32) * track_duration;
            dimmer_samples.push(SeriesSample {
                time,
                values: vec![0.0],
                label: None,
            });
            color_samples.push(SeriesSample {
                time,
                values: vec![0.0, 0.0, 0.0],
                label: None,
            });
            position_samples.push(SeriesSample {
                time,
                values: vec![f32::NAN, f32::NAN],
                label: None,
            });
            strobe_samples.push(SeriesSample {
                time,
                values: vec![0.0],
                label: None,
            });
            speed_samples.push(SeriesSample {
                time,
                values: vec![1.0],
                label: None,
            });
        }

        // Only iterate over dirty sample ranges (not all samples!)
        for &(range_start, range_end) in &dirty_ranges {
            for i in range_start..range_end {
                let time = (i as f32 / (num_samples - 1) as f32) * track_duration;

                // Recompute this sample (same logic as composite_layers_unified)
                let mut current_dimmer = 0.0;
                let mut current_color = vec![0.0, 0.0, 0.0, 0.0];
                let mut current_position = vec![f32::NAN, f32::NAN];
                let mut current_strobe = 0.0;
                let mut current_speed = 1.0f32;
                let mut available_color: Option<Vec<f32>> = None;

                for layer in &sorted_layers {
                    if time >= layer.start_time && time < layer.end_time {
                        if let Some(prim) = layer
                            .layer
                            .primitives
                            .iter()
                            .find(|p| p.primitive_id == *primitive_id)
                        {
                            let mut layer_dimmer_sample: Option<f32> = None;

                            if let Some(s) = &prim.dimmer {
                                if let Some(vals) = sample_series(s, time, true) {
                                    if let Some(v) = vals.first() {
                                        layer_dimmer_sample = Some(*v);
                                        current_dimmer =
                                            blend_values(current_dimmer, *v, layer.blend_mode);
                                    }
                                }
                            }

                            let sampled_color: Option<Vec<f32>> = prim
                                .color
                                .as_ref()
                                .and_then(|s| sample_series(s, time, true))
                                .filter(|v| v.len() >= 3)
                                .map(|v| {
                                    if v.len() >= 4 {
                                        v
                                    } else {
                                        vec![v[0], v[1], v[2], 1.0]
                                    }
                                });

                            let sampled_a = sampled_color
                                .as_ref()
                                .and_then(|v| v.get(3).copied())
                                .unwrap_or(1.0)
                                .clamp(0.0, 1.0);

                            let has_color_override = sampled_color.is_some() && sampled_a > 0.0001;

                            let inherited = available_color
                                .clone()
                                .unwrap_or_else(|| vec![0.0, 0.0, 0.0, 1.0]);
                            let layer_hue: Option<Vec<f32>> = if let Some(ref top) = sampled_color {
                                if sampled_a <= 0.0001 {
                                    available_color.clone()
                                } else if sampled_a >= 0.9999 {
                                    Some(vec![top[0], top[1], top[2], 1.0])
                                } else {
                                    let r = inherited.get(0).copied().unwrap_or(0.0)
                                        * (1.0 - sampled_a)
                                        + top.get(0).copied().unwrap_or(0.0) * sampled_a;
                                    let g = inherited.get(1).copied().unwrap_or(0.0)
                                        * (1.0 - sampled_a)
                                        + top.get(1).copied().unwrap_or(0.0) * sampled_a;
                                    let b = inherited.get(2).copied().unwrap_or(0.0)
                                        * (1.0 - sampled_a)
                                        + top.get(2).copied().unwrap_or(0.0) * sampled_a;
                                    Some(vec![r, g, b, 1.0])
                                }
                            } else {
                                available_color.clone()
                            };

                            let layer_alpha = layer_dimmer_sample.unwrap_or(0.0);

                            if let Some(ref hue) = layer_hue {
                                let top_rgba = vec![hue[0], hue[1], hue[2], layer_alpha];
                                current_color =
                                    blend_color(&current_color, &top_rgba, BlendMode::Replace);
                            }

                            if has_color_override {
                                available_color = layer_hue;
                            }

                            if let Some(s) = &prim.position {
                                if let Some(vals) = sample_series(s, time, true) {
                                    if vals.len() >= 2 {
                                        let pan = vals[0];
                                        let tilt = vals[1];
                                        if pan.is_finite() {
                                            current_position.resize(2, f32::NAN);
                                            current_position[0] = pan;
                                        }
                                        if tilt.is_finite() {
                                            current_position.resize(2, f32::NAN);
                                            current_position[1] = tilt;
                                        }
                                    }
                                }
                            }

                            if let Some(s) = &prim.strobe {
                                if let Some(vals) = sample_series(s, time, false) {
                                    if let Some(v) = vals.first() {
                                        current_strobe =
                                            blend_values(current_strobe, *v, layer.blend_mode);
                                    }
                                }
                            }

                            if let Some(s) = &prim.speed {
                                if let Some(vals) = sample_series(s, time, false) {
                                    if let Some(v) = vals.first() {
                                        let speed_val = if *v > 0.5 { 1.0 } else { 0.0 };
                                        current_speed *= speed_val;
                                    }
                                }
                            }
                        }
                    }
                }

                // Update the samples
                dimmer_samples[i] = SeriesSample {
                    time,
                    values: vec![current_dimmer],
                    label: None,
                };

                let rgb_color = if current_color.len() >= 3 {
                    current_color[0..3].to_vec()
                } else {
                    current_color
                };
                color_samples[i] = SeriesSample {
                    time,
                    values: rgb_color,
                    label: None,
                };
                position_samples[i] = SeriesSample {
                    time,
                    values: current_position.clone(),
                    label: None,
                };
                strobe_samples[i] = SeriesSample {
                    time,
                    values: vec![current_strobe],
                    label: None,
                };
                speed_samples[i] = SeriesSample {
                    time,
                    values: vec![current_speed],
                    label: None,
                };
            }
        }

        composited_primitives.push(PrimitiveTimeSeries {
            primitive_id: primitive_id.clone(),
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
            position: Some(Series {
                dim: 2,
                labels: None,
                samples: position_samples,
            }),
            strobe: Some(Series {
                dim: 1,
                labels: None,
                samples: strobe_samples,
            }),
            speed: Some(Series {
                dim: 1,
                labels: None,
                samples: speed_samples,
            }),
        });
    }

    LayerTimeSeries {
        primitives: composited_primitives,
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
        let mut position_samples: Vec<SeriesSample> = Vec::with_capacity(num_samples);
        let mut strobe_samples: Vec<SeriesSample> = Vec::with_capacity(num_samples);
        let mut speed_samples: Vec<SeriesSample> = Vec::with_capacity(num_samples);

        for i in 0..num_samples {
            let time = (i as f32 / (num_samples - 1) as f32) * track_duration;

            // Default values (Transparent Black/Zero)
            let mut current_dimmer = 0.0;
            let mut current_color = vec![0.0, 0.0, 0.0, 0.0];
            // Default to NaN so downstream DMX can "hold last" when nothing writes movement.
            // A layer can override only pan or tilt by leaving the other axis as NaN.
            let mut current_position = vec![f32::NAN, f32::NAN];
            let mut current_strobe = 0.0;
            // Speed defaults to 1.0 (fast). Compositing rule: multiply (any 0 = frozen).
            let mut current_speed = 1.0f32;
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
                        let sampled_color: Option<Vec<f32>> = prim
                            .color
                            .as_ref()
                            .and_then(|s| sample_series(s, time, true))
                            .filter(|v| v.len() >= 3)
                            .map(|v| {
                                if v.len() >= 4 {
                                    v
                                } else {
                                    vec![v[0], v[1], v[2], 1.0]
                                }
                            });

                        // Interpret color alpha as "mix amount" (tint strength), not opacity.
                        // Opacity/intensity is controlled solely by dimmer.
                        let sampled_a = sampled_color
                            .as_ref()
                            .and_then(|v| v.get(3).copied())
                            .unwrap_or(1.0)
                            .clamp(0.0, 1.0);

                        // Treat alpha == 0 as "no override" (inherit).
                        let has_color_override = sampled_color.is_some() && sampled_a > 0.0001;

                        // Determine the hue to use for this layer:
                        // - If no override, inherit hue from below (if available).
                        // - If override (alpha ~ 1), use sampled hue.
                        // - If mix (0 < alpha < 1), blend inherited hue -> sampled hue by alpha.
                        let inherited = available_color
                            .clone()
                            .unwrap_or_else(|| vec![0.0, 0.0, 0.0, 1.0]);
                        let layer_hue: Option<Vec<f32>> = if let Some(ref top) = sampled_color {
                            if sampled_a <= 0.0001 {
                                available_color.clone()
                            } else if sampled_a >= 0.9999 {
                                Some(vec![top[0], top[1], top[2], 1.0])
                            } else {
                                let r = inherited.get(0).copied().unwrap_or(0.0)
                                    * (1.0 - sampled_a)
                                    + top.get(0).copied().unwrap_or(0.0) * sampled_a;
                                let g = inherited.get(1).copied().unwrap_or(0.0)
                                    * (1.0 - sampled_a)
                                    + top.get(1).copied().unwrap_or(0.0) * sampled_a;
                                let b = inherited.get(2).copied().unwrap_or(0.0)
                                    * (1.0 - sampled_a)
                                    + top.get(2).copied().unwrap_or(0.0) * sampled_a;
                                Some(vec![r, g, b, 1.0])
                            }
                        } else {
                            available_color.clone()
                        };

                        // Dimmer acts as opacity/intensity: defaults to 0 (invisible) if not defined
                        let layer_alpha = layer_dimmer_sample.unwrap_or(0.0);

                        // Blend: hue with dimmer as opacity (do not double-multiply)
                        if let Some(ref hue) = layer_hue {
                            let top_rgba = vec![hue[0], hue[1], hue[2], layer_alpha];
                            current_color =
                                blend_color(&current_color, &top_rgba, BlendMode::Replace);
                        }

                        // Update inherited color for layers above (hue only, not dimmer)
                        if has_color_override {
                            available_color = layer_hue;
                        }

                        // Movement: strict override by z-index (no blending).
                        // If a layer defines position, it wins for the axes it specifies.
                        if let Some(s) = &prim.position {
                            if let Some(vals) = sample_series(s, time, true) {
                                if vals.len() >= 2 {
                                    let pan = vals[0];
                                    let tilt = vals[1];
                                    if pan.is_finite() {
                                        current_position.resize(2, f32::NAN);
                                        current_position[0] = pan;
                                    }
                                    if tilt.is_finite() {
                                        current_position.resize(2, f32::NAN);
                                        current_position[1] = tilt;
                                    }
                                }
                            }
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

                        // If layer defines speed, multiply it (any 0 = frozen)
                        if let Some(s) = &prim.speed {
                            if let Some(vals) = sample_series(s, time, false) {
                                if let Some(v) = vals.first() {
                                    // Binary: treat as 0 or 1
                                    let speed_val = if *v > 0.5 { 1.0 } else { 0.0 };
                                    current_speed *= speed_val;
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

            position_samples.push(SeriesSample {
                time,
                values: current_position.clone(),
                label: None,
            });

            strobe_samples.push(SeriesSample {
                time,
                values: vec![current_strobe],
                label: None,
            });

            speed_samples.push(SeriesSample {
                time,
                values: vec![current_speed],
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
            position: Some(Series {
                dim: 2,
                labels: None,
                samples: position_samples,
            }),
            strobe: Some(Series {
                dim: 1,
                labels: None,
                samples: strobe_samples,
            }),
            speed: Some(Series {
                dim: 1,
                labels: None,
                samples: speed_samples,
            }),
        });
    }

    LayerTimeSeries {
        primitives: composited_primitives,
    }
}

/// Pre-position fixtures during gaps between patterns.
///
/// When no pattern is active for a primitive (gaps between annotations), we look ahead
/// to find the next pattern that will use this primitive and set position/color to match
/// that pattern's starting state. This allows fixtures to physically move into position
/// during the gap, so they're ready when the next pattern starts.
///
/// This is different from checking dimmer=0, because a pattern might intentionally
/// animate position while the dimmer is off (for artistic effect).
///
/// If `dirty_ranges` is provided, only processes samples within those ranges (for incremental updates).
fn preposition_fixtures_with_ranges(
    layer: &mut LayerTimeSeries,
    annotations: &[AnnotationLayer],
    track_duration: f32,
    dirty_ranges: Option<&[(usize, usize)]>,
    affected_primitives: Option<&HashSet<String>>,
) {
    let sample_interval = 1.0 / COMPOSITE_SAMPLE_RATE;

    for prim in &mut layer.primitives {
        let prim_id = &prim.primitive_id;

        // Skip primitives that aren't affected (for incremental updates)
        if let Some(affected) = affected_primitives {
            if !affected.contains(prim_id) {
                continue;
            }
        }

        // Collect annotations that contain this primitive, sorted by start time
        let mut prim_annotations: Vec<&AnnotationLayer> = annotations
            .iter()
            .filter(|ann| {
                ann.layer
                    .primitives
                    .iter()
                    .any(|p| p.primitive_id == *prim_id)
            })
            .collect();
        prim_annotations.sort_by(|a, b| a.start_time.partial_cmp(&b.start_time).unwrap());

        if prim_annotations.is_empty() {
            continue;
        }

        // Get the number of samples from the composited position/color series
        let num_samples = prim
            .position
            .as_ref()
            .map(|s| s.samples.len())
            .or_else(|| prim.color.as_ref().map(|s| s.samples.len()))
            .unwrap_or(0);

        if num_samples == 0 {
            continue;
        }

        // Build iterator over sample indices based on dirty_ranges
        let sample_indices: Box<dyn Iterator<Item = usize>> = if let Some(ranges) = dirty_ranges {
            Box::new(ranges.iter().flat_map(|&(start, end)| start..end))
        } else {
            Box::new(0..num_samples)
        };

        // For each sample, determine if it's in a gap (no annotation active)
        // and if so, what the next annotation's starting position/color is
        for i in sample_indices {
            let time = (i as f32 / (num_samples - 1).max(1) as f32) * track_duration;

            // Check if any annotation is active at this time
            let in_pattern = prim_annotations
                .iter()
                .any(|ann| time >= ann.start_time && time < ann.end_time);

            if in_pattern {
                continue; // Pattern is active, don't pre-position
            }

            // We're in a gap - find the next annotation that has position data for this primitive
            let next_ann_with_position = prim_annotations.iter().find(|ann| {
                if ann.start_time <= time + sample_interval * 0.5 {
                    return false;
                }
                // Check if this annotation has position data for our primitive
                ann.layer
                    .primitives
                    .iter()
                    .find(|p| p.primitive_id == *prim_id)
                    .and_then(|p| p.position.as_ref())
                    .map(|s| !s.samples.is_empty())
                    .unwrap_or(false)
            });

            // Also find next annotation with color data (might be different annotation)
            let next_ann_with_color = prim_annotations.iter().find(|ann| {
                if ann.start_time <= time + sample_interval * 0.5 {
                    return false;
                }
                ann.layer
                    .primitives
                    .iter()
                    .find(|p| p.primitive_id == *prim_id)
                    .and_then(|p| p.color.as_ref())
                    .map(|s| !s.samples.is_empty())
                    .unwrap_or(false)
            });

            // Pre-position using the next annotation that has position data
            if let Some(next_ann) = next_ann_with_position {
                if let Some(next_prim) = next_ann
                    .layer
                    .primitives
                    .iter()
                    .find(|p| p.primitive_id == *prim_id)
                {
                    if let Some(ref mut pos_series) = prim.position {
                        if let Some(ref next_pos) = next_prim.position {
                            if let Some(first_sample) = next_pos.samples.first() {
                                if i < pos_series.samples.len() {
                                    pos_series.samples[i].values = first_sample.values.clone();
                                }
                            }
                        }
                    }
                }
            }

            // Pre-position color using the next annotation that has color data
            if let Some(next_ann) = next_ann_with_color {
                if let Some(next_prim) = next_ann
                    .layer
                    .primitives
                    .iter()
                    .find(|p| p.primitive_id == *prim_id)
                {
                    if let Some(ref mut color_series) = prim.color {
                        if let Some(ref next_color) = next_prim.color {
                            if let Some(first_sample) = next_color.samples.first() {
                                if i < color_series.samples.len() {
                                    color_series.samples[i].values = first_sample.values.clone();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Wrapper for full track prepositioning (used by tests and full rebuilds)
fn preposition_fixtures(
    layer: &mut LayerTimeSeries,
    annotations: &[AnnotationLayer],
    track_duration: f32,
) {
    preposition_fixtures_with_ranges(layer, annotations, track_duration, None, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::node_graph::{PrimitiveTimeSeries, Series, SeriesSample};

    fn series2(v0: f32, v1: f32) -> Series {
        Series {
            dim: 2,
            labels: None,
            samples: vec![
                SeriesSample {
                    time: 0.0,
                    values: vec![v0, v1],
                    label: None,
                },
                SeriesSample {
                    time: 1.0,
                    values: vec![v0, v1],
                    label: None,
                },
            ],
        }
    }

    #[test]
    fn position_is_strictly_overridden_by_top_layer_axes() {
        let bottom = AnnotationLayer {
            start_time: 0.0,
            end_time: 1.0,
            z_index: 0,
            blend_mode: BlendMode::Replace,
            layer: LayerTimeSeries {
                primitives: vec![PrimitiveTimeSeries {
                    primitive_id: "p".into(),
                    color: None,
                    dimmer: None,
                    position: Some(series2(10.0, 20.0)),
                    strobe: None,
                    speed: None,
                }],
            },
        };

        // Top overrides pan only; tilt stays from below.
        let top = AnnotationLayer {
            start_time: 0.0,
            end_time: 1.0,
            z_index: 10,
            blend_mode: BlendMode::Replace,
            layer: LayerTimeSeries {
                primitives: vec![PrimitiveTimeSeries {
                    primitive_id: "p".into(),
                    color: None,
                    dimmer: None,
                    position: Some(series2(30.0, f32::NAN)),
                    strobe: None,
                    speed: None,
                }],
            },
        };

        let composited = composite_layers_unified(vec![bottom, top], 1.0);
        let prim = composited
            .primitives
            .iter()
            .find(|p| p.primitive_id == "p")
            .unwrap();
        let pos = prim.position.as_ref().unwrap();
        let v = sample_series(pos, 0.5, true).unwrap();
        assert!((v[0] - 30.0).abs() < 1e-4);
        assert!((v[1] - 20.0).abs() < 1e-4);
    }

    #[test]
    fn preposition_moves_to_next_pattern_position() {
        // Simulate: Gap (0-3s) -> Pattern (3-6s)
        // During the gap, position should pre-position to pattern's start position
        let track_duration = 6.0;

        // The composited layer (what we'll modify)
        let mut composited = LayerTimeSeries {
            primitives: vec![PrimitiveTimeSeries {
                primitive_id: "test".into(),
                dimmer: None,
                position: Some(Series {
                    dim: 2,
                    labels: None,
                    samples: vec![
                        SeriesSample {
                            time: 0.0,
                            values: vec![0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 1.0,
                            values: vec![0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 2.0,
                            values: vec![0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 3.0,
                            values: vec![100.0, 50.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 4.0,
                            values: vec![100.0, 50.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 5.0,
                            values: vec![100.0, 50.0],
                            label: None,
                        },
                    ],
                }),
                color: Some(Series {
                    dim: 3,
                    labels: None,
                    samples: vec![
                        SeriesSample {
                            time: 0.0,
                            values: vec![0.0, 0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 1.0,
                            values: vec![0.0, 0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 2.0,
                            values: vec![0.0, 0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 3.0,
                            values: vec![1.0, 0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 4.0,
                            values: vec![1.0, 0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 5.0,
                            values: vec![1.0, 0.0, 0.0],
                            label: None,
                        },
                    ],
                }),
                strobe: None,
                speed: None,
            }],
        };

        // The annotation that covers 3-6s
        let annotations = vec![AnnotationLayer {
            start_time: 3.0,
            end_time: 6.0,
            z_index: 0,
            blend_mode: BlendMode::Replace,
            layer: LayerTimeSeries {
                primitives: vec![PrimitiveTimeSeries {
                    primitive_id: "test".into(),
                    dimmer: None,
                    position: Some(Series {
                        dim: 2,
                        labels: None,
                        samples: vec![
                            SeriesSample {
                                time: 3.0,
                                values: vec![100.0, 50.0],
                                label: None,
                            },
                            SeriesSample {
                                time: 6.0,
                                values: vec![100.0, 50.0],
                                label: None,
                            },
                        ],
                    }),
                    color: Some(Series {
                        dim: 3,
                        labels: None,
                        samples: vec![
                            SeriesSample {
                                time: 3.0,
                                values: vec![1.0, 0.0, 0.0],
                                label: None,
                            },
                            SeriesSample {
                                time: 6.0,
                                values: vec![1.0, 0.0, 0.0],
                                label: None,
                            },
                        ],
                    }),
                    strobe: None,
                    speed: None,
                }],
            },
        }];

        preposition_fixtures(&mut composited, &annotations, track_duration);

        let prim = &composited.primitives[0];

        // Samples 0-2 are in the gap (before pattern starts at 3.0)
        // They should be pre-positioned to the pattern's starting position
        let pos = prim.position.as_ref().unwrap();
        assert_eq!(pos.samples[0].values, vec![100.0, 50.0]);
        assert_eq!(pos.samples[1].values, vec![100.0, 50.0]);
        assert_eq!(pos.samples[2].values, vec![100.0, 50.0]);

        let color = prim.color.as_ref().unwrap();
        assert_eq!(color.samples[0].values, vec![1.0, 0.0, 0.0]);
        assert_eq!(color.samples[1].values, vec![1.0, 0.0, 0.0]);
        assert_eq!(color.samples[2].values, vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn preposition_handles_gaps_between_patterns() {
        // Simulate: Pattern A (0-2s) -> Gap (2-4s) -> Pattern B (4-6s)
        // During the gap, should pre-position to Pattern B's start
        let track_duration = 6.0;

        let mut composited = LayerTimeSeries {
            primitives: vec![PrimitiveTimeSeries {
                primitive_id: "test".into(),
                dimmer: None,
                position: Some(Series {
                    dim: 2,
                    labels: None,
                    samples: vec![
                        SeriesSample {
                            time: 0.0,
                            values: vec![10.0, 10.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 1.0,
                            values: vec![10.0, 10.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 2.0,
                            values: vec![10.0, 10.0],
                            label: None,
                        }, // Gap starts
                        SeriesSample {
                            time: 3.0,
                            values: vec![0.0, 0.0],
                            label: None,
                        }, // In gap
                        SeriesSample {
                            time: 4.0,
                            values: vec![50.0, 50.0],
                            label: None,
                        }, // Pattern B
                        SeriesSample {
                            time: 5.0,
                            values: vec![50.0, 50.0],
                            label: None,
                        },
                    ],
                }),
                color: None,
                strobe: None,
                speed: None,
            }],
        };

        let annotations = vec![
            AnnotationLayer {
                start_time: 0.0,
                end_time: 2.0,
                z_index: 0,
                blend_mode: BlendMode::Replace,
                layer: LayerTimeSeries {
                    primitives: vec![PrimitiveTimeSeries {
                        primitive_id: "test".into(),
                        dimmer: None,
                        position: Some(series2(10.0, 10.0)),
                        color: None,
                        strobe: None,
                        speed: None,
                    }],
                },
            },
            AnnotationLayer {
                start_time: 4.0,
                end_time: 6.0,
                z_index: 0,
                blend_mode: BlendMode::Replace,
                layer: LayerTimeSeries {
                    primitives: vec![PrimitiveTimeSeries {
                        primitive_id: "test".into(),
                        dimmer: None,
                        position: Some(Series {
                            dim: 2,
                            labels: None,
                            samples: vec![
                                SeriesSample {
                                    time: 4.0,
                                    values: vec![50.0, 50.0],
                                    label: None,
                                },
                                SeriesSample {
                                    time: 6.0,
                                    values: vec![50.0, 50.0],
                                    label: None,
                                },
                            ],
                        }),
                        color: None,
                        strobe: None,
                        speed: None,
                    }],
                },
            },
        ];

        preposition_fixtures(&mut composited, &annotations, track_duration);

        let pos = composited.primitives[0].position.as_ref().unwrap();
        // Samples 0-1 are in Pattern A, unchanged
        assert_eq!(pos.samples[0].values, vec![10.0, 10.0]);
        assert_eq!(pos.samples[1].values, vec![10.0, 10.0]);
        // Samples 2-3 are in the gap, should pre-position to Pattern B's start (50, 50)
        assert_eq!(pos.samples[2].values, vec![50.0, 50.0]);
        assert_eq!(pos.samples[3].values, vec![50.0, 50.0]);
        // Samples 4-5 are in Pattern B, unchanged
        assert_eq!(pos.samples[4].values, vec![50.0, 50.0]);
        assert_eq!(pos.samples[5].values, vec![50.0, 50.0]);
    }

    #[test]
    fn preposition_does_not_modify_active_pattern() {
        // Verify that samples within an active pattern are NOT modified
        // even if dimmer is 0 (pattern might intentionally animate while off)
        let track_duration = 4.0;

        let mut composited = LayerTimeSeries {
            primitives: vec![PrimitiveTimeSeries {
                primitive_id: "test".into(),
                dimmer: None,
                position: Some(Series {
                    dim: 2,
                    labels: None,
                    samples: vec![
                        SeriesSample {
                            time: 0.0,
                            values: vec![0.0, 0.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 1.0,
                            values: vec![25.0, 25.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 2.0,
                            values: vec![50.0, 50.0],
                            label: None,
                        },
                        SeriesSample {
                            time: 3.0,
                            values: vec![75.0, 75.0],
                            label: None,
                        },
                    ],
                }),
                color: None,
                strobe: None,
                speed: None,
            }],
        };

        // Single pattern covering the entire duration
        let annotations = vec![AnnotationLayer {
            start_time: 0.0,
            end_time: 4.0,
            z_index: 0,
            blend_mode: BlendMode::Replace,
            layer: LayerTimeSeries {
                primitives: vec![PrimitiveTimeSeries {
                    primitive_id: "test".into(),
                    dimmer: None,
                    position: Some(Series {
                        dim: 2,
                        labels: None,
                        samples: vec![
                            SeriesSample {
                                time: 0.0,
                                values: vec![0.0, 0.0],
                                label: None,
                            },
                            SeriesSample {
                                time: 4.0,
                                values: vec![100.0, 100.0],
                                label: None,
                            },
                        ],
                    }),
                    color: None,
                    strobe: None,
                    speed: None,
                }],
            },
        }];

        preposition_fixtures(&mut composited, &annotations, track_duration);

        // All samples should be unchanged because pattern is always active
        let pos = composited.primitives[0].position.as_ref().unwrap();
        assert_eq!(pos.samples[0].values, vec![0.0, 0.0]);
        assert_eq!(pos.samples[1].values, vec![25.0, 25.0]);
        assert_eq!(pos.samples[2].values, vec![50.0, 50.0]);
        assert_eq!(pos.samples[3].values, vec![75.0, 75.0]);
    }
}
