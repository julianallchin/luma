use uuid::Uuid;

use crate::models::venues::Venue;

const VENUE_COLUMNS: &str = "id, uid, name, description, share_code, role, created_at, updated_at";

/// Fetch a single venue by ID
pub async fn get_venue(pool: &sqlx::SqlitePool, id: &str) -> Result<Venue, String> {
    let query = format!("SELECT {} FROM venues WHERE id = ?", VENUE_COLUMNS);
    let row = sqlx::query_as::<_, Venue>(&query)
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
    let rows = sqlx::query_as::<_, Venue>(&query)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to list venues: {}", e))?;

    Ok(rows)
}

/// List venues belonging to a specific user (owned or joined)
pub async fn list_venues_for_user(
    pool: &sqlx::SqlitePool,
    uid: &str,
) -> Result<Vec<Venue>, String> {
    let query = format!(
        "SELECT {} FROM venues WHERE uid = ? ORDER BY updated_at DESC",
        VENUE_COLUMNS
    );
    let rows = sqlx::query_as::<_, Venue>(&query)
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

/// Insert a venue from a cloud join operation (role = 'member')
pub async fn insert_joined_venue(
    pool: &sqlx::SqlitePool,
    id: &str,
    uid: &str,
    name: &str,
    description: Option<&str>,
    share_code: Option<&str>,
) -> Result<Venue, String> {
    sqlx::query(
        "INSERT INTO venues (id, uid, name, description, share_code, role) VALUES (?, ?, ?, ?, ?, 'member')",
    )
    .bind(id)
    .bind(uid)
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
