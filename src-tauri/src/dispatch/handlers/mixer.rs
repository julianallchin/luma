use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venues as venues_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::mixer::{MixerMapping, MixerStatus};

// ── port listing ──────────────────────────────────────────────────────────────

/// The one mixer command with no venue lease — port enumeration reveals nothing
/// venue-scoped.
pub async fn mixer_list_ports(services: &AppServices) -> Result<Vec<String>, CommandError> {
    Ok(services.mixer.list_ports()?)
}

// ── connection ────────────────────────────────────────────────────────────────

/// Connect to a MIDI mixer port with the given CC mapping and persist the
/// config to the venue database so it survives restarts and crashes.
pub async fn mixer_connect(
    services: &AppServices,
    venue_id: String,
    port_name: String,
    mapping: MixerMapping,
) -> Result<(), CommandError> {
    // Serialize before taking the lease so a bad mapping fails without touching
    // the database.
    let mapping_json = serde_json::to_string(&mapping)
        .map_err(|e| CommandError::Invalid(format!("Failed to serialise mapping: {}", e)))?;

    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services
        .mixer
        .connect(&port_name, mapping, &services.events)?;
    venues_db::set_mixer_config(&mut access, Some(&port_name), Some(&mapping_json)).await?;
    Ok(access.commit().await?)
}

/// Disconnect the MIDI mixer and clear the saved config so it does not
/// auto-reconnect on next venue load.
pub async fn mixer_disconnect(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services.mixer.disconnect()?;
    venues_db::set_mixer_config(&mut access, None, None).await?;
    Ok(access.commit().await?)
}

// ── status ────────────────────────────────────────────────────────────────────

/// Returns current connection status and available port list.
/// Also triggers dead-connection detection and auto-reconnect; call every ~2 s.
pub async fn mixer_get_status(
    services: &AppServices,
    venue_id: String,
) -> Result<MixerStatus, CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.mixer.status())
}

// ── venue init (auto-reconnect) ───────────────────────────────────────────────

/// Called when a venue loads. Reads saved mixer config from the database and
/// seeds the manager's preferred config so auto-reconnect in `mixer_get_status`
/// can reconnect without user action.
pub async fn mixer_init_for_venue(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = venues_db::get_venue(&mut access).await?;

    // Unparseable stored mapping degrades to "no mapping" rather than blocking
    // venue load; the user re-learns it from the mixer dialog.
    let mapping: Option<MixerMapping> = match venue.mixer_mapping_json.as_deref() {
        Some(json) if !json.is_empty() => serde_json::from_str(json).ok(),
        _ => None,
    };

    services
        .mixer
        .set_preferred_config(venue.mixer_port, mapping, &services.events);
    Ok(())
}

/// Open a MIDI port temporarily without saving to DB — used during the learn
/// flow so CC messages can be captured before the user clicks Save.
pub async fn mixer_open_port(
    services: &AppServices,
    venue_id: String,
    port_name: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services
        .mixer
        .connect(&port_name, MixerMapping::default(), &services.events)?)
}

// ── learn ─────────────────────────────────────────────────────────────────────

/// Arm learn mode. The next CC message on the connected mixer port fires a
/// `mixer_learned { channel, cc }` event instead of being mapped.
pub async fn mixer_start_learn(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.mixer.start_learn(&services.events)?)
}

pub async fn mixer_cancel_learn(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.mixer.cancel_learn()?)
}
