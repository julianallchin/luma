use tauri::{AppHandle, State};

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venues as venues_db;
use crate::database::Db;
use crate::mixer_manager::MixerManager;
use crate::models::mixer::{MixerMapping, MixerStatus};

// ── port listing ──────────────────────────────────────────────────────────────

#[tauri::command]
pub fn mixer_list_ports(mixer: State<'_, MixerManager>) -> Result<Vec<String>, String> {
    mixer.list_ports()
}

// ── connection ────────────────────────────────────────────────────────────────

/// Connect to a MIDI mixer port with the given CC mapping and persist the
/// config to the venue database so it survives restarts and crashes.
#[tauri::command]
pub async fn mixer_connect(
    app: AppHandle,
    mixer: State<'_, MixerManager>,
    db: State<'_, Db>,
    venue_id: String,
    port_name: String,
    mapping: MixerMapping,
) -> Result<(), String> {
    let mapping_json = serde_json::to_string(&mapping)
        .map_err(|e| format!("Failed to serialise mapping: {}", e))?;

    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    mixer.connect(&port_name, mapping, app)?;
    venues_db::set_mixer_config(&mut access, Some(&port_name), Some(&mapping_json)).await?;
    access.commit().await
}

/// Disconnect the MIDI mixer and clear the saved config so it does not
/// auto-reconnect on next venue load.
#[tauri::command]
pub async fn mixer_disconnect(
    mixer: State<'_, MixerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    mixer.disconnect()?;
    venues_db::set_mixer_config(&mut access, None, None).await?;
    access.commit().await
}

// ── status ────────────────────────────────────────────────────────────────────

/// Returns current connection status and available port list.
/// Also triggers dead-connection detection and auto-reconnect; call every ~2 s.
#[tauri::command]
pub async fn mixer_get_status(
    mixer: State<'_, MixerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<MixerStatus, String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(mixer.status())
}

// ── venue init (auto-reconnect) ───────────────────────────────────────────────

/// Called when a venue loads. Reads saved mixer config from the database and
/// seeds the manager's preferred config so auto-reconnect in `mixer_get_status`
/// can reconnect without user action.
#[tauri::command]
pub async fn mixer_init_for_venue(
    app: AppHandle,
    mixer: State<'_, MixerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = venues_db::get_venue(&mut access).await?;

    let mapping: Option<MixerMapping> = match venue.mixer_mapping_json.as_deref() {
        Some(json) if !json.is_empty() => serde_json::from_str(json).ok(),
        _ => None,
    };

    mixer.set_preferred_config(venue.mixer_port, mapping, app);
    Ok(())
}

/// Open a MIDI port temporarily without saving to DB — used during the learn
/// flow so CC messages can be captured before the user clicks Save.
#[tauri::command]
pub async fn mixer_open_port(
    app: AppHandle,
    mixer: State<'_, MixerManager>,
    db: State<'_, Db>,
    venue_id: String,
    port_name: String,
) -> Result<(), String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    mixer.connect(&port_name, MixerMapping::default(), app)
}

// ── learn ─────────────────────────────────────────────────────────────────────

/// Arm learn mode. The next CC message on the connected mixer port fires a
/// `mixer_learned { channel, cc }` Tauri event instead of being mapped.
#[tauri::command]
pub async fn mixer_start_learn(
    app: AppHandle,
    mixer: State<'_, MixerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    mixer.start_learn(app)
}

#[tauri::command]
pub async fn mixer_cancel_learn(
    mixer: State<'_, MixerManager>,
    db: State<'_, Db>,
    venue_id: String,
) -> Result<(), String> {
    let _access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    mixer.cancel_learn()
}
