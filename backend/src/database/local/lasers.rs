//! What a venue stores about its lasers (`docs/design/lasers.md`): each
//! projector's settings and the venue's window, as parameters of the
//! projector's node and of the root, and each projector's limit
//! (`laser_limits`).

use std::collections::BTreeMap;
use uuid::Uuid;

use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::database::local::venue_graph;
use crate::laser::{Key, Settings, Window};
use crate::models::lasers::LaserLimit;

/// Set the window the venue's laser clips draw in; `None` goes back to the
/// one fitted to its projectors.
///
/// # Errors
/// Refuses what is not a window, and a venue whose root has not arrived.
pub async fn set_window(
    access: &mut VenueAccess<'_, Write>,
    window: Option<Window>,
) -> Result<(), String> {
    if let Some(window) = &window {
        window.check()?;
    }
    let root = venue_graph::root_id(access)
        .await?
        .ok_or("This venue is still syncing")?;
    let values = window.map(|window| window.params());
    let params = Window::PARAMS
        .into_iter()
        .enumerate()
        .map(|(i, key)| (key.to_owned(), values.map(|values| values[i].1)))
        .collect();
    venue_graph::set_params(access, &root, &params).await
}

/// Change some laser settings, by the parameter each is stored under: one
/// projector's, or with no projector the ones the venue's projectors share.
/// A value is kept inside its setting's range.
///
/// # Errors
/// Refuses a parameter that is no laser setting, one that is each box's own
/// when no projector is named, and a projector that is not in this venue.
pub async fn set_settings(
    access: &mut VenueAccess<'_, Write>,
    projector: Option<&str>,
    changes: &BTreeMap<String, f64>,
) -> Result<(), String> {
    let mut settings = Settings::default();
    let mut params = BTreeMap::new();
    for (param, value) in changes {
        let key = Key::of(param).ok_or_else(|| format!("{param} is not a laser setting"))?;
        if projector.is_none() && key.is_own() {
            return Err(format!("{param} is set on each projector"));
        }
        settings.set(key, *value);
        params.insert(param.clone(), Some(settings.get(key)));
    }
    let node = node_of(access, projector).await?;
    venue_graph::set_params(access, &node, &params).await
}

/// Give a projector a kind of its own, starting from `kind`, or with `None`
/// have it follow the venue's again.
///
/// # Errors
/// Refuses a projector that is not in this venue.
pub async fn set_own(
    access: &mut VenueAccess<'_, Write>,
    projector: &str,
    kind: Option<&Settings>,
) -> Result<(), String> {
    let params = Key::ALL
        .into_iter()
        .filter(|key| !key.is_own())
        .map(|key| (key.param().to_owned(), kind.map(|kind| kind.get(key))))
        .collect();
    let node = node_of(access, Some(projector)).await?;
    venue_graph::set_params(access, &node, &params).await
}

/// The node laser settings are stored on: a projector's, or the venue's
/// root for what its projectors share.
async fn node_of(
    access: &mut VenueAccess<'_, Write>,
    projector: Option<&str>,
) -> Result<String, String> {
    let Some(projector) = projector else {
        return venue_graph::root_id(access)
            .await?
            .ok_or_else(|| "This venue is still syncing".to_owned());
    };
    if venue_graph::nodes_in_venue(access, &[projector.to_owned()])
        .await?
        .is_empty()
    {
        return Err("That projector is not in this venue".into());
    }
    Ok(projector.to_owned())
}

/// Set where a projector may send light, a polygon in galvo coordinates, or
/// with `None` let it send light anywhere in its scan.
///
/// # Errors
/// Refuses what is not a polygon inside the scan, and a projector that is
/// not in this venue.
pub async fn set_limit(
    access: &mut VenueAccess<'_, Write>,
    projector: &str,
    points: Option<&[[f64; 2]]>,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_owned();
    let Some(points) = points else {
        sqlx::query("DELETE FROM laser_limits WHERE fixture_id = ? AND venue_id = ?")
            .bind(projector)
            .bind(&venue_id)
            .execute(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to clear the laser limit: {error}"))?;
        return Ok(());
    };
    LaserLimit::check(points)?;
    let points = serde_json::to_string(points).map_err(|error| error.to_string())?;
    let uid = access.principal().to_owned();
    let done = sqlx::query(
        "INSERT INTO laser_limits (id, uid, venue_id, fixture_id, points)
         SELECT ?, ?, venue_id, id, ? FROM fixtures WHERE id = ? AND venue_id = ?
         ON CONFLICT(fixture_id) DO UPDATE SET points = excluded.points",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(uid)
    .bind(points)
    .bind(projector)
    .bind(&venue_id)
    .execute(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to save the laser limit: {error}"))?;
    if done.rows_affected() == 0 {
        return Err("That projector is not in this venue".into());
    }
    Ok(())
}

/// Every projector limit in the venue.
pub async fn list_limits(access: &mut impl AuthorizedVenue) -> Result<Vec<LaserLimit>, String> {
    let venue_id = access.venue_id().to_owned();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT fixture_id, points FROM laser_limits WHERE venue_id = ? ORDER BY fixture_id",
    )
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
