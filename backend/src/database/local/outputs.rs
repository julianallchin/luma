//! `universe_outputs`: which node each DMX universe goes to.
//!
//! Rows in, rows out. App-global rather than venue-scoped — which box is
//! plugged in where is a property of the room, not of the venue document — so
//! this module takes a pool rather than a [`crate::database::local::venue_access::VenueAccess`].

use sqlx::SqlitePool;

use crate::models::patch::UniverseOutput;

/// Every binding, by universe.
///
/// # Errors
/// Fails if the table cannot be read.
pub async fn list(pool: &SqlitePool) -> Result<Vec<UniverseOutput>, String> {
    sqlx::query_as::<_, UniverseOutput>(
        "SELECT universe, node_ip, node_port, port_address, node_name
         FROM universe_outputs ORDER BY universe ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to read universe outputs: {e}"))
}

/// Point `universe` at a node, replacing whatever it pointed at before.
///
/// An upsert rather than an insert plus a delete: a universe has exactly one
/// output, which is what the primary key says, and rebinding is one write.
///
/// # Errors
/// Fails if the row cannot be written — including a `port_address` outside
/// Art-Net's 15 bits, which the table's own check refuses.
pub async fn bind(
    pool: &SqlitePool,
    universe: i64,
    node_ip: &str,
    node_port: i64,
    port_address: i64,
    node_name: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO universe_outputs (universe, node_ip, node_port, port_address, node_name)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(universe) DO UPDATE SET
             node_ip = excluded.node_ip,
             node_port = excluded.node_port,
             port_address = excluded.port_address,
             node_name = excluded.node_name,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(universe)
    .bind(node_ip)
    .bind(node_port)
    .bind(port_address)
    .bind(node_name)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to bind universe {universe}: {e}"))?;
    Ok(())
}

/// Forget `universe`'s binding. Unbinding something that was never bound is
/// not an error: the caller wanted no binding, and there is none.
///
/// # Errors
/// Fails if the row cannot be deleted.
pub async fn unbind(pool: &SqlitePool, universe: i64) -> Result<(), String> {
    sqlx::query("DELETE FROM universe_outputs WHERE universe = ?")
        .bind(universe)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to unbind universe {universe}: {e}"))?;
    Ok(())
}
