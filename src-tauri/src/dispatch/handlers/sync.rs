use crate::dispatch::{AppServices, CommandError};

/// The only command that is host control rather than app behavior.
pub async fn force_quit(services: &AppServices) -> Result<(), CommandError> {
    services.host.exit(0);
    Ok(())
}
