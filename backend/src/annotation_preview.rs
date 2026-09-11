//! Annotation Preview Generator
//!
//! Generates space-time heatmap thumbnails for timeline annotations.
//! Each preview is a small RGBA image where rows = fixtures, columns = time steps,
//! and pixel color = fixture RGB × dimmer.

use std::collections::HashMap;

use crate::compositor::fetch_pattern_graph;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::eval::context::build_resident_context;
use crate::eval::{compile::compile_pattern, Arena, Scene, Scope};
use crate::models::node_graph::{BeatGrid, Graph};
use crate::models::patterns::AnnotationPreview;
use crate::models::universe::UniverseState;
use crate::storage::StorageRoot;

/// Columns per beat in the preview thumbnail
const STEPS_PER_BEAT: u32 = 16;
const MIN_PREVIEW_WIDTH: u32 = 8;
const MAX_PREVIEW_WIDTH: u32 = 512;
const MAX_PREVIEW_HEIGHT: u32 = 32;

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
    storage: &StorageRoot,
    resource_root: &std::path::Path,
    track_id: &str,
    venue_id: &str,
    instance: Option<&str>,
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
        storage,
        resource_root,
        track_id,
        venue_id,
        instance,
        &graph.nodes,
        &graph.edges,
        &args,
        (start_time, end_time),
        beat_grid,
    )
    .await;
    let plan = compile_pattern(&graph, &args, ctx, primitive_ids)
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
/// read as diagonals; past [`MAX_PREVIEW_HEIGHT`] heads, a row is the
/// brightest head of a band of neighbours in that order.
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

    // A rig with more heads than rows shares each row between a band of
    // neighbours in that order and shows the brightest of them, so no head is
    // dropped from the strip.
    let band = |row: usize| {
        let (n, h) = (prim_ids.len(), height as usize);
        &prim_ids[row * n / h..(row + 1) * n / h]
    };
    for row in 0..height as usize {
        for col in 0..width as usize {
            let frame = &frames[col];
            let (color, dimmer) = band(row)
                .iter()
                .filter_map(|id| frame.primitives.get(id).map(|p| (p.color, p.dimmer)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
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
                    crate::models::selection::Selection::all().to_value()
                }
                _ => arg.default_value.clone(),
            };
            (arg.id.clone(), value)
        })
        .collect()
}

/// Render a heatmap preview for a single pattern over a time range, without
/// placing it on the timeline. Args are the pattern's own defaults, with every
/// Selection arg forced to `all` — so this is not what the timeline clip would
/// render.
#[allow(clippy::too_many_arguments)]
pub async fn preview_pattern_image(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &std::path::Path,
    pattern_id: &str,
    track_id: &str,
    venue_id: &str,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<AnnotationPreview, String> {
    if end_time <= start_time {
        return Err("end_time must be greater than start_time".into());
    }
    let _venue_access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id)).await?;

    let graph_json = fetch_pattern_graph(pool, pattern_id, Some(venue_id)).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph: {}", e))?;

    let args = preview_arg_values(&graph);
    let times = preview_times(beat_grid.as_ref(), start_time, end_time);
    let frames = eval_pattern_frames(
        pool,
        pool,
        storage,
        resource_root,
        track_id,
        venue_id,
        None,
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

/// [`preview_pattern_image`] for an *unsaved* graph — the graph arrives inline
/// instead of being fetched by pattern id, which is how the graph-editor agent
/// sees the output of an edit before it is saved.
#[allow(clippy::too_many_arguments)]
pub async fn preview_graph_image(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &std::path::Path,
    graph: &Graph,
    track_id: &str,
    venue_id: &str,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<AnnotationPreview, String> {
    if end_time <= start_time {
        return Err("end_time must be greater than start_time".into());
    }
    let _venue_access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id)).await?;

    let args = preview_arg_values(graph);
    let times = preview_times(beat_grid.as_ref(), start_time, end_time);
    let frames = eval_pattern_frames(
        pool,
        pool,
        storage,
        resource_root,
        track_id,
        venue_id,
        None,
        graph,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::universe::PrimitiveState;

    fn frame(lit: impl Iterator<Item = usize>) -> UniverseState {
        let mut primitives = HashMap::new();
        for head in 0..64 {
            primitives.insert(
                format!("head-{head:02}"),
                PrimitiveState {
                    dimmer: 0.0,
                    color: [0.0, 1.0, 0.0],
                    strobe: 0.0,
                    position: [0.0; 2],
                    speed: 1.0,
                },
            );
        }
        for head in lit {
            primitives
                .get_mut(&format!("head-{head:02}"))
                .unwrap()
                .dimmer = 1.0;
        }
        UniverseState { primitives }
    }

    #[test]
    fn every_head_reaches_the_strip_when_there_are_more_heads_than_rows() {
        // Half the rig lights only at the start, the other half only at the
        // end. A strip that kept the 32 earliest heads would go dark after
        // the first column.
        let mut frames = vec![frame(0..32)];
        frames.extend((0..30).map(|_| frame(std::iter::empty())));
        frames.push(frame(32..64));
        let preview = render_preview("clip".into(), &frames, None, 0.0, 1.0);
        assert_eq!((preview.width, preview.height), (32, MAX_PREVIEW_HEIGHT));
        let green_at = |col: u32| {
            (0..preview.height)
                .map(|row| preview.pixels[((row * preview.width + col) * 4 + 1) as usize])
                .max()
                .unwrap()
        };
        assert_eq!(green_at(0), 255);
        assert_eq!(green_at(15), 0);
        assert_eq!(green_at(31), 255);
    }
}
