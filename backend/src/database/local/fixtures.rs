use uuid::Uuid;

use crate::database::local::sync_delete;
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
    address_pinned: bool,
) -> Result<PatchedFixture, String> {
    let id = Uuid::new_v4().to_string();
    let venue_id = access.venue_id().to_owned();
    let uid = access.principal().map(str::to_owned);

    sqlx::query(
        "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, address_pinned, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(i64::from(address_pinned))
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
        address_pinned,
        pos_x: 0.0,
        pos_y: 0.0,
        pos_z: 0.0,
        rot_x: 0.0,
        rot_y: 0.0,
        rot_z: 0.0,
    })
}

/// Move one fixture to `universe`/`address`, recording whether the number came
/// from a human.
///
/// The **only** writer of an address after the insert. `pinned` is part of the
/// same statement because the two are one decision: a hand-set address that did
/// not mark itself would be re-derived by the next auto-patch, and a pin
/// without an address would mean nothing.
///
/// # Errors
/// Fails if the write is refused — including by the table's own footprint
/// check, which is what makes a truncating reader unreachable.
pub async fn update_fixture_address(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    universe: i64,
    address: i64,
    pinned: bool,
) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let result = sqlx::query(
        "UPDATE fixtures SET universe = ?, address = ?, address_pinned = ?
         WHERE id = ? AND venue_id = ?",
    )
    .bind(universe)
    .bind(address)
    .bind(i64::from(pinned))
    .bind(id)
    .bind(venue_id)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to address fixture: {e}"))?;
    Ok(result.rows_affected())
}

/// Repatch one fixture into a different mode: the mode's name and the width it
/// costs, written together.
///
/// One statement rather than a mode write beside an address write, because a
/// row whose `mode_name` and `num_channels` disagree is a footprint nobody can
/// compute — and the two are decided by the same lookup in the definition.
///
/// # Errors
/// Fails if the write is refused.
pub async fn update_fixture_mode(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    mode_name: &str,
    num_channels: i64,
    universe: i64,
    address: i64,
) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let result = sqlx::query(
        "UPDATE fixtures SET mode_name = ?, num_channels = ?, universe = ?, address = ?
         WHERE id = ? AND venue_id = ?",
    )
    .bind(mode_name)
    .bind(num_channels)
    .bind(universe)
    .bind(address)
    .bind(id)
    .bind(venue_id)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to set fixture mode: {e}"))?;
    Ok(result.rows_affected())
}

/// Pin or unpin one address, without moving it.
///
/// Separate from [`update_fixture_address`] because pinning an address a
/// fixture is already at is not an address edit: there is nothing to admit, so
/// there is nothing that can be refused.
///
/// # Errors
/// Fails if the write is refused.
pub async fn update_fixture_pin(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    pinned: bool,
) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let result =
        sqlx::query("UPDATE fixtures SET address_pinned = ? WHERE id = ? AND venue_id = ?")
            .bind(i64::from(pinned))
            .bind(id)
            .bind(venue_id)
            .execute(&mut *access.connection())
            .await
            .map_err(|e| format!("Failed to pin fixture address: {e}"))?;
    Ok(result.rows_affected())
}

/// Forget every hand-set address in the venue, so the next allocation derives
/// them all. What the Auto Patch button means.
///
/// # Errors
/// Fails if the write is refused.
pub async fn clear_address_pins(access: &mut VenueAccess<'_, Write>) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    let result = sqlx::query(
        "UPDATE fixtures SET address_pinned = 0 WHERE venue_id = ? AND address_pinned <> 0",
    )
    .bind(venue_id)
    .execute(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to clear address pins: {e}"))?;
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
    let deleted = sync_delete::delete_synced_where(
        access.connection(),
        "fixtures",
        "id = ? AND venue_id = ?",
        &[id, &venue_id],
    )
    .await
    .map_err(|e| format!("Failed to remove patched fixture: {}", e))?;
    Ok(deleted as u64)
}

// -----------------------------------------------------------------------------
// Queries
// -----------------------------------------------------------------------------

pub async fn get_patched_fixtures(
    access: &mut impl AuthorizedVenue,
) -> Result<Vec<PatchedFixture>, String> {
    sqlx::query_as::<_, PatchedFixture>(
        "SELECT id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, address_pinned, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z
         FROM fixtures WHERE venue_id = ? ORDER BY id ASC",
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
        "SELECT id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, address_pinned, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z
         FROM fixtures WHERE id = ? AND venue_id = ?",
    )
    .bind(id)
    .bind(access.venue_id().to_owned())
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to get fixture: {}", e))
}
