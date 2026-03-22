use sqlx::SqlitePool;

use crate::models::venues::VenueImplementationOverride;

/// Fetch a venue override by composite key
pub async fn get_venue_override(
    pool: &SqlitePool,
    venue_id: &str,
    pattern_id: &str,
) -> Result<VenueImplementationOverride, String> {
    sqlx::query_as::<_, VenueImplementationOverride>(
        "SELECT venue_id, pattern_id, implementation_id, uid, created_at, updated_at
         FROM venue_implementation_overrides WHERE venue_id = ? AND pattern_id = ?",
    )
    .bind(venue_id)
    .bind(pattern_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch venue_override: {}", e))
}

/// List all venue overrides
pub async fn list_venue_overrides(
    pool: &SqlitePool,
) -> Result<Vec<VenueImplementationOverride>, String> {
    sqlx::query_as::<_, VenueImplementationOverride>(
        "SELECT venue_id, pattern_id, implementation_id, uid, created_at, updated_at
         FROM venue_implementation_overrides",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list venue_overrides: {}", e))
}
