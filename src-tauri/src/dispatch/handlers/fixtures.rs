use std::path::{Component, Path};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::{AppServices, CommandError};
use crate::fixtures::layout::fixture_mount;
use crate::models::fixtures::{FixtureDefinition, FixtureEntry, FixtureFacing, PatchedFixture};
use crate::services::fixtures as fixture_service;
use crate::services::groups::invalidate_venue_fixture_cache;

/// Pushing the patch to ArtNet is best-effort: the manager needs an `AppHandle`
/// to construct, so a headless host has none and the patch simply goes
/// unpublished. The optionality lives in the type rather than in a lookup that
/// can silently find nothing.
pub async fn get_patched_fixtures(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<PatchedFixture>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let fixtures = fixture_service::get_patched_fixtures(&mut access).await?;
    publish_patch(services, fixtures.clone());
    Ok(fixtures)
}

/// Which way every **placed** fixture in a venue points.
///
/// A companion to [`get_patched_fixtures`] rather than extra columns on it: a
/// facing is the outward normal of the socket the fixture hangs from, so it is
/// what the resolver says and cannot be stored beside the patch without going
/// stale. Callers that need both fetch both — they are already fetching several
/// things about a venue in parallel.
///
/// A fixture that is patched but not placed is **absent from the result**
/// rather than carried with a fabricated origin pose: it is in the tray and has
/// no facing at all. Consumers key by id, so absence is the answer.
pub async fn get_fixture_facings(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<FixtureFacing>, CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = crate::venue_graph::resolved(&mut access, &services.fixtures_root).await?;
    Ok(fixture_service::get_patched_fixtures(&mut access)
        .await?
        .into_iter()
        .filter_map(|f| {
            let direction = fixture_mount(venue.pose(&f.id)?).normal();
            Some(FixtureFacing {
                id: f.id,
                direction: direction.to_array(),
                word: fixture_kinematics::StageDirection::of(direction)
                    .label()
                    .to_string(),
            })
        })
        .collect())
}

/// Build the in-memory index of the bundled fixture definitions. Returns how
/// many were indexed; [`search_fixtures`] fails until this has run.
pub async fn initialize_fixtures(services: &AppServices) -> Result<usize, CommandError> {
    Ok(fixture_service::initialize_fixtures(&services.fixtures_root, &services.fixtures).await?)
}

pub async fn search_fixtures(
    services: &AppServices,
    query: String,
    offset: usize,
    limit: usize,
) -> Result<Vec<FixtureEntry>, CommandError> {
    Ok(fixture_service::search_fixtures(
        query,
        offset,
        limit,
        &services.fixtures,
    )?)
}

pub async fn get_fixture_definition(
    services: &AppServices,
    path: String,
) -> Result<FixtureDefinition, CommandError> {
    let relative = confine_to_root(&path)?;
    Ok(fixture_service::get_fixture_definition(
        &services.fixtures_root,
        relative,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub async fn patch_fixture(
    services: &AppServices,
    venue_id: String,
    universe: i64,
    address: i64,
    num_channels: i64,
    manufacturer: String,
    model: String,
    mode_name: String,
    fixture_path: String,
    label: Option<String>,
) -> Result<PatchedFixture, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let fixture = fixtures_db::insert_fixture(
        &mut access,
        universe,
        address,
        num_channels,
        &manufacturer,
        &model,
        &mode_name,
        &fixture_path,
        label.as_deref(),
    )
    .await?;
    commit_and_publish(services, access).await?;
    Ok(fixture)
}

pub async fn move_patched_fixture(
    services: &AppServices,
    venue_id: String,
    id: String,
    address: i64,
) -> Result<(), CommandError> {
    let mut access = fixture_write(services, &venue_id, &id).await?;
    require_changed(fixtures_db::update_fixture_address(&mut access, &id, address).await?)?;
    commit_and_publish(services, access).await
}

pub async fn remove_patched_fixture(
    services: &AppServices,
    venue_id: String,
    id: String,
) -> Result<(), CommandError> {
    let mut access = fixture_write(services, &venue_id, &id).await?;
    require_changed(fixtures_db::delete_fixture(&mut access, &id).await?)?;
    commit_and_publish(services, access).await
}

pub async fn rename_patched_fixture(
    services: &AppServices,
    venue_id: String,
    id: String,
    label: String,
) -> Result<(), CommandError> {
    let mut access = fixture_write(services, &venue_id, &id).await?;
    require_changed(fixtures_db::update_fixture_label(&mut access, &id, &label).await?)?;
    commit_and_publish(services, access).await
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Open a write scope on one fixture. `venue_id` is a redundant cross-check —
/// the authorized resource is the fixture itself.
async fn fixture_write<'a>(
    services: &'a AppServices,
    venue_id: &str,
    id: &str,
) -> Result<VenueAccess<'a, Write>, CommandError> {
    let access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Fixture(id)).await?;
    access.require_venue(venue_id)?;
    Ok(access)
}

/// Commit a patch-mutating write and republish the resulting patch.
///
/// Every fixture mutation ends this way, so the read-back, the commit, the
/// ArtNet push and the cache invalidation stay in one place and cannot drift
/// apart per command.
async fn commit_and_publish(
    services: &AppServices,
    mut access: VenueAccess<'_, Write>,
) -> Result<(), CommandError> {
    let patch = fixtures_db::get_patched_fixtures(&mut access).await?;
    access.commit().await?;
    publish_patch(services, patch);
    invalidate_venue_fixture_cache();
    Ok(())
}

fn publish_patch(services: &AppServices, patch: Vec<PatchedFixture>) {
    if let Some(artnet) = services.artnet.as_ref() {
        artnet.update_patch(patch);
    }
}

/// A write that touched no row means the resource was not in scope.
pub(super) fn require_changed(rows_affected: u64) -> Result<(), CommandError> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(CommandError::NotFound("Venue resource not found".into()))
    }
}

/// Reject a fixture path that would escape the fixtures root.
///
/// The path comes from the frontend, which only ever echoes back a `path` the
/// index handed it — but it is joined onto a root directory, so the seam is
/// where it has to be constrained rather than trusted.
fn confine_to_root(path: &str) -> Result<&Path, CommandError> {
    let relative = Path::new(path);
    let escapes = relative.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    if escapes {
        return Err(CommandError::Invalid(format!(
            "fixture path escapes the fixtures root: {path}"
        )));
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confines_fixture_paths_to_the_root() {
        assert!(confine_to_root("chauvet/rogue-r2.qxf").is_ok());
        assert!(confine_to_root("../../etc/passwd").is_err());
        assert!(confine_to_root("a/../../b.qxf").is_err());
        assert!(confine_to_root("/etc/passwd").is_err());
    }
}
