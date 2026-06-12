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
            "Cannot delete group: it still contains {} members",
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
//
// A membership row covers either the whole fixture (head_index = WHOLE_FIXTURE)
// or one head of it (head_index >= 0). A whole-fixture row subsumes any
// per-head rows for the same (fixture, group).

/// Sentinel head_index meaning "every head of the fixture".
pub const WHOLE_FIXTURE: i64 = -1;

/// Add a fixture (head_index = [`WHOLE_FIXTURE`]) or one of its heads to a group.
pub async fn add_member_to_group(
    pool: &SqlitePool,
    fixture_id: &str,
    group_id: &str,
    head_index: i64,
) -> Result<(), String> {
    if head_index == WHOLE_FIXTURE {
        // Whole-fixture membership subsumes any per-head rows.
        sqlx::query(
            "DELETE FROM fixture_group_members
             WHERE fixture_id = ? AND group_id = ? AND head_index != ?",
        )
        .bind(fixture_id)
        .bind(group_id)
        .bind(WHOLE_FIXTURE)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear per-head rows: {}", e))?;
    } else {
        // Already covered by a whole-fixture row?
        let covered: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM fixture_group_members
             WHERE fixture_id = ? AND group_id = ? AND head_index = ?",
        )
        .bind(fixture_id)
        .bind(group_id)
        .bind(WHOLE_FIXTURE)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to check membership: {}", e))?;
        if covered.is_some() {
            return Ok(());
        }
    }

    // Get next display order within group
    let max_order: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(display_order) FROM fixture_group_members WHERE group_id = ?",
    )
    .bind(group_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to get max display order: {}", e))?;

    let display_order = max_order.unwrap_or(0) + 1;

    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT OR IGNORE INTO fixture_group_members (id, fixture_id, group_id, head_index, display_order)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(fixture_id)
    .bind(group_id)
    .bind(head_index)
    .bind(display_order)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to add member to group: {}", e))?;

    Ok(())
}

/// Remove membership rows for a fixture in a group. `head_index = None`
/// removes everything (whole-fixture and per-head rows alike);
/// `Some(h)` removes only that head's row.
pub async fn remove_member_from_group(
    pool: &SqlitePool,
    fixture_id: &str,
    group_id: &str,
    head_index: Option<i64>,
) -> Result<(), String> {
    let result = match head_index {
        None => {
            sqlx::query("DELETE FROM fixture_group_members WHERE fixture_id = ? AND group_id = ?")
                .bind(fixture_id)
                .bind(group_id)
                .execute(pool)
                .await
        }
        Some(h) => {
            sqlx::query(
                "DELETE FROM fixture_group_members
                 WHERE fixture_id = ? AND group_id = ? AND head_index = ?",
            )
            .bind(fixture_id)
            .bind(group_id)
            .bind(h)
            .execute(pool)
            .await
        }
    };
    result.map_err(|e| format!("Failed to remove member from group: {}", e))?;
    Ok(())
}

/// Replace a whole-fixture membership with explicit per-head rows.
/// Used when removing one head from a fixture that was added whole:
/// the -1 row is deleted and the remaining heads get their own rows.
/// Returns false (and does nothing) when there is no whole-fixture row.
pub async fn split_whole_fixture_membership(
    pool: &SqlitePool,
    fixture_id: &str,
    group_id: &str,
    keep_heads: &[i64],
) -> Result<bool, String> {
    let display_order: Option<i64> = sqlx::query_scalar(
        "SELECT display_order FROM fixture_group_members
         WHERE fixture_id = ? AND group_id = ? AND head_index = ?",
    )
    .bind(fixture_id)
    .bind(group_id)
    .bind(WHOLE_FIXTURE)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to read membership: {}", e))?;

    let Some(display_order) = display_order else {
        return Ok(false);
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start transaction: {}", e))?;

    sqlx::query(
        "DELETE FROM fixture_group_members
         WHERE fixture_id = ? AND group_id = ? AND head_index = ?",
    )
    .bind(fixture_id)
    .bind(group_id)
    .bind(WHOLE_FIXTURE)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("Failed to remove whole-fixture row: {}", e))?;

    for &h in keep_heads {
        sqlx::query(
            "INSERT OR IGNORE INTO fixture_group_members (id, fixture_id, group_id, head_index, display_order)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(fixture_id)
        .bind(group_id)
        .bind(h)
        .bind(display_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to insert head row: {}", e))?;
    }

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit: {}", e))?;
    Ok(true)
}

/// One membership row in a group, joined with its fixture.
pub struct GroupMember {
    pub fixture: PatchedFixture,
    pub head_index: i64,
}

/// Get all membership rows in a group (a fixture appears once per row:
/// once with [`WHOLE_FIXTURE`] or once per member head).
pub async fn get_members_in_group(
    pool: &SqlitePool,
    group_id: &str,
) -> Result<Vec<GroupMember>, String> {
    #[derive(FromRow)]
    struct Row {
        #[sqlx(flatten)]
        fixture: PatchedFixture,
        head_index: i64,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT f.id, f.uid, f.venue_id, f.universe, f.address, f.num_channels,
                f.manufacturer, f.model, f.mode_name, f.fixture_path, f.label,
                f.pos_x, f.pos_y, f.pos_z, f.rot_x, f.rot_y, f.rot_z,
                m.head_index
         FROM fixtures f
         JOIN fixture_group_members m ON f.id = m.fixture_id
         WHERE m.group_id = ?
         ORDER BY m.display_order, m.head_index",
    )
    .bind(group_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get members in group: {}", e))?;
    Ok(rows
        .into_iter()
        .map(|r| GroupMember {
            fixture: r.fixture,
            head_index: r.head_index,
        })
        .collect())
}

/// All membership rows for a venue: (fixture_id, normalized-or-raw group name, head_index).
/// Feeds the selection-expression cache.
pub async fn get_venue_memberships(
    pool: &SqlitePool,
    venue_id: &str,
) -> Result<Vec<(String, Option<String>, i64)>, String> {
    #[derive(FromRow)]
    struct Row {
        fixture_id: String,
        name: Option<String>,
        head_index: i64,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT m.fixture_id, g.name, m.head_index
         FROM fixture_group_members m
         JOIN fixture_groups g ON g.id = m.group_id
         WHERE g.venue_id = ?",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get venue memberships: {}", e))?;
    Ok(rows
        .into_iter()
        .map(|r| (r.fixture_id, r.name, r.head_index))
        .collect())
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
