use crate::dispatch::{AppServices, CommandError};
use crate::models::composable_patterns::ComposablePreviewRequest;
use serde_json::Value;

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
