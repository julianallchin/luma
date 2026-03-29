use uuid::Uuid;

use crate::models::venues::Venue;

const VENUE_COLUMNS: &str = "id, uid, name, description, share_code, role, created_at, updated_at";

/// Fetch a single venue by ID
pub async fn get_venue(pool: &sqlx::SqlitePool, id: &str) -> Result<Venue, String> {
    let query = format!("SELECT {} FROM venues WHERE id = ?", VENUE_COLUMNS);
    let row = sqlx::query_as::<_, Venue>(sqlx::AssertSqlSafe(query))
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to fetch venue: {}", e))?;

    Ok(row)
}

/// List all venues
pub async fn list_venues(pool: &sqlx::SqlitePool) -> Result<Vec<Venue>, String> {
    let query = format!(
        "SELECT {} FROM venues ORDER BY updated_at DESC",
        VENUE_COLUMNS
    );
    let rows = sqlx::query_as::<_, Venue>(sqlx::AssertSqlSafe(query))
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list venues: {}", e))?;

    Ok(rows)
}

/// List venues belonging to a specific user (owned or joined via membership)
pub async fn list_venues_for_user(
    pool: &sqlx::SqlitePool,
    uid: &str,
) -> Result<Vec<Venue>, String> {
    let query = format!(
        "SELECT {cols} FROM venues WHERE uid = ?
         UNION
         SELECT {cols} FROM venues
         WHERE id IN (SELECT venue_id FROM venue_memberships WHERE user_id = ?)
         ORDER BY updated_at DESC",
        cols = VENUE_COLUMNS
    );
    let rows = sqlx::query_as::<_, Venue>(sqlx::AssertSqlSafe(query))
        .bind(uid)
        .bind(uid)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list venues for user: {}", e))?;

    Ok(rows)
}

/// Create a new venue
pub async fn create_venue(
    pool: &sqlx::SqlitePool,
    name: String,
    description: Option<String>,
    uid: Option<String>,
) -> Result<Venue, String> {
    let id = Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO venues (id, name, description, uid) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(&description)
        .bind(&uid)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create venue: {}", e))?;

    get_venue(pool, &id).await
}

/// Insert a venue from a cloud join operation (role = 'member').
/// `uid` should be the venue OWNER's uid (not the joiner's).
/// Uses ON CONFLICT for idempotency (re-joining updates name/description).
pub async fn insert_joined_venue(
    pool: &sqlx::SqlitePool,
    id: &str,
    owner_uid: &str,
    name: &str,
    description: Option<&str>,
    share_code: Option<&str>,
) -> Result<Venue, String> {
    sqlx::query(
        "INSERT INTO venues (id, uid, name, description, share_code, role) VALUES (?, ?, ?, ?, ?, 'member')
         ON CONFLICT(id) DO UPDATE SET
           uid = excluded.uid,
           name = excluded.name,
           description = excluded.description",
    )
    .bind(id)
    .bind(owner_uid)
    .bind(name)
    .bind(description)
    .bind(share_code)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to insert joined venue: {}", e))?;

    get_venue(pool, id).await
}

/// Update a venue
pub async fn update_venue(
    pool: &sqlx::SqlitePool,
    id: &str,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    sqlx::query("UPDATE venues SET name = ?, description = ? WHERE id = ?")
        .bind(&name)
        .bind(&description)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update venue: {}", e))?;

    get_venue(pool, id).await
}

/// Delete a venue (cascades to fixtures)
pub async fn delete_venue(pool: &sqlx::SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM venues WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete venue: {}", e))?;

    Ok(())
}

/// Set the share_code for a venue
pub async fn set_share_code(pool: &sqlx::SqlitePool, id: &str, code: &str) -> Result<(), String> {
    sqlx::query("UPDATE venues SET share_code = ? WHERE id = ?")
        .bind(code)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set venue share_code: {}", e))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Venue memberships
// -----------------------------------------------------------------------------

/// Add a venue membership record for a user
pub async fn add_venue_membership(
    pool: &sqlx::SqlitePool,
    venue_id: &str,
    user_id: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO venue_memberships (venue_id, user_id, role) VALUES (?, ?, 'member')
         ON CONFLICT(venue_id, user_id) DO NOTHING",
    )
    .bind(venue_id)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to add venue membership: {}", e))?;
    Ok(())
}

/// Remove a venue membership record for a user
pub async fn remove_venue_membership(
    pool: &sqlx::SqlitePool,
    venue_id: &str,
    user_id: &str,
) -> Result<(), String> {
    sqlx::query("DELETE FROM venue_memberships WHERE venue_id = ? AND user_id = ?")
        .bind(venue_id)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to remove venue membership: {}", e))?;
    Ok(())
}

/// Count the number of memberships for a venue
pub async fn count_venue_memberships(
    pool: &sqlx::SqlitePool,
    venue_id: &str,
) -> Result<i64, String> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM venue_memberships WHERE venue_id = ?")
            .bind(venue_id)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Failed to count venue memberships: {}", e))?;
    Ok(count)
}
