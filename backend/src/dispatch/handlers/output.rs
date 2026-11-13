//! Output devices and persisted routing: DMX universes to their ports, and
//! laser projectors to their DACs.
//! Listing devices requires the physical-output service installed by the
//! desktop host. Headless callers can still edit routing without opening a
//! socket or a device.

use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::lasers::{LaserDevice, LaserDeviceKind, LaserOutput, LaserRoute};
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

// -- Lasers --

fn hub(services: &AppServices) -> Result<&crate::output::OutputHub, CommandError> {
    services
        .output
        .as_deref()
        .ok_or_else(|| CommandError::NotFound("Laser output is not running on this host".into()))
}

/// Every laser DAC a driver can see now, bound or not.
pub async fn list_laser_devices(services: &AppServices) -> Result<Vec<LaserDevice>, CommandError> {
    Ok(hub(services)?.laser_devices())
}

pub async fn list_laser_routes(services: &AppServices) -> Result<Vec<LaserRoute>, CommandError> {
    Ok(crate::database::local::outputs::laser_routes(&services.db.0).await?)
}

/// Play a projector through a DAC, in place of whichever it was played
/// through. `address` is where to look for a DAC that does not announce
/// itself.
pub async fn bind_laser_route(
    services: &AppServices,
    fixture_id: String,
    device_kind: LaserDeviceKind,
    device_id: String,
    device_name: Option<String>,
    address: Option<String>,
) -> Result<(), CommandError> {
    crate::database::local::outputs::bind_laser(
        &services.db.0,
        &LaserRoute {
            fixture_id,
            device_kind,
            device_id,
            device_name,
            address,
        },
    )
    .await?;
    republish_lasers(services).await
}

pub async fn unbind_laser_route(
    services: &AppServices,
    fixture_id: String,
) -> Result<(), CommandError> {
    crate::database::local::outputs::unbind_laser(&services.db.0, &fixture_id).await?;
    republish_lasers(services).await
}

async fn republish_lasers(services: &AppServices) -> Result<(), CommandError> {
    let routes = crate::database::local::outputs::laser_routes(&services.db.0).await?;
    if let Some(output) = services.output.as_deref() {
        output.set_laser_routes(routes);
    }
    Ok(())
}

/// Whether laser output is on, and how each bound projector's DAC is doing.
pub async fn get_laser_output(services: &AppServices) -> Result<LaserOutput, CommandError> {
    Ok(hub(services)?.laser_output())
}

/// Turn laser output on or off. It is off at every launch.
pub async fn set_laser_output(
    services: &AppServices,
    on: bool,
) -> Result<LaserOutput, CommandError> {
    let hub = hub(services)?;
    hub.set_laser_output(on);
    Ok(hub.laser_output())
}

/// Show one projector's test pattern, the edge of the venue's window and
/// its middle, in place of what it would play; `None` shows none.
pub async fn set_laser_test(
    services: &AppServices,
    projector_id: Option<String>,
) -> Result<LaserOutput, CommandError> {
    let hub = hub(services)?;
    hub.set_laser_test(projector_id);
    Ok(hub.laser_output())
}
