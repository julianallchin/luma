//! Live render-engine control: layer teardown and the identify blink. Every
//! one of these is authorized by a venue lease and then mutates in-memory
//! render state — nothing here writes to the database.

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::render_engine::authorize_identify_targets;

/// Clear the active layer so the render loop emits nothing. Called when
/// navigating away from the track/pattern editor.
pub async fn render_clear_active_layer(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services.render_engine.set_active_scene(None);
    Ok(())
}

/// Trigger a two-blink identify sequence for one or more targets (visualizer +
/// DMX output). Targets are `"fixtureId"` (whole fixture) or `"fixtureId:head"`.
pub async fn render_identify(
    services: &AppServices,
    targets: Vec<String>,
) -> Result<(), CommandError> {
    let _access = authorize_identify_targets(&services.db.0, &targets).await?;
    services.render_engine.identify_targets(targets);
    Ok(())
}
