//! Space-time heatmap previews: per-clip thumbnails for the timeline, and the
//! ad-hoc renders the pattern and graph agents look at.
//!
//! Every one is read-only and throws its scene away — none of them touch the
//! render engine's active scene.

use crate::annotation_preview::{self as preview, LivePreviewInput};
use crate::dispatch::{AppServices, CommandError};
use crate::models::node_graph::{BeatGrid, Graph};
use crate::models::patterns::AnnotationPreview;

/// One annotation's preview, rendered from the live args the editor holds
/// mid-drag rather than the persisted row. Rendered alone (blend `Replace`,
/// z 0), so it is per-clip output, not composite output.
pub async fn preview_annotation(
    services: &AppServices,
    track_id: String,
    venue_id: String,
    annotation: LivePreviewInput,
) -> Result<AnnotationPreview, CommandError> {
    Ok(preview::preview_annotation(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &track_id,
        &venue_id,
        annotation,
    )
    .await?)
}

/// Every persisted annotation's preview for `(track_id, venue_id)`, in z-index
/// order. Empty when the track has no annotations here.
pub async fn generate_annotation_previews(
    services: &AppServices,
    track_id: String,
    venue_id: String,
) -> Result<Vec<AnnotationPreview>, CommandError> {
    Ok(preview::generate_annotation_previews(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &track_id,
        &venue_id,
    )
    .await?)
}

/// A saved pattern's output over a span, with Selection args forced to `all`.
pub async fn preview_pattern_image(
    services: &AppServices,
    pattern_id: String,
    track_id: String,
    venue_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<AnnotationPreview, CommandError> {
    Ok(preview::preview_pattern_image(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &pattern_id,
        &track_id,
        &venue_id,
        start_time,
        end_time,
        beat_grid,
    )
    .await?)
}

/// [`preview_pattern_image`] for an unsaved graph — the graph-editor agent's
/// "look at my edit" tool.
pub async fn preview_graph_image(
    services: &AppServices,
    graph: Graph,
    track_id: String,
    venue_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<AnnotationPreview, CommandError> {
    Ok(preview::preview_graph_image(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &graph,
        &track_id,
        &venue_id,
        start_time,
        end_time,
        beat_grid,
    )
    .await?)
}

/// The blended composite of every annotation on a track. No venue argument —
/// the venue is inferred from the track's accessible score.
pub async fn view_composite_image(
    services: &AppServices,
    track_id: String,
    start_time: f32,
    end_time: f32,
) -> Result<AnnotationPreview, CommandError> {
    Ok(preview::view_composite_image(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &track_id,
        start_time,
        end_time,
    )
    .await?)
}
