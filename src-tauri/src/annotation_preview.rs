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
use crate::compositor::{build_scene, fetch_pattern_graph, fetch_scores, load_beat_grid};
use crate::database::Db;
use crate::eval::context::build_resident_context;
use crate::eval::{compile::compile_pattern, Arena, Scene, Scope};
use crate::models::node_graph::{BeatGrid, Graph};
use crate::models::patterns::AnnotationPreview;
use crate::models::universe::UniverseState;

/// Columns per beat in the preview thumbnail
const STEPS_PER_BEAT: u32 = 16;
const MIN_PREVIEW_WIDTH: u32 = 8;
const MAX_PREVIEW_WIDTH: u32 = 512;
const MAX_PREVIEW_HEIGHT: u32 = 32;

pub(crate) struct CachedPreview {
    pub(crate) preview: AnnotationPreview,
}

pub(crate) static PREVIEW_CACHE: Lazy<Mutex<HashMap<String, CachedPreview>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Column time axis for a preview over `[start, end]`.
fn preview_times(beat_grid: Option<&BeatGrid>, start_time: f32, end_time: f32) -> Vec<f32> {
    let width = compute_preview_width(beat_grid, start_time, end_time);
    let span = end_time - start_time;
    let divisor = (width - 1).max(1) as f32;
    (0..width)
        .map(|col| start_time + (col as f32 / divisor) * span)
        .collect()
}

/// Compile + eval one pattern (Selection args forced to a provided arg map) over
/// the preview column grid → one [`UniverseState`] per column.
#[allow(clippy::too_many_arguments)]
async fn eval_pattern_frames(
    local_pool: &sqlx::SqlitePool,
    project_pool: &sqlx::SqlitePool,
    resource_root: &std::path::Path,
    track_id: &str,
    venue_id: &str,
    graph: &Graph,
    args: &HashMap<String, serde_json::Value>,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
    times: &[f32],
) -> Result<Vec<UniverseState>, String> {
    // Fill unset args from the pattern defaults (annotations carry only
    // overrides) before the context build — the selection pre-pass resolves
    // arg-wired selections from this map.
    let mut args = args.clone();
    for ad in &graph.args {
        args.entry(ad.id.clone())
            .or_insert_with(|| ad.default_value.clone());
    }
    let (ctx, primitive_ids) = build_resident_context(
        local_pool,
        project_pool,
        resource_root,
        track_id,
        venue_id,
        &graph.nodes,
        &graph.edges,
        &args,
        (start_time, end_time),
        beat_grid,
    )
    .await;
    let plan = compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids)
        .map_err(|e| format!("Failed to compile pattern: {:?}", e))?;
    let scene = Scene::new(vec![crate::eval::CompiledAnnotation {
        plan: std::sync::Arc::new(plan),
        span: (start_time, end_time),
        z_index: 0,
        blend_mode: crate::models::node_graph::BlendMode::Replace,
    }]);
    let mut arena = Arena::default();
    Ok(scene.render(times, Scope::Single(0), &mut arena))
}

/// One annotation's live editor state for a targeted preview regen (live args,
/// ahead of the DB during a drag).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePreviewInput {
    pub id: String,
    pub pattern_id: String,
    pub start_time: f32,
    pub end_time: f32,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Regenerate the heatmap preview for a SINGLE annotation using its live args.
/// The editor calls this during a drag so the edited clip's thumbnail updates in
/// real time, instead of waiting for the debounced full regeneration.
#[tauri::command]
pub async fn preview_annotation(
    app: AppHandle,
    db: State<'_, Db>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, FftService>,
    track_id: String,
    venue_id: String,
    annotation: LivePreviewInput,
) -> Result<AnnotationPreview, String> {
    let beat_grid = load_beat_grid(&db.0, &track_id).await?;
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    let graph_json = fetch_pattern_graph(&db.0, &annotation.pattern_id).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;
    if graph.nodes.is_empty() {
        return Ok(empty_preview(annotation.id));
    }

    let (start, end) = (annotation.start_time, annotation.end_time);
    let times = preview_times(beat_grid.as_ref(), start, end);
    let args: HashMap<String, serde_json::Value> = annotation
        .args
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let frames = eval_pattern_frames(
        &db.0,
        &db.0,
        &resource_root,
        &track_id,
        &venue_id,
        &graph,
        &args,
        start,
        end,
        beat_grid.clone(),
        &times,
    )
    .await?;
    let preview = render_preview(
        annotation.id.clone(),
        &frames,
        beat_grid.as_ref(),
        start,
        end,
    );
    PREVIEW_CACHE
        .lock()
        .expect("preview cache mutex poisoned")
        .insert(
            annotation.id.clone(),
            CachedPreview {
                preview: preview.clone(),
            },
        );
    Ok(preview)
}

#[tauri::command]
pub async fn generate_annotation_previews(
    app: AppHandle,
    db: State<'_, Db>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, FftService>,
    track_id: String,
    venue_id: String,
) -> Result<Vec<AnnotationPreview>, String> {
    let gen_start = Instant::now();

    let annotations = fetch_scores(&db.0, &track_id, &venue_id).await?;
    if annotations.is_empty() {
        return Ok(vec![]);
    }

    let beat_grid = load_beat_grid(&db.0, &track_id).await?;
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    let mut previews = Vec::with_capacity(annotations.len());
    let mut generated = 0usize;

    for annotation in &annotations {
        let graph_json = fetch_pattern_graph(&db.0, &annotation.pattern_id).await?;
        let graph: Graph = serde_json::from_str(&graph_json)
            .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;

        if graph.nodes.is_empty() {
            previews.push(empty_preview(annotation.id.clone()));
            continue;
        }

        let start = annotation.start_time as f32;
        let end = annotation.end_time as f32;
        let times = preview_times(beat_grid.as_ref(), start, end);
        let args: HashMap<String, serde_json::Value> = annotation
            .args
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();

        let frames = eval_pattern_frames(
            &db.0,
            &db.0,
            &resource_root,
            &track_id,
            &venue_id,
            &graph,
            &args,
            start,
            end,
            beat_grid.clone(),
            &times,
        )
        .await?;

        let preview = render_preview(
            annotation.id.clone(),
            &frames,
            beat_grid.as_ref(),
            start,
            end,
        );

        {
            let mut cache = PREVIEW_CACHE.lock().expect("preview cache mutex poisoned");
            cache.insert(
                annotation.id.clone(),
                CachedPreview {
                    preview: preview.clone(),
                },
            );
        }
        generated += 1;
        previews.push(preview);
    }

    let total_ms = gen_start.elapsed().as_secs_f64() * 1000.0;
    log::info!(
        "[annotation_preview] track={} annotations={} generated={} total_ms={:.2}",
        track_id,
        annotations.len(),
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

fn empty_preview(annotation_id: String) -> AnnotationPreview {
    AnnotationPreview {
        annotation_id,
        width: 1,
        height: 1,
        pixels: vec![0, 0, 0, 0],
        dominant_color: [0.0; 3],
    }
}

/// Compute preview width from beat grid: count beats in [start, end), multiply by STEPS_PER_BEAT.
/// Falls back to duration-based estimate if no beat grid.
fn compute_preview_width(beat_grid: Option<&BeatGrid>, start_time: f32, end_time: f32) -> u32 {
    let duration = end_time - start_time;
    let beat_count = if let Some(bg) = beat_grid {
        let count = bg
            .beats
            .iter()
            .filter(|&&b| b >= start_time && b < end_time)
            .count() as u32;
        if count > 0 {
            count
        } else {
            let bps = bg.bpm / 60.0;
            (duration * bps).round().max(1.0) as u32
        }
    } else {
        (duration * 2.0).round().max(1.0) as u32
    };

    (beat_count * STEPS_PER_BEAT).clamp(MIN_PREVIEW_WIDTH, MAX_PREVIEW_WIDTH)
}

/// Render a heatmap from a column-sampled grid of [`UniverseState`] frames
/// (`frames[col]` = the state at preview column `col`). Rows are primitives,
/// ordered by brightness-weighted center of mass in time so spatial patterns
/// read as diagonals.
pub(crate) fn render_preview(
    annotation_id: String,
    frames: &[UniverseState],
    beat_grid: Option<&BeatGrid>,
    start_time: f32,
    end_time: f32,
) -> AnnotationPreview {
    let _ = (beat_grid, start_time, end_time); // width is implied by frames.len()
    let width = frames.len() as u32;
    if width == 0 {
        return empty_preview(annotation_id);
    }

    // Collect the set of primitive ids across all columns.
    let mut prim_ids: Vec<String> = {
        let mut set: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for f in frames {
            for k in f.primitives.keys() {
                set.insert(k.as_str());
            }
        }
        set.into_iter().map(|s| s.to_string()).collect()
    };
    if prim_ids.is_empty() {
        return empty_preview(annotation_id);
    }

    // Brightness-weighted center of mass per primitive.
    prim_ids.sort_by(|a, b| {
        let com = |id: &str| -> f64 {
            let mut weighted = 0.0f64;
            let mut total = 0.0f64;
            for (col, f) in frames.iter().enumerate() {
                let d = f.primitives.get(id).map(|p| p.dimmer).unwrap_or(0.0) as f64;
                weighted += col as f64 * d;
                total += d;
            }
            if total > 0.0 {
                weighted / total
            } else {
                f64::MAX
            }
        };
        com(a)
            .partial_cmp(&com(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let height = (prim_ids.len() as u32).min(MAX_PREVIEW_HEIGHT);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let mut color_sum = [0.0f64; 3];
    let mut weight_sum = 0.0f64;

    for (row, prim_id) in prim_ids.iter().take(height as usize).enumerate() {
        for col in 0..width as usize {
            let frame = &frames[col];
            let (color, dimmer) = frame
                .primitives
                .get(prim_id)
                .map(|p| (p.color, p.dimmer))
                .unwrap_or(([1.0, 1.0, 1.0], 0.0));

            let r = (color[0] * dimmer * 255.0).clamp(0.0, 255.0) as u8;
            let g = (color[1] * dimmer * 255.0).clamp(0.0, 255.0) as u8;
            let b = (color[2] * dimmer * 255.0).clamp(0.0, 255.0) as u8;

            let idx = ((row as u32 * width + col as u32) * 4) as usize;
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

/// Build the default arg map for a pattern graph, forcing Selection args to "all".
fn preview_arg_values(graph: &Graph) -> HashMap<String, serde_json::Value> {
    graph
        .args
        .iter()
        .map(|arg| {
            let value = match arg.arg_type {
                crate::models::node_graph::PatternArgType::Selection => {
                    serde_json::json!({ "expression": "all", "spatialReference": "global" })
                }
                _ => arg.default_value.clone(),
            };
            (arg.id.clone(), value)
        })
        .collect()
}

/// Render a heatmap preview for a single pattern over a time range, without
/// placing it on the timeline. Args use the pattern's defaults (Selection args
/// resolve to `all`).
#[tauri::command]
pub async fn preview_pattern_image(
    app: AppHandle,
    db: State<'_, Db>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, FftService>,
    pattern_id: String,
    track_id: String,
    venue_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<AnnotationPreview, String> {
    if end_time <= start_time {
        return Err("end_time must be greater than start_time".into());
    }

    let graph_json = fetch_pattern_graph(&db.0, &pattern_id).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    let args = preview_arg_values(&graph);
    let times = preview_times(beat_grid.as_ref(), start_time, end_time);
    let frames = eval_pattern_frames(
        &db.0,
        &db.0,
        &resource_root,
        &track_id,
        &venue_id,
        &graph,
        &args,
        start_time,
        end_time,
        beat_grid.clone(),
        &times,
    )
    .await?;

    Ok(render_preview(
        format!("preview_{pattern_id}"),
        &frames,
        beat_grid.as_ref(),
        start_time,
        end_time,
    ))
}

/// Render a heatmap preview of an *unsaved* graph over a time range. Identical
/// to `preview_pattern_image` but takes the graph inline instead of fetching it
/// by pattern id — this is how the graph-editor agent "sees" the output of an
/// edit before it's saved.
#[tauri::command]
pub async fn preview_graph_image(
    app: AppHandle,
    db: State<'_, Db>,
    _stem_cache: State<'_, StemCache>,
    _fft_service: State<'_, FftService>,
    graph: Graph,
    track_id: String,
    venue_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<AnnotationPreview, String> {
    if end_time <= start_time {
        return Err("end_time must be greater than start_time".into());
    }

    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    let args = preview_arg_values(&graph);
    let times = preview_times(beat_grid.as_ref(), start_time, end_time);
    let frames = eval_pattern_frames(
        &db.0,
        &db.0,
        &resource_root,
        &track_id,
        &venue_id,
        &graph,
        &args,
        start_time,
        end_time,
        beat_grid.clone(),
        &times,
    )
    .await?;

    Ok(render_preview(
        "preview_graph".to_string(),
        &frames,
        beat_grid.as_ref(),
        start_time,
        end_time,
    ))
}

/// Render a heatmap preview of the *composited* track output over a time range.
/// Builds the Scene fresh from the DB scores and renders the composite.
#[tauri::command]
pub async fn view_composite_image(
    app: AppHandle,
    db: State<'_, Db>,
    track_id: String,
    start_time: f32,
    end_time: f32,
) -> Result<AnnotationPreview, String> {
    if end_time <= start_time {
        return Err("end_time must be greater than start_time".into());
    }

    let venue_id = crate::database::local::scores::get_venue_for_track(&db.0, &track_id)
        .await?
        .ok_or_else(|| "No score with annotations for this track.".to_string())?;
    let annotations = fetch_scores(&db.0, &track_id, &venue_id).await?;
    if annotations.is_empty() {
        return Err("No annotations for this track/venue.".into());
    }
    let beat_grid = load_beat_grid(&db.0, &track_id).await?;
    let resource_root = crate::services::fixtures::resolve_fixtures_root(&app)
        .map_err(|e| format!("Failed to resolve fixtures root: {}", e))?;

    let scene = build_scene(
        &db.0,
        &db.0,
        &resource_root,
        &track_id,
        &venue_id,
        &annotations,
    )
    .await?;

    let times = preview_times(beat_grid.as_ref(), start_time, end_time);
    let mut arena = Arena::default();
    let frames = scene.render(&times, Scope::Composite, &mut arena);

    Ok(render_preview(
        format!("composite_{track_id}"),
        &frames,
        beat_grid.as_ref(),
        start_time,
        end_time,
    ))
}
