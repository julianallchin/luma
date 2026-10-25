//! A venue's laser zones and its projectors' limits
//! (`docs/design/lasers.md`).

use crate::database::local::lasers as db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::handlers::fixtures::require_changed;
use crate::dispatch::{AppServices, CommandError};
use crate::models::lasers::{LaserLimit, LaserZone};

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

pub async fn list_laser_zones(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<LaserZone>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(db::list_zones(&mut access).await?)
}

/// Create a zone (no `zone_id`) or change one, as [`LaserZone::check`]
/// allows.
pub async fn save_laser_zone(
    services: &AppServices,
    venue_id: String,
    zone_id: Option<String>,
    name: String,
    turn: [f64; 2],
    tilt: [f64; 2],
) -> Result<LaserZone, CommandError> {
    let name = LaserZone::check(&name, turn, tilt).map_err(CommandError::Invalid)?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let zone = db::save_zone(&mut access, zone_id.as_deref(), name, turn, tilt)
        .await?
        .ok_or_else(|| CommandError::NotFound("No such laser zone in this venue".into()))?;
    access.commit().await?;
    Ok(zone)
}

pub async fn delete_laser_zone(
    services: &AppServices,
    venue_id: String,
    zone_id: String,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    require_changed(db::delete_zone(&mut access, &zone_id).await?)?;
    access.commit().await?;
    Ok(())
}

pub async fn list_laser_limits(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<LaserLimit>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(db::list_limits(&mut access).await?)
}

/// Set a projector's limit: three or more points, each coordinate −1 to 1.
/// `None` clears it, so the projector may send light anywhere in its field.
pub async fn set_laser_limit(
    services: &AppServices,
    venue_id: String,
    fixture_id: String,
    points: Option<Vec<[f64; 2]>>,
) -> Result<(), CommandError> {
    if let Some(points) = &points {
        let inside = |p: &[f64; 2]| p.iter().all(|c| c.is_finite() && (-1. ..=1.).contains(c));
        if points.len() < 3 || !points.iter().all(inside) {
            return Err(CommandError::Invalid(
                "A limit is three or more points, each coordinate -1 to 1".into(),
            ));
        }
    }
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    if !db::set_limit(&mut access, &fixture_id, points.as_deref()).await? {
        return Err(CommandError::NotFound(
            "No such fixture in this venue".into(),
        ));
    }
    access.commit().await?;
    Ok(())
}
