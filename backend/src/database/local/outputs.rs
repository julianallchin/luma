//! `dmx_routes`: which device and port each DMX universe goes to.
//!
//! Rows in, rows out. App-global rather than venue-scoped — which box is
//! plugged in where is a property of the room, not of the venue document — so
//! this module takes a pool rather than a [`crate::database::local::venue_access::VenueAccess`].

use sqlx::SqlitePool;

use crate::models::patch::{DeviceKind, DmxRoute};

/// Every route, by universe.
///
/// # Errors
/// Fails if the table cannot be read.
pub async fn list(pool: &SqlitePool) -> Result<Vec<DmxRoute>, String> {
    sqlx::query_as::<_, DmxRoute>(
        "SELECT universe, device_kind, device_id, port, device_name, address
         FROM dmx_routes ORDER BY universe ASC, device_kind ASC, device_id ASC, port ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to read DMX routes: {e}"))
}

/// Send `route.universe` to `route`'s device and port, alongside whatever it
/// already goes to. Binding the same port twice refreshes its name and address.
///
/// # Errors
/// Fails if the row cannot be written — including a port outside 0..=32767,
/// which the table's own check refuses.
pub async fn bind(pool: &SqlitePool, route: &DmxRoute) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO dmx_routes (universe, device_kind, device_id, port, device_name, address)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(universe, device_kind, device_id, port) DO UPDATE SET
             device_name = excluded.device_name,
             address = excluded.address,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(route.universe)
    .bind(route.device_kind.as_str())
    .bind(&route.device_id)
    .bind(route.port)
    .bind(&route.device_name)
    .bind(&route.address)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to bind universe {}: {e}", route.universe))?;
    Ok(())
}

/// Stop sending `universe` to one device port. Unbinding a route that was
/// never there is not an error: the caller wanted no route, and there is none.
///
/// # Errors
/// Fails if the row cannot be deleted.
pub async fn unbind(
    pool: &SqlitePool,
    universe: i64,
    device_kind: DeviceKind,
    device_id: &str,
    port: i64,
) -> Result<(), String> {
    sqlx::query(
        "DELETE FROM dmx_routes
         WHERE universe = ? AND device_kind = ? AND device_id = ? AND port = ?",
    )
    .bind(universe)
    .bind(device_kind.as_str())
    .bind(device_id)
    .bind(port)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to unbind universe {universe}: {e}"))?;
    Ok(())
}
