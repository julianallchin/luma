//! A venue's laser zones and its projectors' limits (`laser_zones`,
//! `laser_limits`; `docs/design/lasers.md`).

use uuid::Uuid;

use crate::database::local::deletes;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::lasers::{LaserLimit, LaserZone};

/// The venue's zones, by name.
pub async fn list_zones(access: &mut impl AuthorizedVenue) -> Result<Vec<LaserZone>, String> {
    let venue_id = access.venue_id().to_owned();
    let rows: Vec<(String, String, f64, f64, f64, f64)> = sqlx::query_as(
        "SELECT id, name, turn_min, turn_max, tilt_min, tilt_max FROM laser_zones
         WHERE venue_id = ? ORDER BY name COLLATE NOCASE, created_at, id",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to list laser zones: {error}"))?;
    Ok(rows
        .into_iter()
        .map(
            |(id, name, turn_min, turn_max, tilt_min, tilt_max)| LaserZone {
                id,
                venue_id: venue_id.clone(),
                name,
                turn: [turn_min, turn_max],
                tilt: [tilt_min, tilt_max],
            },
        )
        .collect())
}

/// Create a zone (`id` `None`) or replace one of this venue's. Answers the
/// zone as stored.
pub async fn save_zone(
    access: &mut VenueAccess<'_, Write>,
    id: Option<&str>,
    name: &str,
    turn: [f64; 2],
    tilt: [f64; 2],
) -> Result<Option<LaserZone>, String> {
    let zone = LaserZone {
        id: id.map_or_else(|| Uuid::new_v4().to_string(), str::to_owned),
        venue_id: access.venue_id().to_owned(),
        name: name.to_owned(),
        turn,
        tilt,
    };
    let uid = access.principal().map(str::to_owned);
    let query = if id.is_some() {
        "UPDATE laser_zones SET name = ?, turn_min = ?, turn_max = ?, tilt_min = ?, tilt_max = ?
         WHERE id = ? AND venue_id = ?"
    } else {
        "INSERT INTO laser_zones (name, turn_min, turn_max, tilt_min, tilt_max, id, venue_id, uid)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    };
    let mut statement = sqlx::query(query)
        .bind(&zone.name)
        .bind(zone.turn[0])
        .bind(zone.turn[1])
        .bind(zone.tilt[0])
        .bind(zone.tilt[1])
        .bind(&zone.id)
        .bind(&zone.venue_id);
    if id.is_none() {
        statement = statement.bind(uid);
    }
    let done = statement
        .execute(&mut *access.connection())
        .await
        .map_err(|error| format!("Failed to save laser zone: {error}"))?;
    Ok((done.rows_affected() == 1).then_some(zone))
}

/// Answers how many zones it deleted: one, or none when `id` is not this
/// venue's. Laser clips naming it draw in the venue's first zone.
pub async fn delete_zone(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    deletes::delete_where(
        access.connection(),
        "laser_zones",
        "id = ? AND venue_id = ?",
        &[id, &venue_id],
    )
    .await
    .map(|deleted| deleted as u64)
}

/// Every projector limit in the venue.
pub async fn list_limits(access: &mut impl AuthorizedVenue) -> Result<Vec<LaserLimit>, String> {
    let venue_id = access.venue_id().to_owned();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, points FROM laser_limits WHERE venue_id = ? ORDER BY id")
            .bind(&venue_id)
            .fetch_all(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to list laser limits: {error}"))?;
    Ok(rows
        .into_iter()
        .filter_map(|(fixture_id, points)| match serde_json::from_str(&points) {
            Ok(points) => Some(LaserLimit { fixture_id, points }),
            Err(error) => {
                log::warn!("laser limit for {fixture_id} is not a polygon: {error}");
                None
            }
        })
        .collect())
}

/// Set a projector's limit, or clear it with `None`. Answers whether the
/// fixture is this venue's.
pub async fn set_limit(
    access: &mut VenueAccess<'_, Write>,
    fixture_id: &str,
    points: Option<&[[f64; 2]]>,
) -> Result<bool, String> {
    let venue_id = access.venue_id().to_owned();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM fixtures WHERE id = ? AND venue_id = ?)")
            .bind(fixture_id)
            .bind(&venue_id)
            .fetch_one(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to read the fixture: {error}"))?;
    if !exists {
        return Ok(false);
    }
    let Some(points) = points else {
        deletes::delete_where(
            access.connection(),
            "laser_limits",
            "id = ? AND venue_id = ?",
            &[fixture_id, &venue_id],
        )
        .await?;
        return Ok(true);
    };
    let json = serde_json::to_string(points).map_err(|error| error.to_string())?;
    let uid = access.principal().map(str::to_owned);
    sqlx::query(
        "INSERT INTO laser_limits (id, uid, venue_id, points) VALUES (?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET points = excluded.points",
    )
    .bind(fixture_id)
    .bind(uid)
    .bind(&venue_id)
    .bind(json)
    .execute(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to save laser limit: {error}"))?;
    Ok(true)
}
