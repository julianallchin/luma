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
use crate::models::node_graph::BlendMode;

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

async fn provide_timeline(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    track_id: &str,
) -> Result<(), String> {
    let Some(score_id) = ctx.scope.score_id.as_deref() else {
        unavailable(b, "track.revision", NO_TIMELINE)?;
        unavailable(b, "track.clips", NO_TIMELINE)?;
        return inline(b, "track.editable", false);
    };

    // Do not publish a caller-mismatched document under this track. Mutation
    // authorization is still rechecked transactionally by the host service;
    // this is the read-side scope invariant.
    let score = match local::scores::get_score(ctx.pool, score_id).await {
        Ok(score) => score,
        Err(error) => {
            let reason = format!("the authored lighting timeline could not be loaded: {error}");
            unavailable(b, "track.revision", &reason)?;
            unavailable(b, "track.clips", reason)?;
            return inline(b, "track.editable", false);
        }
    };
    let venue_matches = ctx
        .scope
        .venue_id
        .as_deref()
        .is_some_and(|venue_id| venue_id == score.venue_id.as_str());
    if score.track_id.as_str() != track_id || !venue_matches {
        let reason = "the selected lighting timeline does not belong to this track and venue";
        unavailable(b, "track.revision", reason)?;
        unavailable(b, "track.clips", reason)?;
        return inline(b, "track.editable", false);
    }

    let mut scores = match local::scores::list_track_scores_for_score(ctx.pool, score_id).await {
        Ok(scores) => scores,
        Err(error) => {
            let reason = format!("the authored lighting clips could not be loaded: {error}");
            unavailable(b, "track.revision", &reason)?;
            unavailable(b, "track.clips", reason)?;
            return inline(b, "track.editable", false);
        }
    };
    let revision = crate::services::track_edits::track_revision(&scores);

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
        .map(|score| ClipBinding {
            pattern_name: pattern_name(&score.pattern_id),
            id: score.id,
            pattern_id: score.pattern_id,
            start_s: score.start_time,
            end_s: score.end_time,
            z: score.z_index,
            blend: score.blend_mode,
            args: score.args,
        })
        .collect();

    inline(b, "track.revision", revision)?;
    inline(b, "track.clips", &clips)?;
    // This bit is computed by the trusted command layer. It is descriptive;
    // the atomic edit service independently checks user/score/track/venue.
    inline(b, "track.editable", ctx.scope.track_editable)
}
