//! `luma.track` — track identity plus its authored lighting timeline.
//!
//! A score is persistence vocabulary, not an agent concept. When a concrete
//! score is in scope its clips live directly under `luma.track`, beside the
//! stable semantic revision used by `luma.track.edit()`. Times remain absolute
//! seconds; musical coordinates come from `luma.features`.

use serde::Serialize;

use super::{inline, unavailable, ProviderCtx, NO_TRACK};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::database::local;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::models::node_graph::BlendMode;
use crate::services::graph_scores::{read_score_document, ScoreDocument};
use crate::services::track_edits::{TrackClip, TrackScope};

/// A track can exist without a score selected (notably in graph-agent scope).
const NO_TIMELINE: &str = "no authored lighting timeline is in scope for this agent thread";

#[derive(Serialize)]
struct ClipBinding {
    id: String,
    pattern_id: String,
    pattern_name: Option<String>,
    start_s: f64,
    end_s: f64,
    z: i64,
    blend: BlendMode,
    args: serde_json::Value,
}

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
) -> Result<Option<ScoreDocument>, String> {
    if let Some(document) = &scope.track_document {
        return Ok(Some(document.clone()));
    }
    let (Some(score_id), Some(track_id), Some(venue_id)) =
        (&scope.score_id, &scope.track_id, &scope.venue_id)
    else {
        return Ok(None);
    };
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Score(score_id)).await?;
    access.require_venue(venue_id)?;
    read_score_document(
        &mut access,
        &TrackScope {
            score_id: score_id.clone(),
            track_id: track_id.clone(),
            venue_id: venue_id.clone(),
        },
    )
    .await
    .map(Some)
}

async fn provide_timeline(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    _track_id: &str,
) -> Result<(), String> {
    match &ctx.score_document {
        Ok(Some(ScoreDocument::Graph(document))) => {
            inline(b, "track.revision", &document.revision)?;
            inline(b, "track.document", &document.score)?;
            let grid = crate::services::tracks::get_track_beats(ctx.pool, _track_id).await?;
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
        Ok(Some(ScoreDocument::Legacy(document))) => {
            publish_timeline(b, ctx, document.revision.clone(), document.clips.clone()).await
        }
        result => {
            let reason = match result {
                Err(error) => error.as_str(),
                _ => NO_TIMELINE,
            };
            unavailable(b, "track.revision", reason)?;
            unavailable(b, "track.clips", reason)?;
            inline(b, "track.editable", false)
        }
    }
}

async fn publish_timeline(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    revision: String,
    mut scores: Vec<TrackClip>,
) -> Result<(), String> {
    // Time-major order matches how a human reads and edits the track. `z` is
    // explicit, so ordering is presentation only and never compositing meaning.
    scores.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then_with(|| left.z_index.cmp(&right.z_index))
            .then_with(|| left.id.cmp(&right.id))
    });

    let patterns = local::patterns::list_patterns_pool(ctx.pool)
        .await
        .unwrap_or_default();
    let pattern_name = |id: &str| {
        patterns
            .iter()
            .find(|pattern| pattern.id == id)
            .map(|pattern| pattern.name.clone())
    };
    let clips: Vec<ClipBinding> = scores
        .into_iter()
        .map(|clip| ClipBinding {
            pattern_name: pattern_name(&clip.pattern_id),
            id: clip.id,
            pattern_id: clip.pattern_id,
            start_s: clip.start_time,
            end_s: clip.end_time,
            z: clip.z_index,
            blend: clip.blend_mode,
            args: clip.args,
        })
        .collect();

    inline(b, "track.revision", revision)?;
    inline(b, "track.clips", &clips)?;
    // This bit is computed by the trusted command layer. It is descriptive;
    // the atomic edit service independently checks user/score/track/venue.
    inline(b, "track.editable", ctx.scope.track_editable)
}
