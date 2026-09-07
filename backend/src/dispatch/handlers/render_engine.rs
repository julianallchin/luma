//! Live render-engine control: per-deck perform state, layer teardown, and the
//! identify blink. Every one of these is authorized by a venue lease and then
//! mutates in-memory render state — nothing here writes to the database.

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::render_engine::{authorize_identify_targets, PerformDeckInput};

/// Batch-update per-deck render states (time + volume) from the Perform page.
/// Called every StateChanged frame to drive real-time crossfade blending.
pub async fn render_set_deck_states(
    services: &AppServices,
    venue_id: String,
    states: Vec<PerformDeckInput>,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services.render_engine.set_perform_deck_states(states);
    Ok(())
}

/// Clear all perform state (layers + deck states). Called on disconnect/unmount.
pub async fn render_clear_perform(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    services.render_engine.clear_perform();
    Ok(())
}

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
/// ArtNet). Targets are `"fixtureId"` (whole fixture) or `"fixtureId:head"`.
pub async fn render_identify(
    services: &AppServices,
    targets: Vec<String>,
) -> Result<(), CommandError> {
    let _access = authorize_identify_targets(&services.db.0, &targets).await?;
    services.render_engine.identify_targets(targets);
    Ok(())
}
