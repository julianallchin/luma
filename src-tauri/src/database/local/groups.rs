use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{
    normalize_group_name, validate_group_name, FixtureGroup, MovementConfig,
};

/// Database row for FixtureGroup
#[derive(FromRow)]
struct FixtureGroupRow {
    id: String,
    uid: Option<String>,
    venue_id: String,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
    movement_config: Option<String>,
    display_order: i64,
    created_at: String,
    updated_at: String,
}

impl From<FixtureGroupRow> for FixtureGroup {
    fn from(row: FixtureGroupRow) -> Self {
        let movement_config: Option<MovementConfig> = row
            .movement_config
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        FixtureGroup {
            id: row.id,
            uid: row.uid,
            venue_id: row.venue_id,
            name: row.name,
            axis_lr: row.axis_lr,
            axis_fb: row.axis_fb,
            axis_ab: row.axis_ab,
            movement_config,
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
    venue_id: &str,
    name: Option<&str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    // Normalize and validate the name if provided
    let normalized_name = name.map(|n| {
        let norm = normalize_group_name(n);
        if norm.is_empty() {
            n.to_string()
        } else {
            norm
        }
    });
    if let Some(ref n) = normalized_name {
        validate_group_name(n)?;
    }

    // Get next display order
    let max_order: Option<i64> =
        sqlx::query_scalar("SELECT MAX(display_order) FROM fixture_groups WHERE venue_id = ?")
            .bind(venue_id)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Failed to get max display order: {}", e))?;

    let display_order = max_order.unwrap_or(0) + 1;

    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO fixture_groups (id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, display_order)
         VALUES (?, (SELECT uid FROM venues WHERE id = ?), ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(venue_id)
    .bind(venue_id)
    .bind(normalized_name.as_deref())
    .bind(axis_lr)
    .bind(axis_fb)
    .bind(axis_ab)
    .bind(display_order)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create group: {}", e))?;

    get_group(pool, &id).await
}

/// Get a group by ID
pub async fn get_group(pool: &SqlitePool, id: &str) -> Result<FixtureGroup, String> {
    let row = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, movement_config, display_order, created_at, updated_at
         FROM fixture_groups WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to get group: {}", e))?;
    Ok(row.into())
}

/// List all groups for a venue
pub async fn list_groups(pool: &SqlitePool, venue_id: &str) -> Result<Vec<FixtureGroup>, String> {
    let rows = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, movement_config, display_order, created_at, updated_at
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
    id: &str,
    name: Option<&str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, String> {
    // Normalize and validate the name if provided
    let normalized_name = name.map(|n| {
        let norm = normalize_group_name(n);
        if norm.is_empty() {
            n.to_string()
        } else {
            norm
        }
    });
    if let Some(ref n) = normalized_name {
        validate_group_name(n)?;
    }

    sqlx::query(
        "UPDATE fixture_groups SET name = ?, axis_lr = ?, axis_fb = ?, axis_ab = ? WHERE id = ?",
    )
    .bind(normalized_name.as_deref())
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
pub async fn delete_group(pool: &SqlitePool, id: &str) -> Result<(), String> {
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

// -----------------------------------------------------------------------------
// Membership
// -----------------------------------------------------------------------------

/// Add a fixture to a group
pub async fn add_fixture_to_group(
    pool: &SqlitePool,
    fixture_id: &str,
    group_id: &str,
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
    group_id: &str,
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
    group_id: &str,
) -> Result<Vec<PatchedFixture>, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT f.id, f.uid, f.venue_id, f.universe, f.address, f.num_channels,
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
        "SELECT g.id, g.uid, g.venue_id, g.name, g.axis_lr, g.axis_fb, g.axis_ab,
                g.movement_config, g.display_order, g.created_at, g.updated_at
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

/// Get fixtures not in any group for a venue
pub async fn get_ungrouped_fixtures(
    pool: &SqlitePool,
    venue_id: &str,
) -> Result<Vec<PatchedFixture>, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT f.id, f.uid, f.venue_id, f.universe, f.address, f.num_channels,
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

/// Update movement config for a group
pub async fn update_movement_config(
    pool: &SqlitePool,
    group_id: &str,
    config: Option<&MovementConfig>,
) -> Result<FixtureGroup, String> {
    let config_json = config
        .map(|c| serde_json::to_string(c).map_err(|e| format!("Failed to serialize config: {}", e)))
        .transpose()?;

    sqlx::query("UPDATE fixture_groups SET movement_config = ? WHERE id = ?")
        .bind(&config_json)
        .bind(group_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update movement config: {}", e))?;

    get_group(pool, group_id).await
}

// -----------------------------------------------------------------------------
// Delta sync support
// -----------------------------------------------------------------------------

/// List dirty groups for a venue
pub async fn list_dirty_groups(
    pool: &SqlitePool,
    venue_id: &str,
) -> Result<Vec<FixtureGroup>, String> {
    let rows = sqlx::query_as::<_, FixtureGroupRow>(
        "SELECT id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, movement_config, display_order, created_at, updated_at
         FROM fixture_groups WHERE venue_id = ? AND (synced_at IS NULL OR updated_at > synced_at)",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list dirty groups: {}", e))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Mark a group as synced
pub async fn mark_group_synced(pool: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query(
        "UPDATE fixture_groups SET synced_at = updated_at, version = version + 1 WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark group synced: {}", e))?;
    Ok(())
}

/// Get group member fixture_ids and display_orders for cloud sync
pub async fn get_group_member_ids(
    pool: &SqlitePool,
    group_id: &str,
) -> Result<Vec<(String, i64)>, String> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT m.fixture_id, m.display_order
         FROM fixture_group_members m
         WHERE m.group_id = ?
         ORDER BY m.display_order",
    )
    .bind(group_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get group member ids: {}", e))?;

    Ok(rows)
}
