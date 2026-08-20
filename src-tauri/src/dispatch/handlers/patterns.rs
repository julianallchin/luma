use crate::database::local::patterns as db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::authored_state::AuthoredProjectedDocument;
use crate::models::node_graph::{Graph, PatternArgDef};
use crate::models::patterns::PatternSummary;
use crate::services::graph_documents::{load_visible_graph_document, GraphEditResult};

pub async fn list_patterns(services: &AppServices) -> Result<Vec<PatternSummary>, CommandError> {
    Ok(db::list_patterns_pool(&services.db.0).await?)
}

/// Resolves against a venue, unlike `get_pattern_graph_document`, which passes
/// `venue_id = None` — so the two can return *different* implementations for
/// the same pattern id. See `pattern-args-venue-divergence` in the IPC manifest.
pub async fn get_pattern_args(
    services: &AppServices,
    id: String,
    venue_id: Option<String>,
    implementation_id: Option<String>,
) -> Result<Vec<PatternArgDef>, CommandError> {
    let pool = &services.db.0;
    db::get_pattern_pool(pool, &id).await?;
    let document =
        load_visible_graph_document(pool, &id, venue_id.as_deref(), implementation_id.as_deref())
            .await
            .map_err(|error| CommandError::Internal(error.to_string()))?;
    Ok(document.graph.args)
}

/// Idempotent by `operation_id` + base revision. The TS caller reuses the id
/// verbatim on retry; that reuse is the only thing making a blind retry safe.
pub async fn save_pattern_graph_document(
    services: &AppServices,
    id: String,
    implementation_id: String,
    operation_id: String,
    base_revision: String,
    graph: Graph,
) -> Result<GraphEditResult, CommandError> {
    let owner_user_id = services.session_user_id().await?;
    let result = services
        .authored
        .apply_graph_for_scope(
            &services.db.0,
            owner_user_id.as_deref(),
            &id,
            &implementation_id,
            &operation_id,
            graph,
            &base_revision,
            "Save pattern graph",
        )
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?;
    let AuthoredProjectedDocument::PatternGraph {
        implementation_id: projected_implementation_id,
        revision,
        graph,
    } = result.document
    else {
        return Err(CommandError::Internal(
            "authored graph save returned a track projection".into(),
        ));
    };
    if projected_implementation_id != implementation_id {
        return Err(CommandError::Internal(
            "authored graph save returned another implementation".into(),
        ));
    }
    if result.changed {
        services.sync.push_notify.notify_one();
    }
    Ok(GraphEditResult {
        revision,
        graph,
        changed: result.changed,
    })
}
