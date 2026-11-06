//! Live follow: decks playing in DJ software drive the matching scores. The
//! session itself lives in `crate::perform`; these are its controls.

use crate::database::local::perform_set as db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::perform::{PerformLayout, PerformPattern, PerformSource, PerformState};
use crate::services::perform_set::PerformSet;

/// Follow `source`'s decks with `venue_id`'s scores, replacing whatever was
/// being followed.
pub async fn perform_start(
    services: &AppServices,
    venue_id: String,
    source: PerformSource,
) -> Result<(), CommandError> {
    Ok(services.perform.start(venue_id, source).await?)
}

/// Stop following and clear every deck's lights. Idempotent.
pub async fn perform_stop(services: &AppServices) -> Result<(), CommandError> {
    services.perform.stop().await;
    Ok(())
}

/// The source, its decks and their matches, positions extrapolated to now.
pub async fn perform_state(services: &AppServices) -> Result<PerformState, CommandError> {
    Ok(services.perform.state())
}

/// Shift every deck's lights `latency_ms` earlier (negative: later), within
/// ±1 s, to cover the DJ software's and the source's output latency.
pub async fn perform_set_latency(
    services: &AppServices,
    latency_ms: f32,
) -> Result<(), CommandError> {
    services.perform.set_latency(latency_ms);
    Ok(())
}

/// The venue's Perform set: its patterns, the top of the set first.
pub async fn list_perform_patterns(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<PerformPattern>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(db::list(&mut access).await?)
}

/// Store a Perform pattern, new or changed, under the id the caller gave it.
/// It must pass the same checks a score's pattern does.
pub async fn save_perform_pattern(
    services: &AppServices,
    venue_id: String,
    pattern: PerformPattern,
) -> Result<PerformPattern, CommandError> {
    PerformSet::new(&services.db.0, &services.perform, &venue_id)
        .change(|set| {
            let pattern = PerformPattern {
                venue_id: venue_id.clone(),
                ..pattern
            };
            set.put(pattern.clone());
            Ok(pattern)
        })
        .await
}

/// Delete a Perform pattern, its cell, and every fader cell's hold on it.
pub async fn delete_perform_pattern(
    services: &AppServices,
    venue_id: String,
    pattern_id: String,
) -> Result<(), CommandError> {
    PerformSet::new(&services.db.0, &services.perform, &venue_id)
        .change(|set| set.remove(&pattern_id).map(drop))
        .await
}

/// The venue's Perform layout, or `None` before one is saved.
pub async fn get_perform_layout(
    services: &AppServices,
    venue_id: String,
) -> Result<Option<PerformLayout>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(db::layout(&mut access).await?)
}

/// Replace the venue's Perform layout. Every cell must fit its page, over
/// no other.
pub async fn save_perform_layout(
    services: &AppServices,
    venue_id: String,
    layout: PerformLayout,
) -> Result<(), CommandError> {
    PerformSet::new(&services.db.0, &services.perform, &venue_id)
        .change(|set| {
            set.layout = layout;
            Ok(())
        })
        .await
}

/// Open the venue's Perform set: compile every pattern of it, to play as
/// its controls (`controls::perform`: each pattern's level, its graph's
/// fader and button nodes, its pad among them, `perform_master`) say.
pub async fn perform_open_set(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    Ok(services.perform.open_set(venue_id).await?)
}

/// Set control `name` of the open Perform set's venue to `value`, 0–1, as
/// its faders do: a pattern's level, a fader node, a fader cell (which sets
/// its targets), the master, or one of the venue's own controls. A button
/// set this way gates on above ½ and off below; press it with
/// `perform_press` to play it by its mode.
pub async fn perform_control(
    services: &AppServices,
    name: String,
    value: f64,
) -> Result<(), CommandError> {
    if !luma_patterns::clip_graph::is_control_name(&name) {
        return Err(CommandError::Invalid(format!(
            "expected a control name in snake_case, such as drop or front_level; got \"{name}\""
        )));
    }
    Ok(services.perform.set_control(&name, value)?)
}

/// Press (`down`) or release button `name` of the open Perform set: a
/// button node of a pattern's graph (`controls::perform::node`), its pad
/// (`controls::perform::pad`) or another. It plays as the button says.
pub async fn perform_press(
    services: &AppServices,
    name: String,
    down: bool,
) -> Result<(), CommandError> {
    if !luma_patterns::clip_graph::is_control_name(&name) {
        return Err(CommandError::Invalid(format!(
            "expected a control name in snake_case; got \"{name}\""
        )));
    }
    Ok(services.perform.press(&name, down)?)
}

/// Set the venue's tapped tempo outright, keeping its beat where it is.
pub async fn perform_set_bpm(
    services: &AppServices,
    venue_id: String,
    bpm: f64,
) -> Result<(), CommandError> {
    VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    crate::controls::venue(&venue_id).set_bpm(bpm);
    Ok(())
}
