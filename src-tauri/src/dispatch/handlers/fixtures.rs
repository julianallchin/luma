use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::fixtures::PatchedFixture;
use crate::services::fixtures as fixture_service;

/// Pushing the patch to ArtNet is best-effort: the manager needs an `AppHandle`
/// to construct, so a headless host has none and the patch simply goes
/// unpublished. The optionality lives in the type rather than in a lookup that
/// can silently find nothing.
pub async fn get_patched_fixtures(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<PatchedFixture>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let fixtures = fixture_service::get_patched_fixtures(&mut access).await?;

    if let Some(artnet) = services.artnet.as_ref() {
        artnet.update_patch(fixtures.clone());
    }

    Ok(fixtures)
}
