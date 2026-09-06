//! The Python graph builder edits the canonical document with core GraphEdit
//! operations. This adapter supplies scope, production rendering and history.
use super::*;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::services::graph_scores::{
    self, GraphScoreDocument, GraphScoreEdit, GraphScoreOperation, ScoreDocument,
};

impl TrackHost {
    async fn score_document(&self) -> Result<ScoreDocument, HostCallError> {
        let document = if let Some(workspace) = self.authored_workspace_id.as_deref() {
            self.authored
                .track_workspace(
                    &self.pool,
                    self.edit_scope.as_ref().map(|scope| scope.user_id.as_str()),
                    &self.thread_id,
                    workspace,
                )
                .await
                .map_err(authored_error)?
                .document
        } else {
            let mut access =
                VenueAccess::<Read>::read(&self.pool, VenueResource::Score(&self.scope.score_id))
                    .await
                    .map_err(|error| HostCallError::new("forbidden", error))?;
            graph_scores::read_score_document(&mut access, &self.scope)
                .await
                .map_err(|error| HostCallError::new("invalid_score", error))?
        };
        Ok(document)
    }

    async fn graph_document(&self) -> Result<GraphScoreDocument, HostCallError> {
        match self.score_document().await? {
            ScoreDocument::Graph(document) => Ok(document),
            ScoreDocument::Legacy(_) => Err(HostCallError::new(
                "invalid_score",
                "this score has not been migrated to graphs",
            )),
        }
    }

    pub(crate) fn track_id(&self) -> &str {
        &self.scope.track_id
    }

    /// Stage inspection follows the same live/detached source as Python
    /// editing, including a save made earlier in this cell.
    pub(crate) async fn saved_scene(&self) -> Result<crate::eval::Scene, HostCallError> {
        match self.score_document().await? {
            ScoreDocument::Graph(document) => crate::compositor::build_score_scene(
                &self.pool,
                &self.storage,
                &self.resource_root,
                &self.scope.score_id,
                None,
                Some(document.score),
                true,
            )
            .await
            .map_err(|error| HostCallError::new("compile_error", error)),
            ScoreDocument::Legacy(document) => build_scene_strict(
                &self.pool,
                &self.pool,
                &self.storage,
                &self.resource_root,
                &self.scope.track_id,
                &self.scope.venue_id,
                &as_track_scores(&self.scope, &document.clips),
            )
            .await
            .map_err(|error| HostCallError::new("compile_error", error)),
        }
    }

    async fn prepare_score(
        &self,
        plan: &GraphScoreEdit,
    ) -> Result<crate::eval::Scene, HostCallError> {
        let current = self.graph_document().await?;
        if current.revision != plan.base_revision {
            return Err(HostCallError::new(
                "conflict",
                "the score changed; start a new edit from the current document",
            ));
        }
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
            &self.scope.track_id,
            &plan.candidate,
        )
        .await
        .map_err(|error| HostCallError::new("compile_error", error))
    }

    pub(super) async fn score_call(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        let limit = call_limit(context)?;
        if method == "track.score_apply" {
            let plan: GraphScoreEdit = decode(payload)?;
            let scope = self
                .edit_scope
                .as_ref()
                .ok_or_else(|| HostCallError::new("forbidden", "this score is read-only"))?;
            let operation_scope = context.operation_scope().ok_or_else(|| {
                HostCallError::new(
                    "internal",
                    "editable Python cell has no durable operation scope",
                )
            })?;
            let canonical = crate::canonical_json::to_string(
                &json!({"scope": scope, "workspace": self.authored_workspace_id, "plan": plan}),
            );
            let fingerprint = format!(
                "sha256:{:x}",
                scoped_apply_digest(b"luma.python-graph-score-request.v2", &[&canonical])
            );
            let id = apply_operation_id(operation_scope.operation_namespace(), &fingerprint);
            let operation = GraphScoreOperation {
                thread_id: &self.thread_id,
                workspace_id: self.authored_workspace_id.as_deref(),
                scope: &self.scope,
                id: &id,
                fingerprint: &fingerprint,
            };
            let replay = supervise(
                async {
                    self.authored
                        .graph_score_operation(&self.pool, Some(&scope.user_id), &operation, None)
                        .await
                        .map_err(authored_error)
                },
                context,
                limit,
            )
            .await?;
            if let Some(document) = replay {
                return Ok(json!(document));
            }
            supervise(self.prepare_score(&plan), context, limit).await?;
            context.begin_irreversible()?;
            let document = self
                .authored
                .graph_score_operation(&self.pool, Some(&scope.user_id), &operation, Some(plan))
                .await
                .map_err(authored_error)?;
            return Ok(json!(
                document.expect("an applied score returns its document")
            ));
        }
        supervise(async {
            match method {
                "track.score_check" => {
                    let plan: GraphScoreEdit = decode(payload)?;
                    let scene = self.prepare_score(&plan).await?;
                    Ok(json!({"ok": true, "clips": plan.candidate.clips.len(), "compiledClips": scene.annotations.len()}))
                }
                "track.score_render" => {
                    let request: Render = decode(payload)?;
                    validate_window(&self.pool, &self.scope.track_id, request.start_time, request.end_time).await?;
                    let scene = self.prepare_score(&GraphScoreEdit {base_revision: request.base_revision, candidate: request.candidate}).await?;
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
                _ => Err(HostCallError::new("unknown_method", "unknown score operation")),
            }
        }, context, limit).await
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Render {
    base_revision: String,
    candidate: luma_patterns::Score,
    start_time: f64,
    end_time: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    candidate: luma_patterns::Score,
    graph: String,
    edits: Vec<luma_patterns::GraphEdit>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Independent {
    candidate: luma_patterns::Score,
    clip: String,
    id: String,
}
