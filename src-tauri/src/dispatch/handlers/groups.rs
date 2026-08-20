use crate::database::local::groups as groups_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::handlers::fixtures::require_changed;
use crate::dispatch::{AppServices, CommandError};
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{normalize_group_name, FixtureGroup, FixtureGroupNode, MovementConfig};
use crate::services::groups as groups_service;
use crate::services::groups::invalidate_venue_fixture_cache;

/// Seed used when a selection preview does not supply one. Previews must resolve
/// `random()` selectors the same way evaluation does, so this default is part of
/// the contract, not a convenience.
const DEFAULT_PREVIEW_SEED: u64 = 12345;

// -----------------------------------------------------------------------------
// Group CRUD
// -----------------------------------------------------------------------------

pub async fn create_group(
    services: &AppServices,
    venue_id: String,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    require_unique_name(&mut access, name.as_deref(), None).await?;
    let result =
        groups_db::create_group(&mut access, name.as_deref(), axis_lr, axis_fb, axis_ab).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(result)
}

pub async fn list_groups(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<FixtureGroup>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(groups_db::list_groups(&mut access).await?)
}

pub async fn update_group(
    services: &AppServices,
    id: String,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
) -> Result<FixtureGroup, CommandError> {
    let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Group(&id)).await?;
    require_unique_name(&mut access, name.as_deref(), Some(&id)).await?;
    let result =
        groups_db::update_group(&mut access, &id, name.as_deref(), axis_lr, axis_fb, axis_ab)
            .await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(result)
}

pub async fn delete_group(services: &AppServices, id: String) -> Result<(), CommandError> {
    let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Group(&id)).await?;
    require_changed(groups_db::delete_group(&mut access, &id).await?)?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(())
}

// -----------------------------------------------------------------------------
// Membership
// -----------------------------------------------------------------------------

/// Add a whole fixture (`head_index` = `None`) or a single head to a group.
pub async fn add_fixture_to_group(
    services: &AppServices,
    fixture_id: String,
    group_id: String,
    head_index: Option<i64>,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Group(&group_id)).await?;
    let head = head_index.unwrap_or(groups_db::WHOLE_FIXTURE);
    groups_db::add_member_to_group(&mut access, &fixture_id, &group_id, head).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(())
}

/// Remove a whole fixture (`head_index` = `None`, drops per-head rows too) or a
/// single head from a group. Removing a head from a whole-fixture membership
/// splits it into per-head rows for the remaining heads.
pub async fn remove_fixture_from_group(
    services: &AppServices,
    fixture_id: String,
    group_id: String,
    head_index: Option<i64>,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Group(&group_id)).await?;
    match head_index {
        None => {
            groups_db::remove_member_from_group(&mut access, &fixture_id, &group_id, None).await
        }
        Some(head) => {
            groups_service::remove_head_from_group(
                &services.fixtures_root,
                &mut access,
                &fixture_id,
                &group_id,
                head,
            )
            .await
        }
    }?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(())
}

// -----------------------------------------------------------------------------
// Hierarchy and selection
// -----------------------------------------------------------------------------

pub async fn get_grouped_hierarchy(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<FixtureGroupNode>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(
        groups_service::get_grouped_hierarchy_with_path(&services.fixtures_root, &mut access)
            .await?,
    )
}

pub async fn preview_selection_query(
    services: &AppServices,
    venue_id: String,
    query: String,
    seed: Option<u64>,
) -> Result<Vec<PatchedFixture>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let resolved = groups_service::resolve_selection_expression_with_path(
        &services.fixtures_root,
        &mut access,
        query.trim(),
        seed.unwrap_or(DEFAULT_PREVIEW_SEED),
    )
    .await?;
    Ok(resolved.into_iter().map(|r| r.fixture).collect())
}

/// Fixtures in the venue with no group membership row at all — what the group
/// migration left behind.
pub async fn get_ungrouped_fixtures(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<PatchedFixture>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(groups_db::get_ungrouped_fixtures(&mut access).await?)
}

// -----------------------------------------------------------------------------
// Movement config
// -----------------------------------------------------------------------------

/// `config: None` clears the movement config; it does not mean "leave
/// unchanged".
pub async fn update_movement_config(
    services: &AppServices,
    group_id: String,
    config: Option<MovementConfig>,
) -> Result<FixtureGroup, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Group(&group_id)).await?;
    let group = groups_db::update_movement_config(&mut access, &group_id, config.as_ref()).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(group)
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Group names are unique per venue under [`normalize_group_name`]. A name that
/// normalizes to empty is exempt, which is why this is a scan rather than a DB
/// constraint. `exclude` is the group being renamed, if any.
async fn require_unique_name(
    access: &mut VenueAccess<'_, Write>,
    name: Option<&str>,
    exclude: Option<&str>,
) -> Result<(), CommandError> {
    let Some(normalized) = name.map(normalize_group_name).filter(|n| !n.is_empty()) else {
        return Ok(());
    };
    for group in groups_db::list_groups(access).await? {
        if Some(group.id.as_str()) == exclude {
            continue;
        }
        if group
            .name
            .as_deref()
            .is_some_and(|existing| normalize_group_name(existing) == normalized)
        {
            return Err(CommandError::Conflict {
                expected: None,
                found: Some(normalized.clone()),
                message: format!("A group with name '{normalized}' already exists"),
            });
        }
    }
    Ok(())
}
