//! ArtNet node discovery.
//!
//! All three commands need the manager, and `ArtNetManager::new` needs an
//! `AppHandle` — so on a host that has none they fail rather than pretend.
//! Returning an empty node list or a silent `Ok(())` instead would make a
//! host that cannot do ArtNet at all indistinguishable from a network with no
//! nodes on it.

use crate::artnet::{ArtNetManager, ArtNetNode};
use crate::dispatch::{AppServices, CommandError};

fn manager(services: &AppServices) -> Result<&ArtNetManager, CommandError> {
    services
        .artnet
        .as_deref()
        .ok_or_else(|| CommandError::NotFound("ArtNet is unavailable on this host".into()))
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
