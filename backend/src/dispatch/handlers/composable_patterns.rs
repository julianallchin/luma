use crate::dispatch::{AppServices, CommandError};
use crate::models::composable_patterns::ComposablePreviewRequest;
use serde_json::Value;

pub async fn get_pattern_node_library(_services: &AppServices) -> Result<Value, CommandError> {
    serde_json::to_value(luma_patterns::standard_library())
        .map_err(|error| CommandError::Internal(error.to_string()))
}
pub async fn preview_composable_pattern(
    services: &AppServices,
    request: Value,
) -> Result<Value, CommandError> {
    let request: ComposablePreviewRequest = serde_json::from_value(request)
        .map_err(|error| CommandError::Invalid(error.to_string()))?;
    let result = crate::services::composable_patterns::preview(
        &services.db.0,
        &services.fixtures_root,
        &services.storage,
        request,
    )
    .await?;
    serde_json::to_value(result).map_err(|error| CommandError::Internal(error.to_string()))
}

/// Create the pattern and its graph in one transaction. The score-ownership
/// constraint is enforced by SQLite in that same transaction.
pub async fn create_lighting_pattern(
    services: &AppServices,
    effect: String,
    score_id: String,
    request_id: String,
) -> Result<crate::models::patterns::PatternSummary, CommandError> {
    let graph = crate::node_graph::lighting::pattern(&effect).map_err(CommandError::Invalid)?;
    let name = luma_patterns::standard_library().definitions[&effect]
        .name
        .clone();
    let principal = services.session_user_id().await?;
    let result = crate::services::catalog::create_pattern_with_graph(
        &services.db.0,
        principal.as_deref(),
        &request_id,
        name,
        None,
        Some(graph),
        Some(&score_id),
    )
    .await?;
    Ok(result)
}

/// Save an independent library copy; existing score clips keep their local graph.
pub async fn copy_pattern_to_library(
    services: &AppServices,
    pattern_id: String,
    request_id: String,
) -> Result<crate::models::patterns::PatternSummary, CommandError> {
    let source =
        crate::database::local::patterns::get_pattern_pool(&services.db.0, &pattern_id).await?;
    let document = crate::services::graph_documents::load_visible_graph_document(
        &services.db.0,
        &pattern_id,
        None,
        None,
    )
    .await?;
    let principal = services.session_user_id().await?;
    let result = crate::services::catalog::create_pattern_with_graph(
        &services.db.0,
        principal.as_deref(),
        &request_id,
        source.name,
        source.description,
        Some(document.graph),
        None,
    )
    .await?;
    Ok(result)
}
