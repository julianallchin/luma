use sqlx::{FromRow, SqlitePool};

use crate::models::fixtures::PatchedFixture;
use crate::models::groups::FixtureGroup;

/// Database row for FixtureGroup (tags stored as JSON string)
#[derive(FromRow)]
struct FixtureGroupRow {
    id: i64,
    remote_id: Option<String>,
    uid: Option<String>,
    venue_id: i64,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
    tags: String,
    display_order: i64,
    created_at: String,
    updated_at: String,
}

impl From<FixtureGroupRow> for FixtureGroup {
    fn from(row: FixtureGroupRow) -> Self {
        let tags: Vec<String> = serde_json::from_str(&row.tags).unwrap_or_default();
        FixtureGroup {
            id: row.id,
            remote_id: row.remote_id,
            uid: row.uid,
            venue_id: row.venue_id,
            name: row.name,
            axis_lr: row.axis_lr,
            axis_fb: row.axis_fb,
            axis_ab: row.axis_ab,
            tags,
            display_order: row.display_order,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// -----------------------------------------------------------------------------
// Group CRUD
// -----------------------------------------------------------------------------

/// Create a new fixture group in a venue
pub async fn create_group(
    pool: &SqlitePool,
    venue_id: i64,
    name: Option<&str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    // Get next display order
    let max_order: Option<i64> =
        sqlx::query_scalar("SELECT MAX(display_order) FROM fixture_groups WHERE venue_id = ?")
            .bind(venue_id)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Failed to get max display order: {}", e))?;

    let display_order = max_order.unwrap_or(0) + 1;

    sqlx::query(
        "INSERT INTO fixture_groups (venue_id, name, axis_lr, axis_fb, axis_ab, display_order)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(venue_id)
    .bind(name)
    .bind(axis_lr)
    .bind(axis_fb)
    .bind(axis_ab)
    .bind(display_order)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create group: {}", e))?;

    // Get the inserted row
    let row = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT id, remote_id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, tags, display_order, created_at, updated_at
         FROM fixture_groups WHERE venue_id = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(venue_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch created group: {}", e))?;

    Ok(row.into())
}

/// Get a group by ID
pub async fn get_group(pool: &SqlitePool, id: i64) -> Result<FixtureGroup, String> {
    let row = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT id, remote_id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, tags, display_order, created_at, updated_at
         FROM fixture_groups WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to get group: {}", e))?;
    Ok(row.into())
}

/// List all groups for a venue
pub async fn list_groups(pool: &SqlitePool, venue_id: i64) -> Result<Vec<FixtureGroup>, String> {
    let rows = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT id, remote_id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, tags, display_order, created_at, updated_at
         FROM fixture_groups WHERE venue_id = ? ORDER BY display_order",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list groups: {}", e))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Update a group
pub async fn update_group(
    pool: &SqlitePool,
    id: i64,
    name: Option<&str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    sqlx::query(
        "UPDATE fixture_groups SET name = ?, axis_lr = ?, axis_fb = ?, axis_ab = ? WHERE id = ?",
    )
    .bind(name)
    .bind(axis_lr)
    .bind(axis_fb)
    .bind(axis_ab)
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update group: {}", e))?;

    get_group(pool, id).await
}

/// Delete a group (only if empty)
pub async fn delete_group(pool: &SqlitePool, id: i64) -> Result<(), String> {
    // Check if group has fixtures
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM fixture_group_members WHERE group_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Failed to check group membership: {}", e))?;

    if count > 0 {
        return Err(format!(
            "Cannot delete group: it still contains {} fixtures",
            count
        ));
    }

    sqlx::query("DELETE FROM fixture_groups WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete group: {}", e))?;

    Ok(())
}

/// Get or create the default group for a venue
pub async fn get_or_create_default_group(
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<FixtureGroup, String> {
    // Try to find existing default group
    let existing = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT id, remote_id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, tags, display_order, created_at, updated_at
         FROM fixture_groups WHERE venue_id = ? AND name = 'Default' LIMIT 1",
    )
    .bind(venue_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to find default group: {}", e))?;

    if let Some(row) = existing {
        return Ok(row.into());
    }

    // Create default group
    create_group(
        pool,
        venue_id,
        Some("Default"),
        Some(0.0),
        Some(0.0),
        Some(0.0),
    )
    .await
}

// -----------------------------------------------------------------------------
// Membership
// -----------------------------------------------------------------------------

/// Add a fixture to a group
pub async fn add_fixture_to_group(
    pool: &SqlitePool,
    fixture_id: &str,
    group_id: i64,
) -> Result<(), String> {
    // Get next display order within group
    let max_order: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(display_order) FROM fixture_group_members WHERE group_id = ?",
    )
    .bind(group_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to get max display order: {}", e))?;

    let display_order = max_order.unwrap_or(0) + 1;

    sqlx::query(
        "INSERT OR IGNORE INTO fixture_group_members (fixture_id, group_id, display_order)
         VALUES (?, ?, ?)",
    )
    .bind(fixture_id)
    .bind(group_id)
    .bind(display_order)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to add fixture to group: {}", e))?;

    Ok(())
}

/// Remove a fixture from a group
pub async fn remove_fixture_from_group(
    pool: &SqlitePool,
    fixture_id: &str,
    group_id: i64,
) -> Result<(), String> {
    sqlx::query("DELETE FROM fixture_group_members WHERE fixture_id = ? AND group_id = ?")
        .bind(fixture_id)
        .bind(group_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to remove fixture from group: {}", e))?;

    Ok(())
}

/// Get all fixtures in a group
pub async fn get_fixtures_in_group(
    pool: &SqlitePool,
    group_id: i64,
) -> Result<Vec<PatchedFixture>, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT f.id, f.remote_id, f.uid, f.venue_id, f.universe, f.address, f.num_channels,
                f.manufacturer, f.model, f.mode_name, f.fixture_path, f.label,
                f.pos_x, f.pos_y, f.pos_z, f.rot_x, f.rot_y, f.rot_z
         FROM fixtures f
         JOIN fixture_group_members m ON f.id = m.fixture_id
         WHERE m.group_id = ?
         ORDER BY m.display_order",
    )
    .bind(group_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get fixtures in group: {}", e))
}

/// Get all groups that a fixture belongs to
pub async fn get_groups_for_fixture(
    pool: &SqlitePool,
    fixture_id: &str,
) -> Result<Vec<FixtureGroup>, String> {
    let rows = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT g.id, g.remote_id, g.uid, g.venue_id, g.name, g.axis_lr, g.axis_fb, g.axis_ab, g.tags,
                g.display_order, g.created_at, g.updated_at
         FROM fixture_groups g
         JOIN fixture_group_members m ON g.id = m.group_id
         WHERE m.fixture_id = ?
         ORDER BY g.display_order",
    )
    .bind(fixture_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get groups for fixture: {}", e))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Get count of fixtures in a group
pub async fn get_fixture_count_in_group(pool: &SqlitePool, group_id: i64) -> Result<i64, String> {
    sqlx::query_scalar("SELECT COUNT(*) FROM fixture_group_members WHERE group_id = ?")
        .bind(group_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to get fixture count: {}", e))
}

// -----------------------------------------------------------------------------
// Queries for Selection
// -----------------------------------------------------------------------------

/// Get all groups with their fixture counts for a venue
pub async fn get_groups_with_counts(
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<(FixtureGroup, i64)>, String> {
    let groups = list_groups(pool, venue_id).await?;
    let mut result = Vec::with_capacity(groups.len());

    for group in groups {
        let count = get_fixture_count_in_group(pool, group.id).await?;
        result.push((group, count));
    }

    Ok(result)
}

/// Get fixtures not in any group for a venue
pub async fn get_ungrouped_fixtures(
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<PatchedFixture>, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT f.id, f.remote_id, f.uid, f.venue_id, f.universe, f.address, f.num_channels,
                f.manufacturer, f.model, f.mode_name, f.fixture_path, f.label,
                f.pos_x, f.pos_y, f.pos_z, f.rot_x, f.rot_y, f.rot_z
         FROM fixtures f
         WHERE f.venue_id = ?
           AND NOT EXISTS (
               SELECT 1 FROM fixture_group_members m WHERE m.fixture_id = f.id
           )",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get ungrouped fixtures: {}", e))
}

// -----------------------------------------------------------------------------
// Tags
// -----------------------------------------------------------------------------

/// Update tags for a group
pub async fn set_group_tags(
    pool: &SqlitePool,
    group_id: i64,
    tags: &[String],
) -> Result<FixtureGroup, String> {
    let tags_json = serde_json::to_string(tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    sqlx::query("UPDATE fixture_groups SET tags = ? WHERE id = ?")
        .bind(&tags_json)
        .bind(group_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update group tags: {}", e))?;

    get_group(pool, group_id).await
}

/// Add a tag to a group
pub async fn add_tag_to_group(
    pool: &SqlitePool,
    group_id: i64,
    tag: &str,
) -> Result<FixtureGroup, String> {
    let group = get_group(pool, group_id).await?;
    let mut tags = group.tags;
    if !tags.contains(&tag.to_string()) {
        tags.push(tag.to_string());
    }
    set_group_tags(pool, group_id, &tags).await
}

/// Remove a tag from a group
pub async fn remove_tag_from_group(
    pool: &SqlitePool,
    group_id: i64,
    tag: &str,
) -> Result<FixtureGroup, String> {
    let group = get_group(pool, group_id).await?;
    let tags: Vec<String> = group.tags.into_iter().filter(|t| t != tag).collect();
    set_group_tags(pool, group_id, &tags).await
}
