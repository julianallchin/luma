use crate::database::local::venues as venues_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::venues::Venue;

/// Every venue in the local library, owned and joined alike. Read-only and
/// unscoped — the per-venue authorization gate (`VenueAccess`) guards the
/// *contents* of a venue, not its existence in this list.
pub async fn list_venues(services: &AppServices) -> Result<Vec<Venue>, CommandError> {
    Ok(venues_db::list_venues(&services.db.0).await?)
}
