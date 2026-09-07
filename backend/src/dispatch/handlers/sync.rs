use crate::dispatch::{AppServices, CommandError};
use crate::sync::orchestrator::SyncReport;

/// The only command that is host control rather than app behavior.
pub async fn force_quit(services: &AppServices) -> Result<(), CommandError> {
    services.host.exit(0);
    Ok(())
}

/// Discovery → pull → files → push, serialized against the background loop by
/// the engine's own lock. `library-changed` is emitted mid-run, right after the
/// pull, so the UI refreshes without waiting for file transfers.
///
/// Partial failure is reported in `SyncReport::errors`, not as an error: a
/// pull that fails should not discard the push that succeeded.
pub async fn sync_full(services: &AppServices) -> Result<SyncReport, CommandError> {
    services
        .sync
        .sync_full(&services.sync_host())
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))
}

pub async fn sync_status(
    services: &AppServices,
) -> Result<crate::models::sync::SyncStatus, CommandError> {
    services
        .sync
        .status()
        .await
        .map_err(|e| CommandError::Internal(e.to_string()))
}

pub async fn sync_retry(services: &AppServices) -> Result<(), CommandError> {
    services
        .sync
        .retry()
        .await
        .map_err(|e| CommandError::Internal(e.to_string()))?;
    services
        .sync
        .sync_full(&services.sync_host())
        .await
        .map(|_| ())
        .map_err(|e| CommandError::Internal(e.to_string()))
}
