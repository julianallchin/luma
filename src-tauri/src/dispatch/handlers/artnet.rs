//! ArtNet node discovery.
//!
//! All three commands need the manager, and `ArtNetManager::new` needs an
//! `AppHandle` — so on a host that has none they fail rather than pretend.
//! Returning an empty node list or a silent `Ok(())` instead would make a
//! host that cannot do ArtNet at all indistinguishable from a network with no
//! nodes on it.

use crate::artnet::ArtNetManager;
use crate::dispatch::{AppServices, CommandError};
use crate::models::patch::{ArtNetNode, UniverseOutput};

fn manager(services: &AppServices) -> Result<&ArtNetManager, CommandError> {
    services
        .artnet
        .as_deref()
        .ok_or_else(|| CommandError::NotFound("Art-Net is not running on this host".into()))
}

pub async fn start_discovery(services: &AppServices) -> Result<(), CommandError> {
    manager(services)?.start_discovery();
    Ok(())
}

pub async fn stop_discovery(services: &AppServices) -> Result<(), CommandError> {
    manager(services)?.stop_discovery();
    Ok(())
}

pub async fn get_discovered_nodes(services: &AppServices) -> Result<Vec<ArtNetNode>, CommandError> {
    Ok(manager(services)?.discovered_nodes())
}

// -- Universe outputs --
//
// Binding is a database fact, so unlike discovery these three work on a host
// with no `ArtNetManager`: the table is what the patch page edits, and the
// manager is only the thing that reads it on a frame.

pub async fn list_outputs(services: &AppServices) -> Result<Vec<UniverseOutput>, CommandError> {
    Ok(crate::database::local::outputs::list(&services.db.0).await?)
}

/// Point one universe at a discovered node.
///
/// `port_address` is the node's own 15-bit Net/SubNet/Universe triple, taken
/// from its ArtPollReply — not derived from `universe`, which is the aliasing
/// bug this table replaces.
pub async fn bind_output(
    services: &AppServices,
    universe: i64,
    node_ip: String,
    node_port: i64,
    port_address: i64,
    node_name: Option<String>,
) -> Result<(), CommandError> {
    crate::database::local::outputs::bind(
        &services.db.0,
        universe,
        &node_ip,
        node_port,
        port_address,
        node_name.as_deref(),
    )
    .await?;
    republish(services).await
}

pub async fn unbind_output(services: &AppServices, universe: i64) -> Result<(), CommandError> {
    crate::database::local::outputs::unbind(&services.db.0, universe).await?;
    republish(services).await
}

/// Push the whole table into the manager, so the sender never reads the
/// database on a frame.
async fn republish(services: &AppServices) -> Result<(), CommandError> {
    let outputs = crate::database::local::outputs::list(&services.db.0).await?;
    if let Some(artnet) = services.artnet.as_deref() {
        artnet.set_outputs(outputs);
    }
    Ok(())
}
