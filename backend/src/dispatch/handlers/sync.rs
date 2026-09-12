//! Host control and the sync status the sidebar reads.

use crate::dispatch::{AppServices, CommandError};
use crate::models::sync::SyncStatus;

/// The only command that is host control rather than app behavior.
pub async fn force_quit(services: &AppServices) -> Result<(), CommandError> {
    services.host.exit(0);
    Ok(())
}

/// What replication is doing, off the PowerSync client's own state.
///
/// A host that never started replication reports the default, which reads as
/// "not connected, nothing pending" — the truth for a local-only process.
pub async fn sync_status(services: &AppServices) -> Result<SyncStatus, CommandError> {
    let Some(sync) = &services.sync else {
        return Ok(SyncStatus::default());
    };
    sync.status().await.map_err(CommandError::Internal)
}
