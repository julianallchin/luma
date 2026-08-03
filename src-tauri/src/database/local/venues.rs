use uuid::Uuid;

use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::venues::Venue;

const VENUE_COLUMNS: &str =
    "id, uid, name, description, share_code, role, controller_port, mixer_port, mixer_mapping_json, created_at, updated_at";

/// Fetch a single venue by ID
pub async fn get_venue(access: &mut impl AuthorizedVenue) -> Result<Venue, String> {
    let venue_id = access.venue_id().to_string();
    let row = sqlx::query_as::<_, Venue>(
        "SELECT venue.id, venue.uid, venue.name, venue.description, venue.share_code,
                CASE
                    WHEN admission.active_uid IS NULL OR venue.uid = admission.active_uid
                    THEN 'owner'
                    ELSE 'member'
                END AS role,
                venue.controller_port, venue.mixer_port, venue.mixer_mapping_json,
                venue.created_at, venue.updated_at
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

/// List exactly the venues admitted for the app database's current host-bound
/// principal. Signed-out guest state never includes cached account venues.
pub async fn list_venues(pool: &sqlx::SqlitePool) -> Result<Vec<Venue>, String> {
    let rows = sqlx::query_as::<_, Venue>(
        "SELECT venue.id, venue.uid, venue.name, venue.description, venue.share_code,
                CASE
                    WHEN admission.active_uid IS NULL OR venue.uid = admission.active_uid
                    THEN 'owner'
                    ELSE 'member'
                END AS role,
                venue.controller_port, venue.mixer_port, venue.mixer_mapping_json,
                venue.created_at, venue.updated_at
         FROM venues venue
         CROSS JOIN auth_write_admission admission
         WHERE admission.singleton = 1
           AND admission.armed = 1
           AND admission.accepting = 1
           AND admission.maintenance = 0
           AND (
                (admission.active_uid IS NULL
                 AND venue.uid IS NULL AND venue.role != 'member')
                OR
                (admission.active_uid IS NOT NULL AND (
                    venue.uid = admission.active_uid
                    OR EXISTS(
                        SELECT 1 FROM venue_memberships membership
                        WHERE membership.venue_id = venue.id
                          AND membership.user_id = admission.active_uid
                          AND membership.role = 'member'
                    )
                ))
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

    sqlx::query("INSERT INTO venues (id, name, description, uid) VALUES (?, ?, ?, ?)")
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
    member_uid: &str,
) -> Result<Venue, String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin joined venue installation: {error}"))?;
    let active_uid: Option<String> = sqlx::query_scalar(
        "SELECT active_uid FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND active_uid = ?",
    )
    .bind(member_uid)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize joined venue installation: {error}"))?
    .flatten();
    if active_uid.as_deref() != Some(member_uid) {
        return Err("Authenticated principal changed while joining venue".into());
    }
    let existing_owner: Option<Option<String>> =
        sqlx::query_scalar("SELECT uid FROM venues WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to inspect joined venue: {error}"))?;
    if existing_owner.flatten().as_deref() == Some(member_uid) {
        return Err("You already own this venue".into());
    }
    crate::database::local::write_admission::enter_remote_writes(&mut transaction).await?;
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
    .execute(&mut *transaction)
    .await
    .map_err(|e| format!("Failed to insert joined venue: {}", e))?;
    sqlx::query(
        "INSERT INTO venue_memberships (venue_id, user_id, role)
         VALUES (?, ?, 'member')
         ON CONFLICT(venue_id, user_id) DO NOTHING",
    )
    .bind(id)
    .bind(member_uid)
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to install venue membership: {error}"))?;
    let query = format!("SELECT {} FROM venues WHERE id = ?", VENUE_COLUMNS);
    let venue = sqlx::query_as::<_, Venue>(sqlx::AssertSqlSafe(query))
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to read joined venue: {error}"))?;
    crate::database::local::write_admission::leave_remote_writes(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit joined venue installation: {error}"))?;
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

    let authored_history: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM authored_documents WHERE venue_id = ?")
            .bind(&id)
            .fetch_one(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to inspect venue authored history: {error}"))?;
    if authored_history != 0 {
        return Err(
            "Authored venues must be retained so their score history remains restorable".into(),
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
             JOIN venue_memberships membership
               ON membership.venue_id = venue.id AND membership.user_id = ?
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
    let deleted = sqlx::query("DELETE FROM venue_memberships WHERE venue_id = ? AND user_id = ?")
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
             (id, owner_user_id, agent_kind, subject_kind, subject_id, venue_id, score_id)
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
    async fn venue_deletion_refuses_authored_history() {
        let (_directory, pool) = test_pool().await;
        insert_owned_venue(&pool).await;
        sqlx::query(
            "INSERT INTO authored_documents
             (document_id, document_kind, principal_key, subject_id,
              track_id, venue_id, score_id)
             VALUES (?, 'track_score', 'signed-in:alice', 'track',
                     'track', 'venue', 'score')",
        )
        .bind(format!("ad-{}", "a".repeat(64)))
        .execute(&pool)
        .await
        .unwrap();

        let error = delete_owned_venue(&pool).await.unwrap_err();
        assert!(error.contains("score history remains restorable"));
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
