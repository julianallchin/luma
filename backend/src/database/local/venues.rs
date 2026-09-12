use uuid::Uuid;

use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::venues::Venue;
use luma_render::scene_desc::VenueEnvironment;

/// `role` is not a column read here: it is derived per reader — see
/// [`get_venue`] — so a venue the signed-in principal does not own reads as a
/// member's however it was written.
const VENUE_COLUMNS: &str =
    "id, uid, name, description, share_code, 'owner' AS role, controller_port, mixer_port, mixer_mapping_json, environment, created_at, updated_at";

/// Fetch a single venue by ID
pub async fn get_venue(access: &mut impl AuthorizedVenue) -> Result<Venue, String> {
    let venue_id = access.venue_id().to_string();
    let row = sqlx::query_as::<_, Venue>(
        "SELECT venue.id, venue.uid, venue.name, venue.description, venue.share_code,
                CASE WHEN venue.uid = admission.active_uid THEN 'owner' ELSE 'member' END AS role,
                venue.controller_port, venue.mixer_port, venue.mixer_mapping_json,
                venue.environment, venue.created_at, venue.updated_at
         FROM venues venue
         CROSS JOIN auth_write_admission admission
         WHERE venue.id = ? AND admission.singleton = 1",
    )
    .bind(venue_id)
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to fetch venue: {}", e))?;

    Ok(row)
}

/// The venues the admitted principal owns or is a member of.
pub async fn list_venues(pool: &sqlx::SqlitePool) -> Result<Vec<Venue>, String> {
    let rows = sqlx::query_as::<_, Venue>(
        "SELECT venue.id, venue.uid, venue.name, venue.description, venue.share_code,
                CASE WHEN venue.uid = admission.active_uid THEN 'owner' ELSE 'member' END AS role,
                venue.controller_port, venue.mixer_port, venue.mixer_mapping_json,
                venue.environment, venue.created_at, venue.updated_at
         FROM venues venue
         CROSS JOIN auth_write_admission admission
         WHERE admission.singleton = 1
           AND admission.armed = 1
           AND admission.accepting = 1
           AND admission.maintenance = 0
           AND (
                venue.uid = admission.active_uid
                OR EXISTS(
                    SELECT 1 FROM venue_members membership
                    WHERE membership.venue_id = venue.id
                      AND membership.uid = admission.active_uid
                )
           )
         ORDER BY venue.updated_at DESC",
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
) -> Result<Venue, String> {
    let id = Uuid::new_v4().to_string();
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin venue creation: {error}"))?;
    let principal: Option<String> = sqlx::query_scalar(
        "SELECT active_uid FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 1 AND maintenance = 0",
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize venue creation: {error}"))?
    .ok_or_else(|| "Venue creation is not currently admitted".to_string())?;

    sqlx::query("INSERT INTO venues (id, name, description, uid, groups_initialized) VALUES (?, ?, ?, ?, 1)")
        .bind(&id)
        .bind(&name)
        .bind(&description)
        .bind(&principal)
        .execute(&mut *transaction)
        .await
        .map_err(|e| format!("Failed to create venue: {}", e))?;
    let query = format!("SELECT {} FROM venues WHERE id = ?", VENUE_COLUMNS);
    let venue = sqlx::query_as::<_, Venue>(sqlx::AssertSqlSafe(query))
        .bind(&id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to read created venue: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit venue creation: {error}"))?;
    Ok(venue)
}

/// Update a venue
pub async fn update_venue(
    access: &mut VenueAccess<'_, Write>,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query("UPDATE venues SET name = ?, description = ? WHERE id = ?")
        .bind(&name)
        .bind(&description)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to update venue: {}", e))?;

    get_venue(access).await
}

/// Delete only an unused venue catalog entry owned by the trusted principal.
/// Scores, conversations, and authored revision history are durable state; none
/// may disappear as a side effect of the venue foreign-key cascade.
pub async fn delete_venue(access: &mut VenueAccess<'_, Write>) -> Result<(), String> {
    let id = access.venue_id().to_string();

    let scores: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scores WHERE venue_id = ?")
        .bind(&id)
        .fetch_one(&mut *access.connection())
        .await
        .map_err(|error| format!("Failed to inspect venue scores: {error}"))?;
    if scores != 0 {
        return Err(
            "Venue still owns scores; delete only empty, non-authored venue containers".into(),
        );
    }

    let threads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_threads WHERE venue_id = ?")
        .bind(&id)
        .fetch_one(&mut *access.connection())
        .await
        .map_err(|error| format!("Failed to inspect venue conversations: {error}"))?;
    if threads != 0 {
        return Err(
            "Venue still owns durable conversations; delete those conversations first".into(),
        );
    }

    // Child admission triggers resolve their venue through the still-live
    // parent. During an ON DELETE cascade that parent is already gone, so the
    // authorized aggregate deletion uses a transaction-local maintenance
    // capability and restores ordinary admission before commit.
    access.enter_maintenance().await?;
    let deleted = sqlx::query("DELETE FROM venues WHERE id = ?")
        .bind(&id)
        .execute(&mut *access.connection())
        .await
        .map_err(|error| format!("Failed to delete venue: {error}"))?;
    if deleted.rows_affected() != 1 {
        return Err(format!("Venue {id} not found"));
    }
    access.leave_maintenance().await?;

    Ok(())
}

/// Set the share_code for a venue
pub async fn set_share_code(access: &mut VenueAccess<'_, Write>, code: &str) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query("UPDATE venues SET share_code = ? WHERE id = ?")
        .bind(code)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to set venue share_code: {}", e))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Venue memberships
// -----------------------------------------------------------------------------

/// Remove only the active principal's own joined-venue membership. This is a
/// membership lifecycle operation, not authority to mutate the venue
/// aggregate, so it deliberately does not manufacture a write guard.
pub async fn remove_current_venue_membership(
    pool: &sqlx::SqlitePool,
    venue_id: &str,
    principal: &str,
) -> Result<(), String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin venue leave: {error}"))?;
    let admitted: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM auth_write_admission admission
             JOIN venues venue ON venue.id = ?
             JOIN venue_members membership
               ON membership.venue_id = venue.id AND membership.uid = ?
              AND membership.role = 'member'
             WHERE admission.singleton = 1 AND admission.armed = 1
               AND admission.accepting = 1 AND admission.maintenance = 0
               AND admission.remote_writes = 0 AND admission.active_uid = ?
               AND venue.uid IS NOT ?
         )",
    )
    .bind(venue_id)
    .bind(principal)
    .bind(principal)
    .bind(principal)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize venue leave: {error}"))?;
    if admitted != 1 {
        return Err("Venue resource not found".into());
    }
    let deleted = sqlx::query("DELETE FROM venue_members WHERE venue_id = ? AND uid = ?")
        .bind(venue_id)
        .bind(principal)
        .execute(&mut *transaction)
        .await
        .map_err(|e| format!("Failed to remove venue membership: {}", e))?;
    if deleted.rows_affected() != 1 {
        return Err("Venue resource not found".into());
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit venue leave: {error}"))?;
    Ok(())
}

/// Set the preferred MIDI controller port for a venue (local-only, not synced).
pub async fn set_controller_port(
    access: &mut VenueAccess<'_, Write>,
    port: Option<&str>,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query("UPDATE venues SET controller_port = ? WHERE id = ?")
        .bind(port)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to set controller port: {}", e))?;
    Ok(())
}

/// Set this venue's lighting environment, synced with the venue.
///
/// One write for both modes, because the value is one closed enum: switching a
/// room from indoor to outdoor and moving its one dial are the same edit, and a
/// pair of setters would let a caller write a mode without the scalar that mode
/// needs.
///
/// The ordinary venue dirtiness trigger schedules delivery of this edit.
pub async fn set_environment(
    access: &mut VenueAccess<'_, Write>,
    environment: VenueEnvironment,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query("UPDATE venues SET environment = ? WHERE id = ?")
        .bind(environment.to_record())
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to set venue environment: {e}"))?;
    Ok(())
}

/// Set the MIDI mixer port + mapping for a venue (local-only, not synced).
pub async fn set_mixer_config(
    access: &mut VenueAccess<'_, Write>,
    port: Option<&str>,
    mapping_json: Option<&str>,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query("UPDATE venues SET mixer_port = ?, mixer_mapping_json = ? WHERE id = ?")
        .bind(port)
        .bind(mapping_json)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to set mixer config: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    async fn test_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let directory = tempfile::tempdir().expect("tempdir");
        let database_path = directory.path().join("venues.db");
        let migration_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .expect("migration pool");
        sqlx::migrate!("./migrations")
            .run(&migration_pool)
            .await
            .expect("migrations");
        migration_pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .expect("pool");
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .expect("arm alice");
        (directory, pool)
    }

    async fn insert_owned_venue(pool: &sqlx::SqlitePool) {
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'Venue')")
            .execute(pool)
            .await
            .unwrap();
    }

    async fn venue_exists(pool: &sqlx::SqlitePool) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM venues WHERE id = 'venue'")
            .fetch_one(pool)
            .await
            .unwrap()
            == 1
    }

    async fn delete_owned_venue(pool: &sqlx::SqlitePool) -> Result<(), String> {
        let mut access = crate::database::local::venue_access::VenueAccess::<
            crate::database::local::venue_access::Write,
        >::write(
            pool,
            crate::database::local::venue_access::VenueResource::Venue("venue"),
        )
        .await?;
        delete_venue(&mut access).await?;
        access.commit().await
    }

    #[tokio::test]
    async fn venue_deletion_requires_the_trusted_owner() {
        let (_directory, pool) = test_pool().await;
        insert_owned_venue(&pool).await;

        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        let error = delete_owned_venue(&pool).await.unwrap_err();
        assert_eq!(error, "Venue resource not found");
        assert!(venue_exists(&pool).await);
    }

    #[tokio::test]
    async fn venue_deletion_refuses_scores_that_would_cascade() {
        let (_directory, pool) = test_pool().await;
        insert_owned_venue(&pool).await;
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path)
             VALUES ('track', 'alice', 'hash', '/track')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('score', 'alice', 'track', 'venue', 'Score')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let error = delete_owned_venue(&pool).await.unwrap_err();
        assert!(error.contains("still owns scores"));
        assert!(venue_exists(&pool).await);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = 'score'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn venue_deletion_refuses_durable_threads() {
        let (_directory, pool) = test_pool().await;
        insert_owned_venue(&pool).await;
        sqlx::query(
            "INSERT INTO agent_threads
             (id, uid, agent_kind, subject_kind, subject_id, venue_id, score_id)
             VALUES ('thread', 'alice', 'track_copilot', 'track', 'track', 'venue', 'score')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let error = delete_owned_venue(&pool).await.unwrap_err();
        assert!(error.contains("durable conversations"));
        assert!(venue_exists(&pool).await);
    }

    #[tokio::test]
    async fn venue_deletion_removes_an_empty_owned_venue() {
        let (_directory, pool) = test_pool().await;
        insert_owned_venue(&pool).await;

        delete_owned_venue(&pool).await.unwrap();
        assert!(!venue_exists(&pool).await);
    }
}
