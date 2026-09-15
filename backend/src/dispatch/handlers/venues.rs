//! Venues: the local library's rows.

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venues as venues_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::venues::Venue;
use luma_render::scene_desc::{VenueEnvironment, VenueHaze};

/// Every venue in the local library, owned and joined alike. Read-only and
/// unscoped — the per-venue authorization gate (`VenueAccess`) guards the
/// *contents* of a venue, not its existence in this list.
pub async fn list_venues(services: &AppServices) -> Result<Vec<Venue>, CommandError> {
    Ok(venues_db::list_venues(&services.db.0).await?)
}

/// Note the argument is `id`, not `venue_id` — the wire contract, inconsistent
/// with the rest of the surface but load-bearing.
pub async fn get_venue(services: &AppServices, id: String) -> Result<Venue, CommandError> {
    let mut access = VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&id)).await?;
    Ok(venues_db::get_venue(&mut access).await?)
}

/// The one venue write that does not open a `VenueAccess`: there is no venue
/// yet to take a lease on.
pub async fn create_venue(
    services: &AppServices,
    name: String,
    description: Option<String>,
) -> Result<Venue, CommandError> {
    Ok(venues_db::create_venue(&services.db.0, name, description).await?)
}

/// What kind of room this venue is, and how far up its one dial is.
///
/// Venue truth at the tier of the name, and synced with the venue since the
/// 20260905 migration: take the write lease, write the column, commit — the
/// ordinary venue dirtiness trigger carries the edit up. One write for both
/// modes because the value is one closed enum; see
/// [`venues_db::set_environment`].
pub async fn set_venue_environment(
    services: &AppServices,
    venue_id: String,
    environment: VenueEnvironment,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    venues_db::set_environment(&mut access, environment).await?;
    Ok(access.commit().await?)
}

/// How hazy this venue is, and what its haze looks like.
///
/// Venue truth beside the environment, and synced the same way: write lease,
/// column, commit. One write for the whole look; see [`venues_db::set_haze`].
pub async fn set_venue_haze(
    services: &AppServices,
    venue_id: String,
    haze: VenueHaze,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    venues_db::set_haze(&mut access, haze).await?;
    Ok(access.commit().await?)
}

// ============================================================================
// Helpers
// ============================================================================
