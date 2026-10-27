use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::composable_patterns::{ComposablePreview, ComposablePreviewRequest};
use crate::models::selection::Selection;
use luma_patterns::Cell;

pub async fn preview_composable_pattern(
    services: &AppServices,
    request: ComposablePreviewRequest,
) -> Result<ComposablePreview, CommandError> {
    Ok(crate::services::composable_patterns::preview(
        &services.db.0,
        &services.fixtures_root,
        &services.storage,
        request,
    )
    .await?)
}

/// The heads a clip's selection resolves to in the venue, with their places:
/// what a form maps along an axis. `seed` is the clip's selection seed;
/// `elements` a laser clip's elements per projector.
pub async fn selection_cells(
    services: &AppServices,
    venue_id: String,
    selection: Selection,
    seed: u64,
    elements: Option<usize>,
) -> Result<Vec<Cell>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(crate::services::composable_patterns::resolve_cells(
        &mut access,
        &services.fixtures_root,
        std::slice::from_ref(&selection),
        seed,
        elements,
    )
    .await?)
}
