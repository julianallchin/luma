//! `luma.score` — the annotations laid over the track for one venue.
//!
//! Times are **absolute seconds**, which is what `track_scores` actually stores.
//! The frontend's bar-based editing is a presentation convention layered on the
//! beat grid; publishing bars here would force every consumer to agree on a
//! conversion. Agents that want bars have `luma.features.beats`.

use serde::Serialize;

use super::{inline, unavailable, ProviderCtx};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::database::local;

/// A track has one score per venue; a thread that hasn't picked one has nothing
/// to show.
pub const NO_SCORE: &str = "no score is in scope for this agent thread";

#[derive(Serialize)]
struct AnnotationBinding {
    id: String,
    pattern_id: String,
    pattern_name: Option<String>,
    start_time_s: f64,
    end_time_s: f64,
    z_index: i64,
    blend_mode: String,
    args: serde_json::Value,
}

pub async fn provide(b: &mut BindingBuilder, ctx: &ProviderCtx<'_>) -> Result<(), String> {
    let Some(score_id) = ctx.scope.score_id.as_deref() else {
        return unavailable(b, "score", NO_SCORE);
    };

    match local::scores::get_score(ctx.pool, score_id).await {
        Ok(score) => {
            inline(b, "score.id", &score.id)?;
            inline(b, "score.name", &score.name)?;
            inline(b, "score.venue_id", &score.venue_id)?;
        }
        Err(e) => {
            inline(b, "score.id", score_id)?;
            unavailable(
                b,
                "score.name",
                format!("the score could not be loaded: {e}"),
            )?;
            unavailable(
                b,
                "score.venue_id",
                format!("the score could not be loaded: {e}"),
            )?;
        }
    }

    let annotations = match local::scores::list_track_scores_for_score(ctx.pool, score_id).await {
        Ok(a) => a,
        Err(e) => {
            return unavailable(
                b,
                "score.annotations",
                format!("the score's annotations could not be loaded: {e}"),
            )
        }
    };

    // One lookup per distinct pattern: an annotation that names a pattern id the
    // agent can't resolve is nearly useless to it.
    let names = local::patterns::list_patterns_pool(ctx.pool)
        .await
        .unwrap_or_default();
    let name_of = |id: &str| names.iter().find(|p| p.id == id).map(|p| p.name.clone());

    let bindings: Vec<AnnotationBinding> = annotations
        .into_iter()
        .map(|a| AnnotationBinding {
            pattern_name: name_of(&a.pattern_id),
            id: a.id,
            pattern_id: a.pattern_id,
            start_time_s: a.start_time,
            end_time_s: a.end_time,
            z_index: a.z_index,
            blend_mode: serde_json::to_value(a.blend_mode)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "replace".into()),
            args: a.args,
        })
        .collect();
    inline(b, "score.annotations", &bindings)
}
