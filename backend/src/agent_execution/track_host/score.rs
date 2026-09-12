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
        let mut access =
            VenueAccess::<Write>::write(&self.pool, VenueResource::Score(&self.scope.score_id))
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
            supervise(self.prepare_score(&plan.candidate), context, limit).await?;
            context.begin_irreversible()?;
            self.apply_score(&plan.candidate).await?;
            return Ok(json!(plan.candidate));
        }
        supervise(async {
            match method {
                "track.score_upgrade" => {
                    let request: Candidate = decode(payload)?;
                    let candidate = luma_patterns::migration::upgrade(&request.candidate)
                        .map_err(|error| HostCallError::new("invalid_score", error.to_string()))?;
                    Ok(json!(candidate))
                }
                "track.graph_instance" => {
                    self.edit_scope.as_ref().ok_or_else(|| HostCallError::new("forbidden", "this score is read-only"))?;
                    let request: Instance = decode(payload)?;
                    let library = request.candidate.library(&luma_patterns::standard_library())
                        .map_err(|error| HostCallError::new("invalid_score", error.to_string()))?;
                    let definition = library.definitions.get(&request.definition)
                        .ok_or_else(|| HostCallError::new("invalid_score", "unknown graph definition"))?;
                    let instance = if definition.placeable() {
                        definition.clip_instance(&request.definition)
                            .map_err(|error| HostCallError::new("invalid_score", error.to_string()))?
                    } else { definition.instance(&request.definition) };
                    Ok(json!(instance))
                }
                "track.score_check" => {
                    let plan: Candidate = decode(payload)?;
                    let scene = self.prepare_score(&plan.candidate).await?;
                    Ok(json!({"ok": true, "clips": plan.candidate.clips.len(), "compiledClips": scene.annotations.len()}))
                }
                "track.score_render" => {
                    let request: Render = decode(payload)?;
                    validate_window(&self.pool, &self.scope.track_id, request.start_time, request.end_time).await?;
                    let scene = self.prepare_score(&request.candidate).await?;
                    self.render_scene(scene, request.start_time, request.end_time).await
                }
                "track.graph_edit" => {
                    self.edit_scope.as_ref().ok_or_else(|| HostCallError::new("forbidden", "this score is read-only"))?;
                    let mut request: Edit = decode(payload)?;
                    if request.edits.len() > 256 { return Err(HostCallError::new("invalid_edit", "at most 256 graph gestures per call")); }
                    for edit in request.edits {
                        request.candidate.edit_graph(&luma_patterns::standard_library(), &request.graph, edit)
                            .map_err(|error| HostCallError::new("invalid_edit", error.to_string()))?;
                    }
                    Ok(json!(request.candidate))
                }
                "track.score_independent" => {
                    self.edit_scope.as_ref().ok_or_else(|| HostCallError::new("forbidden", "this score is read-only"))?;
                    let mut request: Independent = decode(payload)?;
                    request.candidate.make_independent(&luma_patterns::standard_library(), &request.clip, &request.id)
                        .map_err(|error| HostCallError::new("invalid_edit", error.to_string()))?;
                    Ok(json!(request.candidate))
                }
                "track.graph_customize" => {
                    self.edit_scope.as_ref().ok_or_else(|| HostCallError::new("forbidden", "this score is read-only"))?;
                    let mut request: Customize = decode(payload)?;
                    request.candidate.customize_node(&luma_patterns::standard_library(), &request.graph, &request.node, &request.id)
                        .map_err(|error| HostCallError::new("invalid_edit", error.to_string()))?;
                    Ok(json!(request.candidate))
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
#[serde(deny_unknown_fields)]
struct Instance {
    candidate: Score,
    definition: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Render {
    candidate: Score,
    start_time: f64,
    end_time: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    candidate: Score,
    graph: String,
    edits: Vec<luma_patterns::GraphEdit>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Independent {
    candidate: Score,
    clip: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Customize {
    candidate: Score,
    graph: String,
    node: String,
    id: String,
}
