//! DMX output devices and persisted universe routing.
//! Listing devices requires the physical-output service installed by the
//! desktop host. Headless callers can still edit routing without opening a
//! socket or a device.

use crate::dispatch::{AppServices, CommandError};
use crate::models::patch::{DeviceKind, DmxRoute, OutputDevice};

/// Every device any transport can see now: Art-Net nodes that answered a
/// poll, the Art-Net broadcast, and supported USB dongles. Empty while output
/// is off, since no driver looks for anything then.
pub async fn list_output_devices(
    services: &AppServices,
) -> Result<Vec<OutputDevice>, CommandError> {
    let output = services
        .output
        .as_deref()
        .ok_or_else(|| CommandError::NotFound("DMX output is not running on this host".into()))?;
    Ok(output.devices())
}

// -- Routes --
//
// Routing is a database fact, so unlike the device list these work on a host
// with no `OutputHub`: the table is what the patch page edits, and the hub is
// only the thing that reads it on a frame.

pub async fn list_routes(services: &AppServices) -> Result<Vec<DmxRoute>, CommandError> {
    Ok(crate::database::local::outputs::list(&services.db.0).await?)
}

/// Send one universe to one device port, alongside wherever it already goes.
///
/// `port` is the device's own: an Art-Net node's announced 15-bit port
/// address, or a dongle's output index — never derived from `universe`.
pub async fn bind_route(
    services: &AppServices,
    universe: i64,
    device_kind: DeviceKind,
    device_id: String,
    port: i64,
    device_name: Option<String>,
    address: Option<String>,
) -> Result<(), CommandError> {
    crate::database::local::outputs::bind(
        &services.db.0,
        &DmxRoute {
            universe,
            device_kind,
            device_id,
            port,
            device_name,
            address,
        },
    )
    .await?;
    republish(services).await
}

pub async fn unbind_route(
    services: &AppServices,
    universe: i64,
    device_kind: DeviceKind,
    device_id: String,
    port: i64,
) -> Result<(), CommandError> {
    crate::database::local::outputs::unbind(
        &services.db.0,
        universe,
        device_kind,
        &device_id,
        port,
    )
    .await?;
    republish(services).await
}

/// Push the whole table into the hub, so no driver reads the database on a
/// frame.
async fn republish(services: &AppServices) -> Result<(), CommandError> {
    let routes = crate::database::local::outputs::list(&services.db.0).await?;
    if let Some(output) = services.output.as_deref() {
        output.set_routes(routes);
    }
    Ok(())
}
