//! Host control and the sync status the sidebar reads.

use crate::dispatch::{AppServices, CommandError};
use crate::models::sync::SyncStatus;

/// The only command that is host control rather than app behavior.
pub async fn force_quit(services: &AppServices) -> Result<(), CommandError> {
    services.host.exit(0);
    Ok(())
}

/// Not connected. Phase two answers this off the PowerSync client's own state;
/// until then the app is local-only and there is nothing in flight to report.
pub async fn sync_status(_services: &AppServices) -> Result<SyncStatus, CommandError> {
    Ok(SyncStatus::default())
}
