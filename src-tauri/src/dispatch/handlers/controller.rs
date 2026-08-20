use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venues as venues_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::midi::{ControllerState, ControllerStatus};

// ============================================================================
// Device Connection
// ============================================================================

pub async fn controller_connect(
    services: &AppServices,
    port_name: String,
    venue_id: String,
) -> Result<(), CommandError> {
    // Ordering is the contract: take the write lease, then open the port, then
    // record it. A failed connect aborts before anything is persisted.
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services.controller.connect(&port_name, &services.events)?;
    venues_db::set_controller_port(&mut access, Some(&port_name)).await?;
    Ok(access.commit().await?)
}

pub async fn controller_disconnect(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services.controller.disconnect()?;
    venues_db::set_controller_port(&mut access, None).await?;
    Ok(access.commit().await?)
}

pub async fn controller_get_status(
    services: &AppServices,
    venue_id: String,
) -> Result<ControllerStatus, CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.controller.status())
}

/// Called when a venue loads. Restores the saved preferred port so
/// auto-reconnect works without the user opening the controller config.
pub async fn controller_init_for_venue(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = venues_db::get_venue(&mut access).await?;
    services
        .controller
        .set_preferred_port(venue.controller_port, &services.events);
    Ok(())
}

// ============================================================================
// Learn Mode
// ============================================================================

pub async fn controller_start_learn(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.controller.start_learn(&services.events)?)
}

pub async fn controller_cancel_learn(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.controller.cancel_learn()?)
}

// ============================================================================
// Manual Layer State
// ============================================================================
//
// The manual layer is render state, not controller state, so these two go
// through `RenderEngine` despite the `controller_` wire prefix.

pub async fn controller_set_active(
    services: &AppServices,
    venue_id: String,
    active: bool,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services.render_engine.set_manual_active(active);
    Ok(())
}

pub async fn controller_get_state(
    services: &AppServices,
    venue_id: String,
) -> Result<ControllerState, CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.render_engine.get_manual_state_snapshot())
}
