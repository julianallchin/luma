use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::composable_patterns::ComposablePreviewRequest;
use crate::models::selection::Selection;
use luma_patterns::Cell;
use serde_json::Value;

pub async fn preview_composable_pattern(
    services: &AppServices,
    request: Value,
) -> Result<Value, CommandError> {
    let request: ComposablePreviewRequest = serde_json::from_value(request)
        .map_err(|error| CommandError::Invalid(error.to_string()))?;
    let result = crate::services::composable_patterns::preview(
        &services.db.0,
        &services.fixtures_root,
        &services.storage,
        request,
    )
    .await?;
    serde_json::to_value(result).map_err(|error| CommandError::Internal(error.to_string()))
}

/// The heads a clip's selection resolves to in the venue, with their places:
/// what a form maps along an axis. `seed` is the clip's selection seed.
pub async fn selection_cells(
    services: &AppServices,
    venue_id: String,
    selection: Selection,
    seed: u64,
) -> Result<Vec<Cell>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(crate::services::composable_patterns::resolve_cells(
        &mut access,
        &services.fixtures_root,
        std::slice::from_ref(&selection),
        seed,
    )
    .await?)
}
