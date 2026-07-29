//! `luma.track` — the identity of the track under analysis.
//!
//! Everything here is a scalar; the numbers that need an axis live under
//! `luma.features`. `bpm` is duplicated from `track_beats` because it is track
//! identity as far as the agent is concerned, not a beat-grid detail.

use super::{inline, unavailable, ProviderCtx, NO_TRACK};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::database::local;

/// Luma has never detected or stored a musical key — this is not "missing data
/// for this track", it is a feature that does not exist.
pub const NO_KEY_SOURCE: &str =
    "musical key is not detected or stored by Luma — there is no key data source";

pub async fn provide(b: &mut BindingBuilder, ctx: &ProviderCtx<'_>) -> Result<(), String> {
    let Some(track) = ctx.track.as_ref() else {
        return unavailable(b, "track", NO_TRACK);
    };

    inline(b, "track.id", &track.id)?;
    inline(b, "track.title", &track.title)?;
    inline(b, "track.artist", &track.artist)?;
    inline(b, "track.album", &track.album)?;
    inline(b, "track.duration_s", track.duration_seconds)?;

    // The tracks table has no bpm column — the beat grid owns it.
    let bpm = local::tracks::get_track_beats_raw(ctx.pool, &track.id)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.bpm);
    inline(b, "track.bpm", bpm)?;

    unavailable(b, "track.key", NO_KEY_SOURCE)
}
