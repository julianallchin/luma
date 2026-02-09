use sqlx::SqlitePool;

use crate::models::venues::VenueImplementationOverride;

/// Fetch a venue override by composite key
pub async fn get_venue_override(
    pool: &SqlitePool,
    venue_id: i64,
    pattern_id: i64,
) -> Result<VenueImplementationOverride, String> {
    sqlx::query_as::<_, VenueImplementationOverride>(
        "SELECT venue_id, pattern_id, implementation_id, remote_id, uid, created_at, updated_at
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
        "SELECT venue_id, pattern_id, implementation_id, remote_id, uid, created_at, updated_at
         FROM venue_implementation_overrides",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list venue_overrides: {}", e))
}

/// Set remote_id after syncing to cloud (by composite key)
pub async fn set_remote_id(
    pool: &SqlitePool,
    venue_id: i64,
    pattern_id: i64,
    remote_id: i64,
) -> Result<(), String> {
    sqlx::query("UPDATE venue_implementation_overrides SET remote_id = ? WHERE venue_id = ? AND pattern_id = ?")
        .bind(remote_id.to_string())
        .bind(venue_id)
        .bind(pattern_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set venue_override remote_id: {}", e))?;
    Ok(())
}
