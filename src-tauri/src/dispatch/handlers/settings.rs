//! Application settings: a string key/value table plus the live effects a
//! write has on the two subsystems that read it.

use crate::database::local::settings as settings_db;
use crate::dispatch::{AppServices, CommandError};
use crate::host_audio;
use crate::settings::{load_settings, AppSettings};

/// Every setting, typed and defaulted.
pub async fn get_settings(services: &AppServices) -> Result<AppSettings, CommandError> {
    Ok(load_settings(&services.db.0).await?)
}

/// Write one setting and apply it immediately.
///
/// Keys are open strings: the valid set exists only in [`load_settings`]'s
/// parser, so a typo writes a row nothing ever reads. Art-Net reload is skipped
/// on a host with no manager (see [`AppServices::artnet`]); the audio reload
/// always runs, and is inert on a host whose playback state drives nothing.
pub async fn set_setting(
    services: &AppServices,
    key: String,
    value: String,
) -> Result<(), CommandError> {
    let pool = &services.db.0;
    settings_db::update_setting(pool, &key, &value).await?;

    if let Some(artnet) = &services.artnet {
        crate::artnet::reload_settings(artnet, pool).await?;
    }
    host_audio::reload_settings(&services.host_audio, pool).await?;

    Ok(())
}
