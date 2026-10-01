//! The Python score builder edits a whole [`luma_patterns::Score`] and hands it
//! back. This adapter owns authority: which score, whose, and whether the write
//! lands on the live rows or on the thread's draft.
use super::*;
use crate::database::local::scores::rows;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::services::{drafts, graph_scores};
use luma_patterns::Score;

impl TrackHost {
    /// The document this thread is working on: its draft's state when it has
    /// one, the live rows otherwise.
    async fn score_document(&self) -> Result<Score, HostCallError> {
        let mut access =
            VenueAccess::<Read>::read(&self.pool, VenueResource::Score(&self.scope.score_id))
                .await
                .map_err(|error| HostCallError::new("forbidden", error))?;
        match self.draft_id.as_deref() {
            Some(draft) => drafts::state(access.connection(), draft)
                .await
                .map_err(|error| HostCallError::new("invalid_score", error)),
            None => rows::load_score(access.connection(), &self.scope.score_id)
                .await
                .map_err(|error| HostCallError::new("invalid_score", error)),
        }
    }

    /// Refuse a candidate whose changed clips fail validation. Playback leaves
    /// a bad clip out and plays the rest; the agent must see the error instead,
    /// or check, render and apply all look fine while the clip goes dark.
    async fn validate_candidate(&self, candidate: &Score) -> Result<(), HostCallError> {
        let stored = self.score_document().await?;
        candidate
            .validate_changes(&luma_patterns::standard_library(), &stored)
            .map_err(|error| HostCallError::new("invalid_score", error.to_string()))
    }

    pub(crate) fn track_id(&self) -> &str {
        &self.scope.track_id
    }

    /// Stage inspection follows the same live/draft source as Python editing,
    /// including a save made earlier in this cell.
    pub(crate) async fn saved_scene(&self) -> Result<crate::eval::Scene, HostCallError> {
        let score = self.score_document().await?;
        crate::compositor::build_score_scene(
            &self.pool,
            &self.storage,
            &self.resource_root,
            &self.scope.score_id,
            Some(score),
        )
        .await
        .map_err(|error| HostCallError::new("compile_error", error))
    }

    pub(crate) async fn prepare_score(
        &self,
        candidate: &Score,
    ) -> Result<crate::eval::Scene, HostCallError> {
        let mut access =
            VenueAccess::<Read>::read(&self.pool, VenueResource::Score(&self.scope.score_id))
                .await
                .map_err(|error| HostCallError::new("forbidden", error))?;
        access
            .require_venue(&self.scope.venue_id)
            .map_err(|error| HostCallError::new("forbidden", error))?;
        graph_scores::prepare_scene(
            &mut access,
            &self.resource_root,
            &self.storage,
            &self.scope.track_id,
            candidate,
        )
        .await
        .map_err(|error| HostCallError::new("compile_error", error))
    }

    /// Write the candidate where this thread is allowed to: a subagent's draft,
    /// or the live rows. There is nothing to compare against first — the row
    /// diff only touches what actually differs.
    async fn apply_score(&self, candidate: &Score) -> Result<(), HostCallError> {
        let scope = self
            .edit_scope
            .as_ref()
            .ok_or_else(|| HostCallError::new("forbidden", "this score is read-only"))?;
        let mut access = VenueAccess::<Write>::write_as(
            &self.pool,
            VenueResource::Score(&self.scope.score_id),
            self.actor.as_deref(),
        )
        .await
        .map_err(|error| HostCallError::new("forbidden", error))?;
        match self.draft_id.as_deref() {
            Some(draft) => drafts::apply(access.connection(), draft, candidate)
                .await
                .map_err(|error| HostCallError::new("internal", error))?,
            None => {
                rows::save_score(
                    access.connection(),
                    &self.scope.score_id,
                    &scope.user_id,
                    candidate,
                )
                .await
                .map_err(|error| HostCallError::new("internal", error))?;
            }
        }
        access
            .commit()
            .await
            .map_err(|error| HostCallError::new("internal", error))
    }

    pub(super) async fn score_call(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        let limit = call_limit(context)?;
        if method == "track.score_apply" {
            let plan: Candidate = decode(payload)?;
            supervise(
                async {
                    self.validate_candidate(&plan.candidate).await?;
                    self.prepare_score(&plan.candidate).await
                },
                context,
                limit,
            )
            .await?;
            context.begin_irreversible()?;
            self.apply_score(&plan.candidate).await?;
            return Ok(json!(plan.candidate));
        }
        if method == "track.clip_check" {
            let request: ClipCheck = decode(payload)?;
            return Ok(clip_check(request));
        }
        supervise(async {
            match method {
                "track.score_check" => {
                    let plan: Candidate = decode(payload)?;
                    self.validate_candidate(&plan.candidate).await?;
                    let scene = self.prepare_score(&plan.candidate).await?;
                    Ok(json!({"ok": true, "clips": plan.candidate.clips.len(), "compiledClips": scene.annotations.len()}))
                }
                "track.score_render" => {
                    let request: Render = decode(payload)?;
                    validate_window(&self.pool, &self.scope.track_id, request.start_time, request.end_time).await?;
                    self.validate_candidate(&request.candidate).await?;
                    let scene = self.prepare_score(&request.candidate).await?;
                    self.render_scene(scene, request.start_time, request.end_time).await
                }
                _ => Err(HostCallError::new("unknown_method", "unknown score operation")),
            }
        }, context, limit).await
    }
}

/// One clip to check on its own, before it joins a candidate. `clip` stays
/// raw JSON so that a clip that does not even parse is reported like any
/// other checker error instead of as a bad payload.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipCheck {
    clip: Value,
    #[serde(default)]
    id: Option<String>,
}

/// `{"ok": true}`, or `{"ok": false, "error": text}` with the checker's text
/// (`clip <name> (<id>): <node>.<input>: expected ...`). It needs no venue:
/// the checker never looks at geometry.
fn clip_check(request: ClipCheck) -> Value {
    let id = request.id.as_deref().unwrap_or("new");
    let checked = serde_json::from_value::<luma_patterns::Clip>(request.clip)
        .map_err(|error| format!("clip ({id}): {error}"))
        .and_then(|clip| {
            Score::validate_clip(&luma_patterns::standard_library(), id, &clip)
                .map_err(|error| error.to_string())
        });
    match checked {
        Ok(()) => json!({"ok": true}),
        Err(error) => json!({"ok": false, "error": error}),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    candidate: Score,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Render {
    candidate: Score,
    start_time: f64,
    end_time: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(clip: Value) -> Value {
        clip_check(ClipCheck {
            clip,
            id: Some("c1".into()),
        })
    }

    fn clip(graph: Value) -> Value {
        json!({
            "name": "Pulse", "start": 0.0, "duration": 4.0, "seed": 1,
            "selection": luma_patterns::Selection::all(),
            "z_index": 0, "blend_mode": "replace", "graph": graph,
        })
    }

    #[test]
    fn a_good_clip_passes() {
        let graph = json!({"version": 3, "nodes": {
            "time1": {"kind": "time"},
            "curve1": {"kind": "curve", "settings": {"kind": "number"},
                       "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}});
        assert_eq!(check(clip(graph)), json!({"ok": true}));
    }

    /// The agent reads the checker's own text: which clip, which input,
    /// what it expected and an example.
    #[test]
    fn a_wrong_wire_returns_the_checker_text() {
        let graph = json!({"version": 3, "nodes": {
            "time1": {"kind": "time"},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "time1"}}}}});
        let result = check(clip(graph));
        assert_eq!(result["ok"], false);
        let error = result["error"].as_str().unwrap();
        assert!(error.contains("Pulse"), "{error}");
        assert!(error.contains("color1.brightness: expected"), "{error}");
        assert!(error.contains("Example: "), "{error}");
    }

    #[test]
    fn a_clip_that_does_not_parse_is_a_checker_error_too() {
        let mut bad = clip(json!({"version": 3, "nodes": {}}));
        bad["colour"] = json!(1);
        let result = check(bad);
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("colour"));
    }
}
