//! A venue's laser rig (`docs/design/lasers.md`).

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};

/// The venue's projectors, posed, with their limits, and its zones: what
/// the visualizer solves each frame's beams with, as a DAC would.
pub async fn get_laser_rig(
    services: &AppServices,
    venue_id: String,
) -> Result<crate::laser::LaserRig, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    crate::laser::LaserRig::load(&mut access, &services.fixtures_root)
        .await
        .map_err(CommandError::from)
}
