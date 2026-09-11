//! `luma.track` — track identity plus its authored lighting timeline.
//!
//! A score is persistence vocabulary, not an agent concept. When a concrete
//! score is in scope its clips live directly under `luma.track`, beside the
//! stable semantic revision used by `luma.track.edit()`. Times remain absolute
//! seconds; musical coordinates come from `luma.features`.

use super::{inline, unavailable, ProviderCtx, NO_TRACK};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::database::local;
use crate::database::local::scores::rows;
use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};

/// A track can exist without a score selected (notably in graph-agent scope).
const NO_TIMELINE: &str = "no authored lighting timeline is in scope for this agent thread";

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

    unavailable(b, "track.key", NO_KEY_SOURCE)?;
    provide_timeline(b, ctx, &track.id).await
}

pub(super) async fn resolve_document(
    pool: &sqlx::SqlitePool,
    scope: &super::BindingScope,
) -> Result<Option<luma_patterns::Score>, String> {
    if let Some(document) = &scope.track_document {
        return Ok(Some(document.clone()));
    }
    let (Some(score_id), Some(venue_id)) = (&scope.score_id, &scope.venue_id) else {
        return Ok(None);
    };
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Score(score_id)).await?;
    access.require_venue(venue_id)?;
    rows::load_score(access.connection(), score_id).await.map(Some)
}

async fn provide_timeline(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    track_id: &str,
) -> Result<(), String> {
    match &ctx.score_document {
        Ok(Some(score)) => {
            inline(b, "track.document", score)?;
            let grid = crate::services::tracks::get_track_beats(ctx.pool, track_id).await?;
            inline(
                b,
                "track.beat_origin_s",
                grid.map(|grid| {
                    grid.downbeats
                        .first()
                        .copied()
                        .unwrap_or(grid.downbeat_offset)
                }),
            )?;
            inline(b, "track.editable", ctx.scope.track_editable)
        }
        result => {
            let reason = match result {
                Err(error) => error.as_str(),
                _ => NO_TIMELINE,
            };
            unavailable(b, "track.document", reason)?;
            inline(b, "track.editable", false)
        }
    }
}
