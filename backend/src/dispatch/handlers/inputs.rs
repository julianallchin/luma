//! MIDI and OSC inputs bound to a venue's live controls. See `crate::inputs`.

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::AppServices;
use crate::error::CommandError;
pub use crate::inputs::{
    Address, Binding, Controller, ControllerState, InputState, LastInput, Mode,
};

async fn open(services: &AppServices, venue_id: &str) -> Result<(), CommandError> {
    let _access = VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(venue_id)).await?;
    Ok(services.inputs.open(venue_id).await?)
}

/// Bind inputs to `venue_id`'s controls, opening every MIDI port and the OSC
/// socket the first time.
pub async fn input_open(
    services: &AppServices,
    venue_id: String,
) -> Result<InputState, CommandError> {
    open(services, &venue_id).await?;
    Ok(services.inputs.state())
}

/// The open ports, the last input heard, learning, and the open venue's
/// bindings.
pub async fn input_state(services: &AppServices) -> Result<InputState, CommandError> {
    Ok(services.inputs.state())
}

/// Wait up to 20 s for the next input, and bind it to `control` on
/// controller `controller_id` (none: the venue's first, or a new one).
/// Fails when nothing is heard or another learn replaces this one.
pub async fn input_learn(
    services: &AppServices,
    venue_id: String,
    control: String,
    mode: Mode,
    controller_id: Option<String>,
) -> Result<Binding, CommandError> {
    if !luma_patterns::clip_graph::is_control_name(&control) {
        return Err(CommandError::Invalid(format!(
            "expected a control name in snake_case, such as drop or front_level; got \"{control}\""
        )));
    }
    if services.inputs.state().venue_id.as_deref() != Some(venue_id.as_str()) {
        open(services, &venue_id).await?;
    }
    Ok(services
        .inputs
        .learn(&control, mode, controller_id.as_deref(), None)
        .await?)
}

/// Stop waiting for an input to learn.
pub async fn input_cancel_learn(services: &AppServices) -> Result<(), CommandError> {
    services.inputs.cancel_learn();
    Ok(())
}

pub async fn input_update_binding(
    services: &AppServices,
    venue_id: String,
    binding_id: String,
    mode: Mode,
    invert: bool,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services
        .inputs
        .update(&venue_id, &binding_id, mode, invert, None)
        .await?)
}

pub async fn input_unbind(
    services: &AppServices,
    venue_id: String,
    binding_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.inputs.unbind(&venue_id, &binding_id, None).await?)
}

/// Put page `page_id` of the venue's layout on controller `controller_id`'s
/// grid, by its profile. Answers the bindings made.
pub async fn input_map_page(
    services: &AppServices,
    venue_id: String,
    controller_id: String,
    page_id: String,
) -> Result<Vec<Binding>, CommandError> {
    let set =
        crate::services::perform_set::PerformSet::new(&services.db.0, &services.perform, &venue_id)
            .read()
            .await?;
    let page = set
        .layout
        .pages
        .iter()
        .find(|page| page.id == page_id)
        .ok_or_else(|| CommandError::NotFound("No such page".into()))?;
    if services.inputs.state().venue_id.as_deref() != Some(venue_id.as_str()) {
        open(services, &venue_id).await?;
    }
    services
        .inputs
        .map_page(&controller_id, page, &set.patterns, None)
        .await
        .map_err(CommandError::Invalid)
}
