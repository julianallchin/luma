use sqlx::SqlitePool;

use crate::models::fixtures::PatchedFixture;
use crate::models::tags::FixtureTag;

// -----------------------------------------------------------------------------
// Tag CRUD
// -----------------------------------------------------------------------------

/// Create a new tag in a venue
pub async fn create_tag(
    pool: &SqlitePool,
    venue_id: i64,
    name: &str,
    category: &str,
    is_auto_generated: bool,
) -> Result<FixtureTag, String> {
    sqlx::query(
        "INSERT INTO fixture_tags (venue_id, name, category, is_auto_generated)
         VALUES (?, ?, ?, ?)",
    )
    .bind(venue_id)
    .bind(name)
    .bind(category)
    .bind(is_auto_generated)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create tag: {}", e))?;

    // Get the inserted row
    let tag = sqlx::query_as::<_, FixtureTag>(
        "SELECT id, remote_id, uid, venue_id, name, category, is_auto_generated, created_at, updated_at
         FROM fixture_tags WHERE venue_id = ? AND name = ?",
    )
    .bind(venue_id)
    .bind(name)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch created tag: {}", e))?;

    Ok(tag)
}

/// Get a tag by ID
pub async fn get_tag(pool: &SqlitePool, id: i64) -> Result<FixtureTag, String> {
    sqlx::query_as::<_, FixtureTag>(
        "SELECT id, remote_id, uid, venue_id, name, category, is_auto_generated, created_at, updated_at
         FROM fixture_tags WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to get tag: {}", e))
}

/// Get a tag by name in a venue
pub async fn get_tag_by_name(
    pool: &SqlitePool,
    venue_id: i64,
    name: &str,
) -> Result<Option<FixtureTag>, String> {
    sqlx::query_as::<_, FixtureTag>(
        "SELECT id, remote_id, uid, venue_id, name, category, is_auto_generated, created_at, updated_at
         FROM fixture_tags WHERE venue_id = ? AND name = ?",
    )
    .bind(venue_id)
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to get tag by name: {}", e))
}

/// List all tags for a venue
pub async fn list_tags(pool: &SqlitePool, venue_id: i64) -> Result<Vec<FixtureTag>, String> {
    sqlx::query_as::<_, FixtureTag>(
        "SELECT id, remote_id, uid, venue_id, name, category, is_auto_generated, created_at, updated_at
         FROM fixture_tags WHERE venue_id = ? ORDER BY category, name",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list tags: {}", e))
}

/// List tags by category for a venue
pub async fn list_tags_by_category(
    pool: &SqlitePool,
    venue_id: i64,
    category: &str,
) -> Result<Vec<FixtureTag>, String> {
    sqlx::query_as::<_, FixtureTag>(
        "SELECT id, remote_id, uid, venue_id, name, category, is_auto_generated, created_at, updated_at
         FROM fixture_tags WHERE venue_id = ? AND category = ? ORDER BY name",
    )
    .bind(venue_id)
    .bind(category)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list tags by category: {}", e))
}

/// Update a tag
pub async fn update_tag(
    pool: &SqlitePool,
    id: i64,
    name: &str,
    category: &str,
) -> Result<FixtureTag, String> {
    sqlx::query("UPDATE fixture_tags SET name = ?, category = ? WHERE id = ?")
        .bind(name)
        .bind(category)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update tag: {}", e))?;

    get_tag(pool, id).await
}

/// Delete a tag (also removes all assignments)
pub async fn delete_tag(pool: &SqlitePool, id: i64) -> Result<(), String> {
    // Assignments are cascade deleted
    sqlx::query("DELETE FROM fixture_tags WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete tag: {}", e))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Tag Assignments
// -----------------------------------------------------------------------------

/// Assign a tag to a fixture
pub async fn assign_tag_to_fixture(
    pool: &SqlitePool,
    fixture_id: &str,
    tag_id: i64,
) -> Result<(), String> {
    sqlx::query(
        "INSERT OR IGNORE INTO fixture_tag_assignments (fixture_id, tag_id)
         VALUES (?, ?)",
    )
    .bind(fixture_id)
    .bind(tag_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to assign tag: {}", e))?;

    Ok(())
}

/// Remove a tag from a fixture
pub async fn remove_tag_from_fixture(
    pool: &SqlitePool,
    fixture_id: &str,
    tag_id: i64,
) -> Result<(), String> {
    sqlx::query("DELETE FROM fixture_tag_assignments WHERE fixture_id = ? AND tag_id = ?")
        .bind(fixture_id)
        .bind(tag_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to remove tag: {}", e))?;

    Ok(())
}

/// Get all tags for a fixture
pub async fn get_tags_for_fixture(
    pool: &SqlitePool,
    fixture_id: &str,
) -> Result<Vec<FixtureTag>, String> {
    sqlx::query_as::<_, FixtureTag>(
        "SELECT t.id, t.remote_id, t.uid, t.venue_id, t.name, t.category, t.is_auto_generated, t.created_at, t.updated_at
         FROM fixture_tags t
         JOIN fixture_tag_assignments a ON t.id = a.tag_id
         WHERE a.fixture_id = ?
         ORDER BY t.category, t.name",
    )
    .bind(fixture_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get tags for fixture: {}", e))
}

/// Get all fixtures with a specific tag
pub async fn get_fixtures_with_tag(
    pool: &SqlitePool,
    tag_id: i64,
) -> Result<Vec<PatchedFixture>, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT f.id, f.remote_id, f.uid, f.venue_id, f.universe, f.address, f.num_channels,
                f.manufacturer, f.model, f.mode_name, f.fixture_path, f.label,
                f.pos_x, f.pos_y, f.pos_z, f.rot_x, f.rot_y, f.rot_z
         FROM fixtures f
         JOIN fixture_tag_assignments a ON f.id = a.fixture_id
         WHERE a.tag_id = ?",
    )
    .bind(tag_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get fixtures with tag: {}", e))
}

/// Batch assign a tag to multiple fixtures
pub async fn batch_assign_tag(
    pool: &SqlitePool,
    fixture_ids: &[String],
    tag_id: i64,
) -> Result<(), String> {
    for fixture_id in fixture_ids {
        assign_tag_to_fixture(pool, fixture_id, tag_id).await?;
    }
    Ok(())
}

/// Clear all assignments for auto-generated tags in a venue (for regeneration)
pub async fn clear_auto_generated_tag_assignments(
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<(), String> {
    sqlx::query(
        "DELETE FROM fixture_tag_assignments
         WHERE tag_id IN (
             SELECT id FROM fixture_tags WHERE venue_id = ? AND is_auto_generated = 1
         )",
    )
    .bind(venue_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to clear auto-generated tag assignments: {}", e))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Tag Lookup for Selection
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Ensure Default Tags Exist
// -----------------------------------------------------------------------------

/// Ensure the standard spatial tags exist for a venue
pub async fn ensure_spatial_tags_exist(
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<FixtureTag>, String> {
    let spatial_tags = [
        "left", "right", "center", "front", "back", "high", "low", "circular",
    ];

    for name in &spatial_tags {
        let existing = get_tag_by_name(pool, venue_id, name).await?;
        if existing.is_none() {
            create_tag(pool, venue_id, name, "spatial", true).await?;
        }
    }

    // Also ensure 'all' meta tag exists
    let all_tag = get_tag_by_name(pool, venue_id, "all").await?;
    if all_tag.is_none() {
        create_tag(pool, venue_id, "all", "meta", true).await?;
    }

    list_tags_by_category(pool, venue_id, "spatial").await
}
