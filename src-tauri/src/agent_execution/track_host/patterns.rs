//! Score-local Pattern creation shares the track's captured authority. Drafts
//! stay in Python; saving enters the existing authored-document transaction.
use super::*;
use crate::models::node_graph::Graph;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PatternRequest {
    name: String,
    description: Option<String>,
    graph: Graph,
}

impl TrackHost {
    pub(super) async fn pattern_call(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        if crate::canonical_json::to_string(&payload).len() > 1024 * 1024 {
            return Err(HostCallError::new(
                "invalid_request",
                "Pattern exceeds 1 MiB",
            ));
        }
        let request: PatternRequest = decode(payload)?;
        if request.name.trim().is_empty() || request.name.len() > 200 {
            return Err(HostCallError::new(
                "invalid_request",
                "Pattern name needs 1–200 bytes",
            ));
        }
        let graph = crate::services::graph_documents::canonicalize_graph(&request.graph)
            .map_err(|e| HostCallError::new("invalid_graph", e.to_string()))?;
        let defaults = graph
            .args
            .iter()
            .map(|arg| (arg.id.clone(), arg.default_value.clone()))
            .collect();
        crate::eval::lighting::library_for_graph(&graph.nodes, &graph.edges, &defaults)
            .map_err(|e| HostCallError::new("invalid_graph", e))?;
        if method == "track.pattern_check" {
            return Ok(json!({"valid": true, "args": graph.args}));
        }
        let scope = self
            .edit_scope
            .as_ref()
            .ok_or_else(|| HostCallError::new("forbidden", "this authored track is read-only"))?;
        if self.authored_workspace_id.is_some() {
            return Err(HostCallError::new("forbidden",
                "save Patterns from the main track thread; child workspace Pattern creation is not supported yet"));
        }
        let operation = context.operation_scope().ok_or_else(|| {
            HostCallError::new(
                "forbidden",
                "Pattern creation needs a durable authored turn",
            )
        })?;
        let fingerprint = crate::canonical_json::to_string(&json!({
            "scope": scope, "request": request,
        }));
        let digest = scoped_apply_digest(
            b"luma.python-pattern-create.v1",
            &[operation.operation_namespace(), &fingerprint],
        );
        let request_id =
            uuid::Uuid::from_bytes(digest.as_ref()[..16].try_into().expect("16 digest bytes"))
                .to_string();
        context.begin_irreversible()?;
        let pattern = self
            .authored
            .create_pattern_with_graph(
                &self.pool,
                Some(&scope.user_id),
                &request_id,
                request.name,
                request.description,
                Some(graph.clone()),
                Some(&scope.score_id),
            )
            .await
            .map_err(authored_error)?;
        Ok(json!({"id": pattern.id, "name": pattern.name, "args": graph.args}))
    }
}
