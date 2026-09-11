//! Space-time heatmap previews: per-clip thumbnails for the timeline, and the
//! ad-hoc renders the pattern and graph agents look at.
//!
//! Every one is read-only and throws its scene away — none of them touch the
//! render engine's active scene.

use crate::annotation_preview as preview;
use crate::dispatch::{AppServices, CommandError};
use crate::models::node_graph::{BeatGrid, Graph};
use crate::models::patterns::AnnotationPreview;

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
