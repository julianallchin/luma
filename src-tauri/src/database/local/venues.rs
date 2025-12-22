use crate::models::venues::Venue;

/// Fetch a single venue by ID
pub async fn get_venue(pool: &sqlx::SqlitePool, id: i64) -> Result<Venue, String> {
    let row = sqlx::query_as::<_, Venue>(
        "SELECT id, remote_id, uid, name, description, created_at, updated_at FROM venues WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch venue: {}", e))?;

    Ok(row)
}

/// List all venues
pub async fn list_venues(pool: &sqlx::SqlitePool) -> Result<Vec<Venue>, String> {
    let rows = sqlx::query_as::<_, Venue>(
        "SELECT id, remote_id, uid, name, description, created_at, updated_at FROM venues ORDER BY updated_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list venues: {}", e))?;

    Ok(rows)
}

/// Create a new venue
pub async fn create_venue(
    pool: &sqlx::SqlitePool,
    name: String,
    description: Option<String>,
    uid: Option<String>,
) -> Result<Venue, String> {
    // remote_id is None until synced to cloud (stores cloud's BIGINT id as string)
    let id = sqlx::query("INSERT INTO venues (name, description, uid) VALUES (?, ?, ?)")
        .bind(&name)
        .bind(&description)
        .bind(&uid)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create venue: {}", e))?
        .last_insert_rowid();

    get_venue(pool, id).await
}

/// Update a venue
pub async fn update_venue(
    pool: &sqlx::SqlitePool,
    id: i64,
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
pub async fn delete_venue(pool: &sqlx::SqlitePool, id: i64) -> Result<(), String> {
    sqlx::query("DELETE FROM venues WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete venue: {}", e))?;

    Ok(())
}
