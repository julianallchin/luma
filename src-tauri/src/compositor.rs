//! Track Compositor
//!
//! Builds a [`Scene`] (one [`CompiledAnnotation`] per score row) for a
//! `(track, venue)` pair and installs it on the render engine. The Scene IS the
//! reusable compiled form — every annotation's pattern is lowered to an eval
//! [`Plan`] once, then `Scene::render` evaluates per-frame (cheap, seek-safe), so
//! the legacy precomputed-`LayerTimeSeries` + composite-cache machinery is gone.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tauri::{AppHandle, State};

use crate::audio::StemCache;
use crate::database::Db;
use crate::eval::context::build_resident_context;
use crate::eval::{compile::compile_pattern, CompiledAnnotation, Scene};
use crate::models::node_graph::{BeatGrid, Graph};
use crate::models::scores::TrackScore;
use crate::render_engine::RenderEngine;
use crate::storage::StorageRoot;

/// Monotonically increasing generation counter. Each `leave_track` bumps this so
/// any in-flight `composite_track` can detect it has gone stale.
static COMPOSITING_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Compiled plan per annotation id + a signature of the inputs that determine it
/// `(pattern, args, span, venue)`. Drives the **incremental composite**: a pass
/// reuses an annotation's plan when its signature is unchanged and recompiles only
/// the ones that actually changed — so tweaking one annotation's color recompiles
/// one plan, not the whole track. Also lets the preview generator reuse plans.
type PlanCacheEntry = (u64, std::sync::Arc<crate::eval::Plan>);
static PLAN_CACHE: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, PlanCacheEntry>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// The freshly-compiled plan for an annotation, if a recent `composite_track`
/// produced one. Lets previews skip the recompile + context rebuild.
pub fn cached_plan(annotation_id: &str) -> Option<std::sync::Arc<crate::eval::Plan>> {
    Some(PLAN_CACHE.lock().ok()?.get(annotation_id)?.1.clone())
}

/// Drop all cached plans (called on `leave_track`; positions/audio context may
/// differ for the next track, so plans must not carry over).
pub fn clear_plan_cache() {
    if let Ok(mut c) = PLAN_CACHE.lock() {
        c.clear();
    }
}

/// Signature of everything that determines an annotation's compiled plan. `z` and
/// `blend_mode` are excluded — they're Scene metadata, not plan inputs.
fn annotation_sig(annotation: &TrackScore, venue_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    annotation.pattern_id.hash(&mut h);
    venue_id.hash(&mut h);
    annotation.start_time.to_bits().hash(&mut h);
    annotation.end_time.to_bits().hash(&mut h);
    annotation.args.to_string().hash(&mut h);
    h.finish()
}

/// Bump the generation so any in-flight compositing aborts at its next check-point.
pub fn cancel_compositing() {
    COMPOSITING_GENERATION.fetch_add(1, Ordering::SeqCst);
}

pub(crate) async fn fetch_track_path_and_hash(
    pool: &sqlx::SqlitePool,
    track_id: &str,
) -> Result<(String, String), String> {
    let info = crate::database::local::tracks::get_track_path_and_hash(pool, track_id)
        .await
        .map_err(|e| format!("Failed to fetch track info: {}", e))?;
    Ok((info.file_path, info.track_hash))
}

/// Cancel compositing, clear the render engine's active scene, and unload audio.
/// Called when navigating away from the track editor.
#[tauri::command]
pub fn leave_track(
    render_engine: State<'_, RenderEngine>,
    host: State<'_, crate::host_audio::HostAudioState>,
    stem_cache: State<'_, StemCache>,
    track_id: String,
) {
    cancel_compositing();
    clear_plan_cache();
    render_engine.set_active_scene(None);
    host.unload();
    stem_cache.remove_track(&track_id);
}

/// Compile one score row into a [`CompiledAnnotation`] (context + plan + timeline
/// metadata). Returns `Ok(None)` for an empty graph (nothing to render).
#[allow(clippy::too_many_arguments)]
async fn compile_annotation(
    local_pool: &sqlx::SqlitePool,
    project_pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    annotation: &TrackScore,
    beat_grid: Option<BeatGrid>,
) -> Result<Option<CompiledAnnotation>, String> {
    let graph_json = fetch_pattern_graph(local_pool, &annotation.pattern_id).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;
    if graph.nodes.is_empty() {
        return Ok(None);
    }

    let mut args: std::collections::HashMap<String, Value> = annotation
        .args
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    // The annotation only carries arg *overrides*; fill unset args from the
    // pattern's own defaults (mirrors run_goldens / legacy
    // `arg_values.get(id).unwrap_or(default)`). Without this, a pattern-arg-fed
    // input (e.g. a gradient/stops) that the user didn't override fails to
    // resolve at compile. Assembled before the context build because the
    // selection pre-pass resolves arg-wired selections from this map.
    for ad in &graph.args {
        args.entry(ad.id.clone())
            .or_insert_with(|| ad.default_value.clone());
    }

    let span = (annotation.start_time as f32, annotation.end_time as f32);
    let (ctx, primitive_ids) = build_resident_context(
        local_pool,
        project_pool,
        storage,
        resource_root,
        track_id,
        venue_id,
        &graph.nodes,
        &graph.edges,
        &args,
        span,
        beat_grid,
    )
    .await;

    let plan =
        compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids).map_err(|e| {
            format!(
                "Failed to compile pattern {}: {:?}",
                annotation.pattern_id, e
            )
        })?;

    Ok(Some(CompiledAnnotation {
        plan: std::sync::Arc::new(plan),
        span,
        z_index: annotation.z_index,
        blend_mode: annotation.blend_mode,
    }))
}

/// Build a [`Scene`] for a `(track, venue)` from the given score rows.
pub(crate) async fn build_scene(
    local_pool: &sqlx::SqlitePool,
    project_pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    annotations: &[TrackScore],
) -> Result<Scene, String> {
    let beat_grid = load_beat_grid(local_pool, track_id).await?;
    let mut compiled = Vec::with_capacity(annotations.len());
    for annotation in annotations {
        let sig = annotation_sig(annotation, venue_id);
        let span = (annotation.start_time as f32, annotation.end_time as f32);

        // Incremental: reuse the cached plan when its inputs are unchanged.
        let hit = PLAN_CACHE.lock().ok().and_then(|c| {
            c.get(&annotation.id)
                .filter(|(s, _)| *s == sig)
                .map(|(_, p)| p.clone())
        });
        if let Some(plan) = hit {
            compiled.push(CompiledAnnotation {
                plan,
                span,
                z_index: annotation.z_index,
                blend_mode: annotation.blend_mode,
            });
            continue;
        }

        // One bad/unlowered pattern must not blank the whole track — skip it with
        // a warning and composite the rest (legacy tolerated this per-node).
        match compile_annotation(
            local_pool,
            project_pool,
            storage,
            resource_root,
            track_id,
            venue_id,
            annotation,
            beat_grid.clone(),
        )
        .await
        {
            Ok(Some(ann)) => {
                if let Ok(mut c) = PLAN_CACHE.lock() {
                    c.insert(annotation.id.clone(), (sig, ann.plan.clone()));
                }
                compiled.push(ann);
            }
            Ok(None) => {}
            Err(e) => log::warn!(
                "[composite] skipping annotation {} (pattern {}): {e}",
                annotation.id,
                annotation.pattern_id
            ),
        }
    }
    Ok(Scene::new(compiled))
}

/// Live annotation passed from the editor — its in-memory state, which during a
/// drag is *ahead* of the database (edits persist on a 300ms trailing edge). The
/// editor sends these so compositing uses live args instead of stale DB rows.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveAnnotation {
    pub id: String,
    pub pattern_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub z_index: i64,
    pub blend_mode: crate::models::node_graph::BlendMode,
    #[serde(default)]
    pub args: Value,
}

impl LiveAnnotation {
    fn into_track_score(self) -> TrackScore {
        TrackScore {
            id: self.id,
            uid: None,
            score_id: String::new(),
            pattern_id: self.pattern_id,
            start_time: self.start_time,
            end_time: self.end_time,
            z_index: self.z_index,
            blend_mode: self.blend_mode,
            args: self.args,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

/// Composite all patterns on a track into a [`Scene`] and install it as the
/// render engine's active scene.
#[tauri::command]
pub async fn composite_track(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, crate::audio::FftService>,
    track_id: String,
    venue_id: String,
    annotations: Option<Vec<LiveAnnotation>>,
    _skip_cache: Option<bool>,
) -> Result<(), String> {
    let generation = COMPOSITING_GENERATION.load(Ordering::SeqCst);

    // Prefer the editor's live annotations (fresh args mid-drag); fall back to the
    // DB for callers that don't pass them.
    let annotations: Vec<TrackScore> = match annotations {
        Some(live) if !live.is_empty() => live
            .into_iter()
            .map(LiveAnnotation::into_track_score)
            .collect(),
        _ => fetch_scores(&db.0, &track_id, &venue_id).await?,
    };
    if annotations.is_empty() {
        render_engine.set_active_scene(None);
        return Ok(());
    }

    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;
    let storage = StorageRoot::from_app(&app)?;

    let scene = build_scene(
        &db.0,
        &db.0,
        &storage,
        &resource_root,
        &track_id,
        &venue_id,
        &annotations,
    )
    .await?;

    // Bail if a newer leave_track / composite invalidated this pass.
    if COMPOSITING_GENERATION.load(Ordering::SeqCst) != generation {
        return Ok(());
    }

    if scene.is_empty() {
        render_engine.set_active_scene(None);
    } else {
        render_engine.set_active_scene(Some(scene));
    }
    Ok(())
}

/// Fetch annotations for a (track, venue) pair, sorted by z_index ascending.
pub(crate) async fn fetch_scores(
    pool: &sqlx::SqlitePool,
    track_id: &str,
    venue_id: &str,
) -> Result<Vec<TrackScore>, String> {
    crate::database::local::scores::get_scores_for_track(pool, track_id, venue_id)
        .await
        .map_err(|e| format!("Failed to fetch scores: {}", e))
}

/// Load beat grid for a track.
pub(crate) async fn load_beat_grid(
    pool: &sqlx::SqlitePool,
    track_id: &str,
) -> Result<Option<BeatGrid>, String> {
    crate::services::tracks::get_track_beats(pool, track_id)
        .await
        .map_err(|e| format!("Failed to load beat grid: {}", e))
}

/// Get track duration in seconds.
pub(crate) async fn get_track_duration(
    pool: &sqlx::SqlitePool,
    track_id: &str,
) -> Result<Option<f32>, String> {
    crate::database::local::tracks::get_track_duration(pool, track_id)
        .await
        .map(|opt| opt.map(|v| v as f32))
}

/// Fetch pattern graph JSON.
pub(crate) async fn fetch_pattern_graph(
    pool: &sqlx::SqlitePool,
    pattern_id: &str,
) -> Result<String, String> {
    crate::database::local::patterns::get_pattern_graph_pool(pool, pattern_id).await
}

// ── DSL Roundtrip Verification ─────────────────────────────────────────

/// A lightweight annotation input for DSL roundtrip verification.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VerifyAnnotation {
    pub pattern_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub z_index: i64,
    pub blend_mode: crate::models::node_graph::BlendMode,
    pub args: Value,
}

impl VerifyAnnotation {
    fn to_track_score(&self, idx: usize) -> TrackScore {
        TrackScore {
            id: format!("__verify_{}", idx),
            uid: None,
            score_id: String::new(),
            pattern_id: self.pattern_id.clone(),
            start_time: self.start_time,
            end_time: self.end_time,
            z_index: self.z_index,
            blend_mode: self.blend_mode,
            args: self.args.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

/// Result of comparing two composited scenes.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VerifyDslResult {
    pub pass: bool,
    pub message: String,
    pub diffs: Vec<VerifyPrimitiveDiff>,
    pub sample_count: usize,
    pub duration_ms: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VerifyPrimitiveDiff {
    pub primitive_id: String,
    pub max_dimmer_diff: f32,
    pub max_color_diff: f32,
    pub max_strobe_diff: f32,
    pub max_position_diff: f32,
    pub worst_time: f32,
}

/// Verify DSL roundtrip by compositing both the original (DB) and reimported
/// annotations into Scenes, then comparing rendered DMX output sample by sample.
#[tauri::command]
pub async fn verify_dsl_roundtrip(
    app: AppHandle,
    db: State<'_, Db>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, crate::audio::FftService>,
    track_id: String,
    venue_id: String,
    reimported: Vec<VerifyAnnotation>,
) -> Result<VerifyDslResult, String> {
    use crate::eval::{Arena, Scope};

    let verify_start = Instant::now();
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;
    let storage = StorageRoot::from_app(&app)?;

    let track_duration = get_track_duration(&db.0, &track_id).await?.unwrap_or(300.0);

    // Original scene from DB scores.
    let original_scores = fetch_scores(&db.0, &track_id, &venue_id).await?;
    let original_scene = build_scene(
        &db.0,
        &db.0,
        &storage,
        &resource_root,
        &track_id,
        &venue_id,
        &original_scores,
    )
    .await?;

    // Reimported scene from the passed annotations.
    let reimported_scores: Vec<TrackScore> = reimported
        .iter()
        .enumerate()
        .map(|(i, a)| a.to_track_score(i))
        .collect();
    let reimported_scene = build_scene(
        &db.0,
        &db.0,
        &storage,
        &resource_root,
        &track_id,
        &venue_id,
        &reimported_scores,
    )
    .await?;

    // Compare at 4 Hz over the track.
    let sample_rate = 4.0_f32;
    let num_samples = ((track_duration * sample_rate).ceil() as usize).max(2);
    let times: Vec<f32> = (0..num_samples)
        .map(|i| (i as f32 / (num_samples - 1) as f32) * track_duration)
        .collect();

    let mut arena = Arena::default();
    let orig_frames = original_scene.render(&times, Scope::Composite, &mut arena);
    let reim_frames = reimported_scene.render(&times, Scope::Composite, &mut arena);

    let all_prim_ids: HashSet<String> = orig_frames
        .iter()
        .chain(reim_frames.iter())
        .flat_map(|f| f.primitives.keys().cloned())
        .collect();

    let tolerance = 0.02_f32;
    let mut diffs: Vec<VerifyPrimitiveDiff> = Vec::new();
    let default = crate::models::universe::PrimitiveState {
        dimmer: 0.0,
        color: [0.0, 0.0, 0.0],
        strobe: 0.0,
        position: [0.0, 0.0],
        speed: 1.0,
    };

    for prim_id in &all_prim_ids {
        let mut max_dimmer = 0.0_f32;
        let mut max_color = 0.0_f32;
        let mut max_strobe = 0.0_f32;
        let mut max_position = 0.0_f32;
        let mut worst_time = 0.0_f32;
        let mut worst_diff = 0.0_f32;

        for (k, &time) in times.iter().enumerate() {
            let o = orig_frames[k].primitives.get(prim_id).unwrap_or(&default);
            let r = reim_frames[k].primitives.get(prim_id).unwrap_or(&default);

            let d_dim = (o.dimmer - r.dimmer).abs();
            let d_col = (o.color[0] - r.color[0])
                .abs()
                .max((o.color[1] - r.color[1]).abs())
                .max((o.color[2] - r.color[2]).abs());
            let d_str = (o.strobe - r.strobe).abs();
            let d_pos = (o.position[0] - r.position[0])
                .abs()
                .max((o.position[1] - r.position[1]).abs());

            let total = d_dim.max(d_col).max(d_str);
            if total > worst_diff {
                worst_diff = total;
                worst_time = time;
            }
            max_dimmer = max_dimmer.max(d_dim);
            max_color = max_color.max(d_col);
            max_strobe = max_strobe.max(d_str);
            max_position = max_position.max(d_pos);
        }

        if max_dimmer > tolerance || max_color > tolerance || max_strobe > tolerance {
            diffs.push(VerifyPrimitiveDiff {
                primitive_id: prim_id.clone(),
                max_dimmer_diff: max_dimmer,
                max_color_diff: max_color,
                max_strobe_diff: max_strobe,
                max_position_diff: max_position,
                worst_time,
            });
        }
    }

    let duration_ms = verify_start.elapsed().as_secs_f64() * 1000.0;
    let pass = diffs.is_empty();
    let message = if pass {
        format!(
            "DMX output matches: {} primitives × {} samples verified in {:.0}ms",
            all_prim_ids.len(),
            num_samples,
            duration_ms
        )
    } else {
        format!(
            "{} of {} primitives differ (sampled at {}Hz, {:.0}ms)",
            diffs.len(),
            all_prim_ids.len(),
            sample_rate,
            duration_ms
        )
    };

    Ok(VerifyDslResult {
        pass,
        message,
        diffs,
        sample_count: num_samples,
        duration_ms,
    })
}
