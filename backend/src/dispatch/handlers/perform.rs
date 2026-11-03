//! Live follow: decks playing in DJ software drive the matching scores. The
//! session itself lives in `crate::perform`; these are its controls.

use crate::dispatch::{AppServices, CommandError};
use crate::models::perform::{PerformSource, PerformState};

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

/// Pin a deck to a library track, or with no track, match it again. A pinned
/// track holds until the deck loads another.
pub async fn perform_assign_track(
    services: &AppServices,
    deck_id: u8,
    track_id: Option<String>,
) -> Result<(), CommandError> {
    Ok(services.perform.assign(deck_id, track_id)?)
}

/// Passively listen for CDJ keepalives for 3 seconds and return what was heard.
/// Safe to call while not connected — it performs no device-number claim.
///
/// An empty list means "nothing heard"; a failure to bind the socket also
/// yields an empty list, so the two are indistinguishable to the caller.
pub async fn prodjlink_discover(
    _services: &AppServices,
) -> Result<Vec<prodjlink::DiscoveredDevice>, CommandError> {
    Ok(prodjlink::discover_cdjs(3000).await)
}
