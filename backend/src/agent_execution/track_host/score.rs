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
