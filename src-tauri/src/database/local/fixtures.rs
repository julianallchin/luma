use uuid::Uuid;

use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::fixtures::PatchedFixture;

// -----------------------------------------------------------------------------
// Inserts / Updates / Deletes
// -----------------------------------------------------------------------------

pub async fn insert_fixture(
    access: &mut VenueAccess<'_, Write>,
    universe: i64,
    address: i64,
    num_channels: i64,
    manufacturer: &str,
    model: &str,
    mode_name: &str,
    fixture_path: &str,
    label: Option<&str>,
) -> Result<PatchedFixture, String> {
    let id = Uuid::new_v4().to_string();
    let venue_id = access.venue_id().to_owned();
    let uid = access.principal().map(str::to_owned);

    sqlx::query(
        "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&uid)
    .bind(&venue_id)
    .bind(universe)
    .bind(address)
    .bind(num_channels)
    .bind(manufacturer)
    .bind(model)
    .bind(mode_name)
    .bind(fixture_path)
    .bind(label)
    .bind(0.0_f64)
    .bind(0.0_f64)
    .bind(0.0_f64)
    .bind(0.0_f64)
    .bind(0.0_f64)
    .bind(0.0_f64)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to insert fixture: {}", e))?;

    Ok(PatchedFixture {
        id,
        uid,
        venue_id,
        universe,
        address,
        num_channels,
        manufacturer: manufacturer.to_string(),
        model: model.to_string(),
        mode_name: mode_name.to_string(),
        fixture_path: fixture_path.to_string(),
        label: label.map(|s| s.to_string()),
        pos_x: 0.0,
        pos_y: 0.0,
        pos_z: 0.0,
        rot_x: 0.0,
        rot_y: 0.0,
        rot_z: 0.0,
    })
}

pub async fn update_fixture_address(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    address: i64,
) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let result = sqlx::query("UPDATE fixtures SET address = ? WHERE id = ? AND venue_id = ?")
        .bind(address)
        .bind(id)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to move patched fixture: {}", e))?;
    Ok(result.rows_affected())
}

pub async fn update_fixture_spatial(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<u64, String> {
    let result = sqlx::query(
        "UPDATE fixtures SET pos_x = ?, pos_y = ?, pos_z = ?, rot_x = ?, rot_y = ?, rot_z = ? WHERE id = ? AND venue_id = ?",
    )
    .bind(pos_x)
    .bind(pos_y)
    .bind(pos_z)
    .bind(rot_x)
    .bind(rot_y)
    .bind(rot_z)
    .bind(id)
    .bind(access.venue_id().to_owned())
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to update fixture spatial data: {}", e))?;
    Ok(result.rows_affected())
}

pub async fn update_fixture_label(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    label: &str,
) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let result = sqlx::query("UPDATE fixtures SET label = ? WHERE id = ? AND venue_id = ?")
        .bind(label)
        .bind(id)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to rename patched fixture: {}", e))?;
    Ok(result.rows_affected())
}

pub async fn delete_fixture(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let result = sqlx::query("DELETE FROM fixtures WHERE id = ? AND venue_id = ?")
        .bind(id)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to remove patched fixture: {}", e))?;
    Ok(result.rows_affected())
}

// -----------------------------------------------------------------------------
// Queries
// -----------------------------------------------------------------------------

pub async fn get_patched_fixtures(
    access: &mut impl AuthorizedVenue,
) -> Result<Vec<PatchedFixture>, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z
         FROM fixtures WHERE venue_id = ?",
    )
    .bind(access.venue_id().to_owned())
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to get patched fixtures: {}", e))
}

/// Fetch a single fixture by ID
pub async fn get_fixture(
    access: &mut impl AuthorizedVenue,
    id: &str,
) -> Result<PatchedFixture, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z
         FROM fixtures WHERE id = ? AND venue_id = ?",
    )
    .bind(id)
    .bind(access.venue_id().to_owned())
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to get fixture: {}", e))
}
