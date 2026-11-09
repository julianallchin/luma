//! Host control and the sync status the status bar reads.

use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::sync::SyncStatus;

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

/// The user has read the list of refused changes; stop counting them.
pub async fn dismiss_sync_refusals(services: &AppServices) -> Result<(), CommandError> {
    let Some(sync) = &services.sync else {
        return Ok(());
    };
    sync.dismiss_refusals()
        .await
        .map_err(CommandError::Internal)
}
