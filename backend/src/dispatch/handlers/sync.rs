//! Host control and the sync status the status bar reads.

use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::sync::{SyncProgress, SyncStatus};

/// What replication is doing, off the PowerSync client's own state, and the
/// media transfer beside it: bytes moving through storage are sync too.
///
/// A host that never started replication reports the default, which reads as
/// "not connected, nothing pending" — the truth for a local-only process.
pub async fn sync_status(services: &AppServices) -> Result<SyncStatus, CommandError> {
    let Some(sync) = &services.sync else {
        return Ok(SyncStatus::default());
    };
    let mut status = sync.status().await.map_err(CommandError::Internal)?;
    status.media = media(services).await?;
    Ok(status)
}

/// The media transfer in flight for the signed-in account, once it has found
/// files to move. Its discovery phases count nothing and are not reported.
async fn media(services: &AppServices) -> Result<Option<SyncProgress>, CommandError> {
    let Some(media) = &services.media else {
        return Ok(None);
    };
    let Some(uid) =
        crate::database::local::auth::get_current_user_id(services.state_pool()).await?
    else {
        return Ok(None);
    };
    Ok(media.progress(&uid).filter(|progress| {
        progress
            .total
            .is_some_and(|total| total > progress.completed)
    }))
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
