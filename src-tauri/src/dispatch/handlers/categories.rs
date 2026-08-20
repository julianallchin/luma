//! Pattern categories.

use crate::database::local::categories as categories_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::patterns::PatternCategory;

/// Every category, unscoped by user despite the `uid` column.
pub async fn list_pattern_categories(
    services: &AppServices,
) -> Result<Vec<PatternCategory>, CommandError> {
    Ok(categories_db::list_pattern_categories_pool(&services.db.0).await?)
}
