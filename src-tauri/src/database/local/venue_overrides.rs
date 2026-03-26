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

// -----------------------------------------------------------------------------
// Delta sync support
// -----------------------------------------------------------------------------

/// List dirty venue overrides for the current user
pub async fn list_dirty_venue_overrides(
    pool: &SqlitePool,
    uid: &str,
) -> Result<Vec<VenueImplementationOverride>, String> {
    sqlx::query_as::<_, VenueImplementationOverride>(
        "SELECT venue_id, pattern_id, implementation_id, uid, created_at, updated_at
         FROM venue_implementation_overrides WHERE uid = ? AND (synced_at IS NULL OR updated_at > synced_at)",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list dirty venue_overrides: {}", e))
}

/// Mark a venue override as synced
pub async fn mark_venue_override_synced(
    pool: &SqlitePool,
    venue_id: &str,
    pattern_id: &str,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE venue_implementation_overrides SET synced_at = updated_at, version = version + 1 WHERE venue_id = ? AND pattern_id = ?",
    )
    .bind(venue_id)
    .bind(pattern_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark venue_override synced: {}", e))?;
    Ok(())
}
