//! MIDI and OSC inputs bound to a venue's live controls. See `crate::inputs`.

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::AppServices;
use crate::error::CommandError;
pub use crate::inputs::{Address, Binding, InputState, LastInput, Mode};

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

/// Wait up to 20 s for the next input, and bind it to `control`. Fails when
/// nothing is heard or another learn replaces this one.
pub async fn input_learn(
    services: &AppServices,
    venue_id: String,
    control: String,
    mode: Mode,
) -> Result<Binding, CommandError> {
    if !luma_patterns::clip_graph::is_control_name(&control) {
        return Err(CommandError::Invalid(format!(
            "expected a control name in snake_case, such as drop or front_level; got \"{control}\""
        )));
    }
    if services.inputs.state().venue_id.as_deref() != Some(venue_id.as_str()) {
        open(services, &venue_id).await?;
    }
    Ok(services.inputs.learn(&control, mode).await?)
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
        .update(&venue_id, &binding_id, mode, invert)
        .await?)
}

pub async fn input_unbind(
    services: &AppServices,
    venue_id: String,
    binding_id: String,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(services.inputs.unbind(&venue_id, &binding_id).await?)
}
