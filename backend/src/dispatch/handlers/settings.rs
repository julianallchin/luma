//! Application settings: a string key/value table plus the live effects a
//! write has on the two subsystems that read it.

use crate::database::local::settings as settings_db;
use crate::dispatch::{AppServices, CommandError};
use crate::settings::{load_settings, AppSettings};

/// Every setting, typed and defaulted.
pub async fn get_settings(services: &AppServices) -> Result<AppSettings, CommandError> {
    Ok(load_settings(&services.db.0).await?)
}

/// Write one setting and apply it immediately.
///
/// Keys are open strings: the valid set exists only in [`load_settings`]'s
/// parser, so a typo writes a row nothing ever reads. Applying is
/// [`AppServices::apply_persisted_settings`] — the same call a host makes at
/// startup, so a write and a fresh process put the subsystems in the same
/// state.
pub async fn set_setting(
    services: &AppServices,
    key: String,
    value: String,
) -> Result<(), CommandError> {
    settings_db::update_setting(&services.db.0, &key, &value).await?;
    services.apply_persisted_settings().await
}
