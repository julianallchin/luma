use crate::database::local::group_overrides as overrides_db;
use crate::database::local::groups as groups_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::handlers::fixtures::require_changed;
use crate::dispatch::{AppServices, CommandError};
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{
    normalize_group_name, FixtureGroup, FixtureGroupNode, GroupTreeNode, MovementConfig,
};
use crate::models::selection::Selection;
use crate::models::universe::UniverseState;
use crate::services::groups as groups_service;
use crate::services::groups::invalidate_venue_fixture_cache;
use crate::stage_render;

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
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
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
        &Selection::new(query.trim()),
        seed.unwrap_or(DEFAULT_PREVIEW_SEED),
    )
    .await?;
    Ok(resolved.into_iter().map(|r| r.fixture).collect())
}

/// The frame that answers "which heads is this?": every head the selection
/// matches open and white, the rest of the rig dark.
///
/// A [`UniverseState`] rather than a fixture list because the answer is
/// head-accurate — [`preview_selection_query`] above collapses a match to whole
/// fixtures and so cannot picture a group that owns half a bar. The caller
/// installs it on a scene and renders; there is no second way to draw a
/// highlight.
///
/// The seed is fixed for the same reason the agent's `venue.render` fixes it:
/// a highlight is a picture of *one* answer, and a picker that redrew a
/// different half on every hover would be lying about what applying does.
pub async fn highlight_selection(
    services: &AppServices,
    venue_id: String,
    selection: Selection,
) -> Result<UniverseState, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let resolved = groups_service::resolve_selection_expression_with_path(
        &services.fixtures_root,
        &mut access,
        &selection,
        DEFAULT_PREVIEW_SEED,
    )
    .await?;
    Ok(stage_render::highlight_state(&resolved))
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

// -----------------------------------------------------------------------------
// The derived group tree, and the overrides on top of it
// -----------------------------------------------------------------------------

/// The venue's group tree: derivation, the manual edits on top, and the
/// authored groups beside them.
///
/// Flat with `parentId`, parents before children — build the tree in one pass.
pub async fn list_group_tree(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(groups_service::group_tree(&services.fixtures_root, &mut access).await?)
}

/// Rename one node of the tree.
///
/// The name stops being derived; the membership does not. Rename a wing's top
/// half and the movers you hang there tomorrow still land in it.
pub async fn rename_group_node(
    services: &AppServices,
    venue_id: String,
    group_id: String,
    label: String,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    override_node(services, &venue_id, &group_id, Some(&label), None, None).await
}

/// Move one node under another. `parent_id` of `None` moves it to the top level.
pub async fn move_group_node(
    services: &AppServices,
    venue_id: String,
    group_id: String,
    parent_id: Option<String>,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    // The empty string is "the top level" on the wire, because `None` already
    // means "leave the derived parent alone" in the row.
    let parent = parent_id.unwrap_or_default();
    override_node(services, &venue_id, &group_id, None, Some(&parent), None).await
}

/// Fold one node's fixtures into another. The source stops being shown and the
/// target counts its members alongside its own — by reference, so both sides go
/// on tracking the rig and [`reset_group_node`] undoes it.
pub async fn merge_group_nodes(
    services: &AppServices,
    venue_id: String,
    group_id: String,
    into_group_id: String,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    if group_id == into_group_id {
        return Err(CommandError::Invalid(
            "a group cannot be merged into itself".into(),
        ));
    }
    override_node(
        services,
        &venue_id,
        &group_id,
        None,
        None,
        Some(&into_group_id),
    )
    .await
}

/// Drop a node's override, restoring derivation for it.
pub async fn reset_group_node(
    services: &AppServices,
    venue_id: String,
    group_id: String,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    overrides_db::remove(&mut access, &group_id).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    list_group_tree(services, venue_id).await
}

/// Write one facet of an override, and hand back the whole tree — the caller
/// that changed one node is about to redraw all of them, and a second round
/// trip would be a second derivation of the same venue.
///
/// The node must be in the tree: an override naming nothing is a patch with
/// nothing to patch.
async fn override_node(
    services: &AppServices,
    venue_id: &str,
    group_id: &str,
    label: Option<&str>,
    parent_id: Option<&str>,
    merged_into: Option<&str>,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(venue_id)).await?;
    for id in [Some(group_id), merged_into].into_iter().flatten() {
        if groups_service::group_node(&services.fixtures_root, &mut access, id)
            .await?
            .is_none()
        {
            return Err(CommandError::NotFound(format!(
                "no group `{id}` in this venue"
            )));
        }
    }
    let path = groups_service::derived_path(&services.fixtures_root, &mut access, group_id)
        .await?
        .unwrap_or_else(|| group_id.to_string());
    overrides_db::put(&mut access, group_id, &path, label, parent_id, merged_into).await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    list_group_tree(services, venue_id.to_string()).await
}
