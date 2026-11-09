use uuid::Uuid;

use sqlx::{FromRow, Row};

use crate::database::local::venue_access::{
    self, AuthorizedVenue, Owner, Standing, VenueAccess, Write,
};
use crate::models::venues::{Venue, VenueMember, ROLE_MEMBER, ROLE_OWNER};
use luma_scene::environment::{VenueEnvironment, VenueHaze};

/// The stored columns of a venue aliased `venue`. Its `role` and `capability`
/// are not among them: they are the reader's, set by [`read_by`].
macro_rules! venue_columns {
    () => {
        "venue.id, venue.uid, venue.name, venue.description, venue.share_code,
         venue.environment, venue.haze, venue.created_at, venue.updated_at"
    };
}

/// `venue` as the principal `standing` describes reads it.
fn read_by(mut venue: Venue, standing: Standing) -> Venue {
    let role = if standing.owner {
        ROLE_OWNER
    } else {
        ROLE_MEMBER
    };
    venue.role = role.into();
    venue.capability = standing.capability;
    venue
}

/// Fetch a single venue by ID
pub async fn get_venue(access: &mut impl AuthorizedVenue) -> Result<Venue, String> {
    let venue_id = access.venue_id().to_string();
    let row = sqlx::query_as::<_, Venue>(concat!(
        "SELECT ",
        venue_columns!(),
        " FROM venues venue WHERE venue.id = ?"
    ))
    .bind(venue_id)
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to fetch venue: {}", e))?;

    Ok(read_by(row, access.standing()))
}

/// List exactly the venues admitted for the app database's current host-bound
/// principal — the ones [`venue_access::standing`] gives them a standing in.
/// Signed-out guest state never includes cached account venues.
pub async fn list_venues(pool: &sqlx::SqlitePool) -> Result<Vec<Venue>, String> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        venue_columns!(),
        ", admission.active_uid AS principal,
                EXISTS(
                    SELECT 1 FROM venue_members membership
                    WHERE membership.venue_id = venue.id
                      AND membership.uid = admission.active_uid
                ) AS is_member
         FROM venues venue
         CROSS JOIN auth_write_admission admission
         WHERE admission.singleton = 1
           AND admission.armed = 1
           AND admission.accepting = 1
           AND admission.maintenance = 0
         ORDER BY venue.updated_at DESC"
    ))
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list venues: {}", e))?;

    let mut venues = Vec::new();
    for row in rows {
        let venue = Venue::from_row(&row).map_err(|e| format!("Failed to read a venue: {e}"))?;
        let principal: Option<String> = row.get("principal");
        let is_member: bool = row.get("is_member");
        if let Some(standing) =
            venue_access::standing(venue.uid.as_deref(), principal.as_deref(), is_member)
        {
            venues.push(read_by(venue, standing));
        }
    }
    Ok(venues)
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
    // The root is part of the venue, so it is written with it: a venue with
    // no root is one whose rows are still arriving from sync.
    sqlx::query("INSERT INTO venue_nodes (id, uid, venue_id, kind) VALUES (?, ?, ?, 'venue')")
        .bind(crate::venue_graph::root_id(&id))
        .bind(&principal)
        .bind(&id)
        .execute(&mut *transaction)
        .await
        .map_err(|e| format!("Failed to create the venue's root: {e}"))?;
    let venue = sqlx::query_as::<_, Venue>(concat!(
        "SELECT ",
        venue_columns!(),
        " FROM venues venue WHERE venue.id = ?"
    ))
    .bind(&id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to read created venue: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit venue creation: {error}"))?;
    let creator = venue_access::standing(venue.uid.as_deref(), principal.as_deref(), false)
        .ok_or_else(|| "The created venue is not its creator's".to_string())?;
    Ok(read_by(venue, creator))
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

/// Set this venue's haze, synced with the venue.
///
/// One write for the whole value, like [`set_environment`]: enabled, density
/// and appearance are one look, and a per-field setter would let a caller write
/// a density onto a room whose haze is off and wonder why nothing changed.
///
/// The march's cost knobs are not here and never will be — see
/// `luma_scene::environment::VenueHaze`.
///
/// The ordinary venue dirtiness trigger schedules delivery of this edit.
pub async fn set_haze(access: &mut VenueAccess<'_, Write>, haze: VenueHaze) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query("UPDATE venues SET haze = ? WHERE id = ?")
        .bind(haze.sanitized().to_record())
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to set venue haze: {e}"))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Sharing
// -----------------------------------------------------------------------------

/// Set or clear the venue's share code, synced with the venue.
///
/// Owner-only by the guard's type, as on the server, where a trigger on
/// `venues` refuses a member's change to the code. Errors with [`SHARE_CODE_TAKEN`] when another
/// venue on this device already holds `code`.
pub async fn set_share_code(
    access: &mut VenueAccess<'_, Owner>,
    code: Option<&str>,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_string();
    sqlx::query("UPDATE venues SET share_code = ? WHERE id = ?")
        .bind(code)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|error| match error.as_database_error() {
            Some(database) if database.is_unique_violation() => SHARE_CODE_TAKEN.to_string(),
            _ => format!("Failed to set the venue's share code: {error}"),
        })?;
    Ok(())
}

/// What [`set_share_code`] answers when the code is another venue's.
pub const SHARE_CODE_TAKEN: &str = "share code already in use";

/// The venue's owner and every account that joined it, owner first, then in
/// the order they joined.
pub async fn venue_members(access: &mut impl AuthorizedVenue) -> Result<Vec<VenueMember>, String> {
    let venue_id = access.venue_id().to_string();
    let you = access.principal().map(str::to_owned);
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT uid, 'owner', created_at FROM venues WHERE id = ?1 AND uid IS NOT NULL
         UNION ALL
         SELECT * FROM (
             SELECT uid, 'member', created_at FROM venue_members
             WHERE venue_id = ?1
               AND uid IS NOT (SELECT uid FROM venues WHERE id = ?1)
             ORDER BY created_at, uid
         )",
    )
    .bind(venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to read the venue's members: {error}"))?;
    Ok(rows
        .into_iter()
        .map(|(uid, role, since)| VenueMember {
            is_you: you.as_deref() == Some(uid.as_str()),
            uid,
            role,
            since,
        })
        .collect())
}

/// Take `uid`'s access to the venue away. The upload carries the delete to
/// the server, whose delete policy on `venue_members` lets the owner remove
/// anyone, and the venue's rows leave that account's devices on its next
/// checkpoint. Whether a row went is the answer.
pub async fn remove_venue_member(
    access: &mut VenueAccess<'_, Owner>,
    uid: &str,
) -> Result<bool, String> {
    let venue_id = access.venue_id().to_string();
    let removed = sqlx::query("DELETE FROM venue_members WHERE venue_id = ? AND uid = ?")
        .bind(venue_id)
        .bind(uid)
        .execute(&mut *access.connection())
        .await
        .map_err(|error| format!("Failed to remove the member: {error}"))?;
    Ok(removed.rows_affected() > 0)
}
