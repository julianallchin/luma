use crate::database::local::group_overrides as overrides_db;
use crate::database::local::group_overrides::GroupOverride;
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
use crate::services::group_derivation;
use crate::services::groups as groups_service;
use crate::services::groups::{invalidate_venue_fixture_cache, GroupSources};
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
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    require_unique_name(services, &mut access, name.as_deref(), None).await?;
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
    // The venue first, because the graph conversion needs a write of its own
    // and the namespace this rename joins is derived from that graph.
    let venue_id = VenueAccess::<Read>::read(&services.db.0, VenueResource::Group(&id))
        .await?
        .venue_id()
        .to_string();
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Group(&id)).await?;
    require_unique_name(services, &mut access, name.as_deref(), Some(&id)).await?;
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

/// The venue's groups with their fixtures: the merged tree
/// ([`list_group_tree`]) with every node's members resolved to fixtures and
/// heads. Flat with `parentId`, parents before children.
pub async fn get_grouped_hierarchy(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<FixtureGroupNode>, CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(GroupSources::read(&services.fixtures_root, &mut access)
        .await?
        .hierarchy())
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

/// Group names are unique per venue under [`normalize_group_name`], across the
/// **whole** namespace: the derived tree and the authored groups share it, so
/// an authored `spots_right_wing` is refused rather than left for a selection
/// expression to union with the wing of that name. A name that normalizes to
/// empty is exempt, which is why this is a scan rather than a DB constraint.
/// `exclude` is the group being renamed, if any.
async fn require_unique_name(
    services: &AppServices,
    access: &mut VenueAccess<'_, Write>,
    name: Option<&str>,
    exclude: Option<&str>,
) -> Result<(), CommandError> {
    let Some(normalized) = name.map(normalize_group_name).filter(|n| !n.is_empty()) else {
        return Ok(());
    };
    let tree = GroupSources::read(&services.fixtures_root, access)
        .await?
        .tree();
    match groups_service::node_answering_to(&tree, &normalized, exclude.unwrap_or_default()) {
        Some(clash) => Err(name_taken(&normalized, clash)),
        None => Ok(()),
    }
}

/// The refusal, in the words a human can act on: the name, and what already
/// answers to it.
fn name_taken(name: &str, clash: &GroupTreeNode) -> CommandError {
    CommandError::Invalid(format!(
        "`{name}` is already what `{}` is called in this venue",
        clash.label
    ))
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
    Ok(GroupSources::read(&services.fixtures_root, &mut access)
        .await?
        .tree())
}

/// Rename one node of the tree. `label` of `None` drops the rename and lets the
/// node derive its name again, keeping whatever move or merge it also carries.
///
/// The name stops being derived; the membership does not. Rename a wing's top
/// half and the movers you hang there tomorrow still land in it.
pub async fn rename_group_node(
    services: &AppServices,
    venue_id: String,
    group_id: String,
    label: Option<String>,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    override_node(
        services,
        &venue_id,
        &group_id,
        Edit {
            label: Some(label),
            ..Edit::default()
        },
    )
    .await
}

/// Move one node under another.
///
/// `parent_id` of `None` drops the move and restores the derived parent; the
/// empty string is the top level, because a node someone dragged out of its
/// branch and a node that never moved are different states and both need a
/// spelling. Moving a node under one of its own descendants is refused.
pub async fn move_group_node(
    services: &AppServices,
    venue_id: String,
    group_id: String,
    parent_id: Option<String>,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    override_node(
        services,
        &venue_id,
        &group_id,
        Edit {
            parent_id: Some(parent_id),
            ..Edit::default()
        },
    )
    .await
}

/// Fold one node's fixtures into another. The source stops being shown and the
/// target counts its members alongside its own — by reference, so both sides go
/// on tracking the rig and [`reset_group_node`] undoes it.
///
/// `into_group_id` of `None` un-merges. A merge that would close a cycle, or
/// fold a node into something already inside it, is refused: neither has a
/// terminal set for the fixtures to land in.
pub async fn merge_group_nodes(
    services: &AppServices,
    venue_id: String,
    group_id: String,
    into_group_id: Option<String>,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    override_node(
        services,
        &venue_id,
        &group_id,
        Edit {
            merged_into: Some(into_group_id),
            ..Edit::default()
        },
    )
    .await
}

/// Drop a node's override, restoring derivation for it.
pub async fn reset_group_node(
    services: &AppServices,
    venue_id: String,
    group_id: String,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    override_node(
        services,
        &venue_id,
        &group_id,
        Edit {
            label: Some(None),
            parent_id: Some(None),
            merged_into: Some(None),
        },
    )
    .await
}

/// One edit to a node's identity, one facet per verb.
///
/// `None` leaves a facet alone, `Some(None)` clears it, `Some(Some(x))` sets
/// it. Three-valued on purpose: the row's three columns are independent, and a
/// patch that could only add facets could never clear a label without also
/// undoing the move beside it.
#[derive(Default)]
struct Edit {
    label: Option<Option<String>>,
    parent_id: Option<Option<String>>,
    merged_into: Option<Option<String>>,
}

impl Edit {
    /// The row this edit leaves behind, or `None` when it leaves nothing to
    /// say — a node with no rename, no move and no merge is a derived node, and
    /// the absence of a row is how that is spelled.
    fn onto(
        self,
        group_id: &str,
        path: String,
        old: Option<&GroupOverride>,
    ) -> Option<GroupOverride> {
        let facet = |edit: Option<Option<String>>, was: Option<&String>| match edit {
            Some(new) => new,
            None => was.cloned(),
        };
        let row = GroupOverride {
            group_id: group_id.to_string(),
            path,
            label: facet(self.label, old.and_then(|row| row.label.as_ref())),
            parent_id: facet(self.parent_id, old.and_then(|row| row.parent_id.as_ref())),
            merged_into: facet(
                self.merged_into,
                old.and_then(|row| row.merged_into.as_ref()),
            ),
        };
        (row.label.is_some() || row.parent_id.is_some() || row.merged_into.is_some()).then_some(row)
    }
}

/// Write one edit, and hand back the whole tree — the caller that changed one
/// node is about to redraw all of them, and a second round trip would be a
/// second derivation of the same venue.
///
/// One derivation per command: the guards, the path the row records and the
/// tree that comes back are all read off the same solve.
///
/// The node must be in the tree: an override naming nothing is a patch with
/// nothing to patch.
async fn override_node(
    services: &AppServices,
    venue_id: &str,
    group_id: &str,
    edit: Edit,
) -> Result<Vec<GroupTreeNode>, CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(venue_id)).await?;
    let mut sources = GroupSources::read(&services.fixtures_root, &mut access).await?;

    for id in [
        Some(group_id),
        edit.parent_id.as_ref().and_then(Option::as_deref),
        edit.merged_into.as_ref().and_then(Option::as_deref),
    ]
    .into_iter()
    .flatten()
    .filter(|id| !id.is_empty())
    {
        if !sources.contains(id) {
            return Err(CommandError::NotFound(format!(
                "no group `{id}` in this venue"
            )));
        }
    }

    let before = sources.tree();
    if let Some(Some(parent)) = edit.parent_id.as_ref() {
        if !parent.is_empty() && is_inside(&before, parent, group_id) {
            return Err(CommandError::Invalid(
                "a group cannot be moved under itself or under one of its own children".into(),
            ));
        }
    }
    if let Some(Some(target)) = edit.merged_into.as_ref() {
        if is_inside(&before, target, group_id) {
            return Err(CommandError::Invalid(
                "a group cannot be merged into itself or into one of its own children".into(),
            ));
        }
    }

    let row = edit.onto(
        group_id,
        sources.derived_path(group_id),
        sources.override_of(group_id),
    );
    // A merge whose chain has no end folds nothing, so it is refused rather
    // than written and silently ignored.
    if row.as_ref().is_some_and(|row| row.merged_into.is_some()) {
        let mut prospective: Vec<GroupOverride> = sources
            .overrides()
            .iter()
            .filter(|existing| existing.group_id != group_id)
            .cloned()
            .collect();
        prospective.push(row.clone().expect("checked just above"));
        if group_derivation::merged_terminal(&prospective, group_id).is_none() {
            return Err(CommandError::Invalid(
                "that merge would close a loop, and a loop has nowhere for the fixtures to go"
                    .into(),
            ));
        }
    }

    // The tree this edit *would* leave behind, derived before anything is
    // written — it is both the uniqueness check's subject and the answer handed
    // back, so the caller cannot be told one thing while the venue holds
    // another.
    //
    // The check is `clash_for` and not a scan of `after`: a typed name is
    // refused, and `after` has already had every name it could clash with
    // separated from it.
    sources.apply(row.clone(), group_id);
    if let Some(clash) = sources.clash_for(group_id) {
        return Err(name_taken(&clash.name, &clash));
    }
    let after = sources.tree();

    match &row {
        Some(row) => overrides_db::put(&mut access, row).await?,
        None => {
            overrides_db::remove(&mut access, group_id).await?;
        }
    }
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(after)
}

/// Whether `node` is `ancestor` or hangs under it, in the tree as it stands.
///
/// Bounded rather than trusting the tree to be acyclic: an override can name
/// any parent, so the shape this walks is only as sound as the rows that made
/// it — and refusing the edit that would close a loop is exactly what this is
/// for.
fn is_inside(tree: &[GroupTreeNode], node: &str, ancestor: &str) -> bool {
    let mut at = node;
    for _ in 0..tree.len().saturating_add(1) {
        if at == ancestor {
            return true;
        }
        let Some(parent) = tree
            .iter()
            .find(|candidate| candidate.id == at)
            .and_then(|candidate| candidate.parent_id.as_deref())
        else {
            return false;
        };
        at = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices, CommandError};

    /// A deck with a `bottom` that sits on the floor and a `top` a fixture or a
    /// tower can clamp to. Real geometry: a stub would pin half the answer.
    const DECK: &str = "stage_lab/stage_praticavel_1x1.glb";
    /// A shipped mover, so the tree's roles come out of the role table rather
    /// than out of the test.
    const MOVER: &str = "resources/fixtures/2511260420/Chauvet/Chauvet-Rogue-R2-Spot.qxf";

    // -----------------------------------------------------------------------
    // The tree, out of the seam a command comes through
    // -----------------------------------------------------------------------

    /// Four towers on the stage, one mover up each: two rows a wing, named for
    /// the height that separates them, and a `top` on each side to make the
    /// role's cross-cut. Derived end to end through `dispatch` rather than from
    /// hand-built facts.
    #[tokio::test]
    async fn the_tree_derives_through_dispatch() {
        let (_dir, services, venue) = rig().await;
        assert_eq!(
            labels(&tree(&services, &venue).await),
            [
                "spots",
                "left wing",
                "top",
                "bottom",
                "right wing",
                "top",
                "bottom",
                "top",
                "bottom",
            ]
        );
    }

    #[tokio::test]
    async fn a_rename_sticks_and_a_reset_undoes_it() {
        let (_dir, services, venue) = rig().await;
        let node = find(&tree(&services, &venue).await, "left wing");

        let renamed = rename(&services, &venue, &node, Some("house left"))
            .await
            .unwrap();
        let edited = at(&renamed, &node);
        assert_eq!(edited["label"], json!("house left"));
        assert_eq!(edited["origin"], json!("edited"));
        assert_eq!(edited["name"], json!("spots_house_left"));

        let reset = dispatch(
            &services,
            "reset_group_node",
            &json!({ "venueId": venue, "groupId": node }),
        )
        .await
        .unwrap();
        assert_eq!(at(&reset, &node)["label"], json!("left wing"));
        assert_eq!(at(&reset, &node)["origin"], json!("derived"));
    }

    /// The defect the `COALESCE` upsert made unreachable: clearing a label had
    /// to take the move with it, because a `NULL` meant "leave it alone".
    #[tokio::test]
    async fn a_label_can_be_cleared_without_undoing_the_move() {
        let (_dir, services, venue) = rig().await;
        let tree = tree(&services, &venue).await;
        let node = find(&tree, "left wing");
        let elsewhere = find(&tree, "right wing");

        rename(&services, &venue, &node, Some("house left"))
            .await
            .unwrap();
        let moved = move_node(&services, &venue, &node, Some(&elsewhere))
            .await
            .unwrap();
        assert_eq!(at(&moved, &node)["parentId"], json!(elsewhere));

        let cleared = rename(&services, &venue, &node, None).await.unwrap();
        assert_eq!(at(&cleared, &node)["label"], json!("left wing"));
        assert_eq!(
            at(&cleared, &node)["parentId"],
            json!(elsewhere),
            "clearing the name took the move with it"
        );
        assert_eq!(at(&cleared, &node)["origin"], json!("edited"));
    }

    #[tokio::test]
    async fn a_move_under_a_descendant_is_refused() {
        let (_dir, services, venue) = rig().await;
        let tree = tree(&services, &venue).await;
        let wing = find(&tree, "left wing");
        let child = child_of(&tree, &wing);

        assert_invalid(
            &move_node(&services, &venue, &wing, Some(&child))
                .await
                .expect_err("a wing was hung under its own row"),
            "under one of its own children",
        );
        assert_invalid(
            &move_node(&services, &venue, &wing, Some(&wing))
                .await
                .expect_err("a wing was hung under itself"),
            "under itself",
        );
    }

    #[tokio::test]
    async fn a_merge_moves_the_fixtures_and_a_reset_gives_them_back() {
        let (_dir, services, venue) = rig().await;
        let tree = tree(&services, &venue).await;
        let wing = find(&tree, "left wing");
        let other = find(&tree, "right wing");
        let before = at(&tree, &other)["fixtures"].as_array().unwrap().len();

        let merged = merge(&services, &venue, &wing, Some(&other)).await.unwrap();
        assert!(
            !merged
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["id"] == json!(wing)),
            "the merged node is still shown, so its fixtures are counted twice"
        );
        assert_eq!(
            at(&merged, &other)["fixtures"].as_array().unwrap().len(),
            before * 2
        );

        let unmerged = merge(&services, &venue, &wing, None).await.unwrap();
        assert_eq!(
            at(&unmerged, &other)["fixtures"].as_array().unwrap().len(),
            before
        );
    }

    #[tokio::test]
    async fn a_merge_that_would_close_a_loop_is_refused() {
        let (_dir, services, venue) = rig().await;
        let tree = tree(&services, &venue).await;
        let wing = find(&tree, "left wing");
        let other = find(&tree, "right wing");

        merge(&services, &venue, &wing, Some(&other)).await.unwrap();
        assert_invalid(
            &merge(&services, &venue, &other, Some(&wing))
                .await
                .expect_err("the two wings ate each other"),
            "close a loop",
        );
        assert_invalid(
            &merge(&services, &venue, &other, Some(&other))
                .await
                .expect_err("a wing ate itself"),
            "into itself",
        );
    }

    #[tokio::test]
    async fn merging_a_node_into_its_own_child_is_refused() {
        let (_dir, services, venue) = rig().await;
        let tree = tree(&services, &venue).await;
        let wing = find(&tree, "left wing");
        let child = child_of(&tree, &wing);
        assert_invalid(
            &merge(&services, &venue, &wing, Some(&child))
                .await
                .expect_err("a wing was folded into its own row"),
            "into one of its own children",
        );
    }

    #[tokio::test]
    async fn an_edit_naming_no_group_is_refused() {
        let (_dir, services, venue) = rig().await;
        let error = rename(&services, &venue, "no-such-group", Some("x"))
            .await
            .expect_err("an override named nothing");
        assert!(
            matches!(error, CommandError::NotFound(_)),
            "expected a not-found, got {}",
            error.kind()
        );
    }

    /// A node that carries no rename, no move and no merge is a derived node,
    /// and the absence of a row is how that is spelled — so the last facet
    /// cleared takes the row with it.
    #[tokio::test]
    async fn clearing_the_last_facet_drops_the_override() {
        let (_dir, services, venue) = rig().await;
        let node = find(&tree(&services, &venue).await, "left wing");
        rename(&services, &venue, &node, Some("house left"))
            .await
            .unwrap();
        let cleared = rename(&services, &venue, &node, None).await.unwrap();
        assert_eq!(at(&cleared, &node)["origin"], json!("derived"));
    }

    /// Defect: the derived tree folds into the selection cache, and no stage
    /// verb invalidated it — so moving a truss left every expression naming a
    /// derived group answering with the old split.
    #[tokio::test]
    async fn moving_a_structure_is_visible_to_a_selection_expression() {
        let (_dir, services, venue) = rig().await;
        assert_eq!(selection(&services, &venue, "spots_left_wing").await, 2);
        assert_eq!(selection(&services, &venue, "spots_right_wing").await, 2);

        // Swing one left tower across the stage: its class changes, so the two
        // sets those names stand for do too.
        let towers = towers(&services, &venue).await;
        let left = tower_at(&services, &venue, &towers, 6.0).await;
        dispatch(
            &services,
            "set_params",
            &json!({
                "venueId": venue,
                "nodeId": left,
                "params": { "u": -8.0 },
                "label": null,
            }),
        )
        .await
        .expect("the tower did not move");

        assert_eq!(
            selection(&services, &venue, "spots_left_wing").await,
            1,
            "the cache still answers with the old split"
        );
        assert_eq!(selection(&services, &venue, "spots_right_wing").await, 3);
    }

    /// Defect: the derived tree and the authored groups share one selection
    /// namespace, and only the authored half was checked — so an authored
    /// `spots_right_wing` could be minted beside the wing of that name, and an
    /// expression naming it silently unioned the two.
    #[tokio::test]
    async fn an_authored_group_cannot_take_a_derived_name() {
        let (_dir, services, venue) = rig().await;
        let error = create(&services, &venue, "Spots Right Wing")
            .await
            .expect_err("an authored group took a wing's name");
        assert_invalid(&error, "spots_right_wing");
    }

    /// And the other way round: a rename is a name too, and it lands in the
    /// same namespace the authored groups live in.
    #[tokio::test]
    async fn a_derived_node_cannot_be_renamed_onto_an_authored_group() {
        let (_dir, services, venue) = rig().await;
        create(&services, &venue, "spots house left")
            .await
            .expect("an authored group nothing derives");
        let node = find(&tree(&services, &venue).await, "left wing");

        let error = rename(&services, &venue, &node, Some("house left"))
            .await
            .expect_err("a wing took the authored group's name");
        assert_invalid(&error, "spots_house_left");
        assert_eq!(
            at(&tree(&services, &venue).await, &node)["label"],
            json!("left wing"),
            "the refused rename was written anyway"
        );
    }

    /// Defect: a derived name was minted from the path and never checked
    /// against the rest of the tree, so two pieces labelled `Truss 1` and
    /// `Truss-1` both answered to `spots_horizontal_truss_1` — and an
    /// expression naming it selected six movers where the tree showed three.
    #[tokio::test]
    async fn two_pieces_labelled_alike_do_not_answer_to_one_name() {
        let (_dir, services, venue) = alike_rig("Truss 1", "Truss-1").await;
        assert_eq!(
            names(&tree(&services, &venue).await),
            [
                "spots",
                "spots_horizontal",
                "spots_horizontal_truss_1",
                "spots_horizontal_truss_1_2",
            ]
        );
        assert_eq!(
            selection(&services, &venue, "spots_horizontal_truss_1").await,
            3,
            "one name still stands for both runs"
        );
        assert_eq!(
            selection(&services, &venue, "spots_horizontal_truss_1_2").await,
            3
        );
    }

    /// Defect: the conversion off the old schema commits like any other write,
    /// and only the write path told the cache. A reader that got in beside it
    /// saw a venue with no graph at all, cached the empty tree that follows,
    /// and the commit left the answer there.
    #[tokio::test]
    async fn a_read_between_the_migration_and_its_commit_leaves_no_empty_tree() {
        use crate::database::local::venue_access::{VenueAccess, VenueResource, Write};

        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        let venue = unconverted_venue(&services).await;

        let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue))
            .await
            .expect("the venue would not open for writing");
        assert!(
            crate::venue_graph::migrate(&mut access, &services.fixtures_root)
                .await
                .expect("the venue would not convert")
        );

        // The racing reader, and a *reading* one: every group verb converts
        // before it reads, so a caller that arrives beside someone else's
        // conversion is one that does not. It cannot see the conversion, and it
        // refills the cache with the tree the commit is about to replace.
        let racing = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            unconverting_selection(&services, &venue, "spots_horizontal"),
        )
        .await
        .expect("the reader could not get in beside the open conversion");
        assert_eq!(racing, 0, "the reader saw a conversion that has not landed");

        crate::venue_graph::commit_graph(access)
            .await
            .expect("the conversion did not commit");
        assert_eq!(
            selection(&services, &venue, "spots_horizontal").await,
            6,
            "the cache still answers from before the conversion"
        );
    }

    /// Defect: the cache was dropped *inside* the write transaction, so a read
    /// arriving between the last write and the commit refilled it from the rows
    /// the commit was about to replace — and the stale answer outlived the verb
    /// that caused it.
    #[tokio::test]
    async fn a_read_between_the_write_and_the_commit_leaves_no_stale_answer() {
        use crate::database::local::venue_access::{VenueAccess, VenueResource, Write};
        use crate::database::local::venue_graph as venue_graph_db;

        let (_dir, services, venue) = rig().await;
        assert_eq!(selection(&services, &venue, "spots_left_wing").await, 2);

        let towers = towers(&services, &venue).await;
        let left = tower_at(&services, &venue, &towers, 6.0).await;
        let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue))
            .await
            .expect("the venue would not open for writing");
        venue_graph_db::set_params(
            &mut access,
            &left,
            &[("u".to_string(), Some(-8.0))].into_iter().collect(),
        )
        .await
        .expect("the tower did not move");

        // The racing reader. It cannot see the move, and it refills the cache
        // with the answer that is about to be wrong.
        let racing = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            selection(&services, &venue, "spots_left_wing"),
        )
        .await
        .expect("the reader could not get in beside the open write");
        assert_eq!(racing, 2);

        crate::venue_graph::commit_graph(access)
            .await
            .expect("the move did not commit");
        assert_eq!(
            selection(&services, &venue, "spots_left_wing").await,
            1,
            "the cache still answers from before the commit"
        );
    }

    // -----------------------------------------------------------------------
    // Plumbing
    // -----------------------------------------------------------------------

    async fn create(
        services: &AppServices,
        venue: &str,
        name: &str,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "create_group",
            &json!({
                "venueId": venue,
                "name": name,
                "axisLr": null,
                "axisFb": null,
                "axisAb": null,
            }),
        )
        .await
    }

    fn assert_invalid(error: &CommandError, needle: &str) {
        let CommandError::Invalid(message) = error else {
            panic!("expected a refusal, got {} ({error})", error.kind());
        };
        assert!(
            message.contains(needle),
            "`{message}` does not mention `{needle}`"
        );
    }

    async fn tree(services: &AppServices, venue: &str) -> Value {
        dispatch(services, "list_group_tree", &json!({ "venueId": venue }))
            .await
            .expect("the tree did not derive")
    }

    fn labels(tree: &Value) -> Vec<String> {
        tree.as_array()
            .unwrap()
            .iter()
            .map(|node| node["label"].as_str().unwrap().to_string())
            .collect()
    }

    fn names(tree: &Value) -> Vec<String> {
        tree.as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// The id of the first node with this label.
    fn find(tree: &Value, label: &str) -> String {
        tree.as_array()
            .unwrap()
            .iter()
            .find(|node| node["label"] == json!(label))
            .unwrap_or_else(|| panic!("no `{label}` in {tree}"))["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn child_of(tree: &Value, parent: &str) -> String {
        tree.as_array()
            .unwrap()
            .iter()
            .find(|node| node["parentId"] == json!(parent))
            .expect("the node has a child")["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn at<'a>(tree: &'a Value, id: &str) -> &'a Value {
        tree.as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == json!(id))
            .unwrap_or_else(|| panic!("`{id}` left the tree"))
    }

    async fn rename(
        services: &AppServices,
        venue: &str,
        group: &str,
        label: Option<&str>,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "rename_group_node",
            &json!({ "venueId": venue, "groupId": group, "label": label }),
        )
        .await
    }

    async fn move_node(
        services: &AppServices,
        venue: &str,
        group: &str,
        parent: Option<&str>,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "move_group_node",
            &json!({ "venueId": venue, "groupId": group, "parentId": parent }),
        )
        .await
    }

    async fn merge(
        services: &AppServices,
        venue: &str,
        group: &str,
        into: Option<&str>,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "merge_group_nodes",
            &json!({ "venueId": venue, "groupId": group, "intoGroupId": into }),
        )
        .await
    }

    /// How many fixtures a selection expression resolves to.
    async fn selection(services: &AppServices, venue: &str, query: &str) -> usize {
        dispatch(
            services,
            "preview_selection_query",
            &json!({ "venueId": venue, "query": query, "seed": null }),
        )
        .await
        .expect("the expression did not resolve")
        .as_array()
        .unwrap()
        .len()
    }

    /// The same, for a caller that does not convert first — which is every
    /// reader outside a group verb, and the only kind that can arrive while a
    /// conversion is still open.
    async fn unconverting_selection(services: &AppServices, venue: &str, query: &str) -> usize {
        use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};

        let mut access = VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(venue))
            .await
            .expect("the venue would not open for reading");
        crate::services::groups::resolve_selection_expression_with_path(
            &services.fixtures_root,
            &mut access,
            &crate::models::selection::Selection::new(query),
            0,
        )
        .await
        .expect("the expression did not resolve")
        .len()
    }

    /// The two tower nodes, in the order they were placed.
    async fn towers(services: &AppServices, venue: &str) -> Vec<String> {
        let rows = dispatch(services, "get_venue_graph", &json!({ "venueId": venue }))
            .await
            .unwrap();
        let mut ids: Vec<String> = rows["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["kind"] == json!("piece"))
            .map(|node| node["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    /// Which of `towers` was placed at `u`.
    async fn tower_at(services: &AppServices, venue: &str, towers: &[String], u: f64) -> String {
        let rows = dispatch(services, "get_venue_graph", &json!({ "venueId": venue }))
            .await
            .unwrap();
        towers
            .iter()
            .find(|id| {
                rows["params"][id.as_str()]["u"]
                    .as_f64()
                    .is_some_and(|value| (value - u).abs() < 1e-9)
            })
            .expect("a tower stands there")
            .clone()
    }

    /// A venue with `n` movers patched into it and nothing placed yet.
    ///
    /// The fixtures are patched *before* the graph is first read, because the
    /// conversion is what gives a patched fixture its graph node.
    async fn venue_with_movers(services: &AppServices, n: i64) -> (String, Vec<String>) {
        let venue = dispatch(
            services,
            "create_venue",
            &json!({ "name": "Golden room", "description": null }),
        )
        .await
        .expect("the venue was not created")["id"]
            .as_str()
            .unwrap()
            .to_string();

        let mut fixtures = Vec::new();
        for n in 0..n {
            let patched = dispatch(
                services,
                "patch_fixture",
                &json!({
                    "venueId": venue,
                    "universe": 0,
                    "address": 1 + n * 20,
                    "numChannels": 18,
                    "manufacturer": "Chauvet",
                    "model": "Rogue R2 Spot",
                    "modeName": "18 Channel",
                    "fixturePath": MOVER,
                    "label": null,
                }),
            )
            .await
            .expect("the fixture was not patched");
            fixtures.push(patched["id"].as_str().unwrap().to_string());
        }
        (venue, fixtures)
    }

    /// Clamp `fixture` to `piece`'s top, `u` along it and `trim` above it.
    async fn clamp(
        services: &AppServices,
        venue: &str,
        fixture: &str,
        piece: &str,
        u: f64,
        trim: f64,
    ) {
        dispatch(
            services,
            "reattach",
            &json!({
                "venueId": venue,
                "nodeId": fixture,
                "parentId": piece,
                "mySocket": "clamp",
                "theirSocket": "top",
                "yaw": null,
            }),
        )
        .await
        .expect("the mover would not clamp to the piece");
        dispatch(
            services,
            "set_params",
            &json!({
                "venueId": venue,
                "nodeId": fixture,
                "params": { "u": u, "v": 0.0, "trim": trim },
                "label": null,
            }),
        )
        .await
        .expect("the mover would not take a trim");
    }

    /// A stage with four towers on it, one mover up each — the wing rig, built
    /// through `dispatch` from end to end.
    async fn rig() -> (tempfile::TempDir, AppServices, String) {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        let (venue, fixtures) = venue_with_movers(&services, 4).await;

        let stage = place(
            &services,
            &venue,
            "stage",
            DECK,
            None,
            Some("Stage"),
            0.0,
            0.0,
        )
        .await;
        // Two towers a side, unlabelled, so the rows are named for the axis
        // that separates them — the movers' 3 m of trim, which beats the 2 m
        // between the towers. A labelled piece names its own row, which is a
        // different test.
        //
        // `u` on a deck's `top` socket runs stage *left*-positive, the opposite
        // of the venue floor's, so these read backwards: `u = 6` stands at
        // `x = -6`. Nothing in derivation depends on it — the side reads a
        // resolved x — but a test that got it wrong would look like a sign bug
        // in the rule.
        let mut towers = Vec::new();
        for u in [6.0, 4.0, -4.0, -6.0] {
            towers.push(
                place(
                    &services,
                    &venue,
                    "piece",
                    DECK,
                    Some((&stage, "top")),
                    None,
                    u,
                    0.0,
                )
                .await,
            );
        }

        for (n, fixture) in fixtures.iter().enumerate() {
            let trim = 2.0 + (n % 2) as f64 * 3.0;
            clamp(&services, &venue, fixture, &towers[n], 0.0, trim).await;
        }

        (directory, services, venue)
    }

    /// Two pieces on the floor whose labels normalize to one name, three movers
    /// evenly spaced along each: one `horizontal` class, two labelled rows, and
    /// the same word for both unless something separates them.
    async fn alike_rig(first: &str, second: &str) -> (tempfile::TempDir, AppServices, String) {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        let (venue, fixtures) = venue_with_movers(&services, 6).await;

        for (n, label) in [first, second].into_iter().enumerate() {
            let truss = place(
                &services,
                &venue,
                "piece",
                DECK,
                None,
                Some(label),
                0.0,
                n as f64 * 4.0,
            )
            .await;
            for (step, fixture) in fixtures[n * 3..n * 3 + 3].iter().enumerate() {
                clamp(
                    &services,
                    &venue,
                    fixture,
                    &truss,
                    step as f64 * 2.0 - 2.0,
                    2.0,
                )
                .await;
            }
        }

        (directory, services, venue)
    }

    /// Six movers in a row on the *old* schema: positions in `fixtures.pos_*`
    /// and no graph at all, which is the venue `ensure_migrated` converts.
    ///
    /// Built by patching through `dispatch` — which converts — and then taking
    /// the graph back out, because the old write path it would otherwise need
    /// is gone.
    async fn unconverted_venue(services: &AppServices) -> String {
        let (venue, fixtures) = venue_with_movers(services, 6).await;
        let pool = &services.db.0;

        for (n, fixture) in fixtures.iter().enumerate() {
            // Evenly spaced, so the run is one distribution and the class it
            // makes is the whole answer: `spots_horizontal`, six movers.
            sqlx::query("UPDATE fixtures SET pos_x = ?, pos_y = 0.0, pos_z = 3.0 WHERE id = ?")
                .bind(n as f64 * 2.0 - 5.0)
                .bind(fixture)
                .execute(pool)
                .await
                .expect("the fixture would not take a position");
        }
        let mut connection = pool.acquire().await.expect("a connection");
        crate::database::local::sync_delete::delete_synced_where(
            &mut connection,
            "venue_nodes",
            "venue_id = ?",
            &[&venue],
        )
        .await
        .expect("the graph would not come back out");
        drop(connection);
        crate::services::groups::invalidate_venue_fixture_cache();

        venue
    }

    #[allow(clippy::too_many_arguments)]
    async fn place(
        services: &AppServices,
        venue: &str,
        kind: &str,
        catalog_ref: &str,
        surface: Option<(&str, &str)>,
        label: Option<&str>,
        u: f64,
        v: f64,
    ) -> String {
        dispatch(
            services,
            "place_free",
            &json!({
                "venueId": venue,
                "kind": kind,
                "catalogRef": catalog_ref,
                "label": label,
                "surfaceNodeId": surface.map(|s| s.0),
                "surfaceSocket": surface.map(|s| s.1),
                "mySocket": "bottom",
                "u": u,
                "v": v,
                "yaw": null,
                "trim": null,
            }),
        )
        .await
        .expect("the piece was refused")["nodeId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// A headless host over a temporary database, with the repo as its resource
    /// root: these tests patch real `.qxf` definitions, so the roles in the tree
    /// come from the role table and not from the test.
    async fn seed(directory: &Path) -> AppServices {
        let db = database::init_app_db_at(directory).await.unwrap();
        let state_db = state::init_state_db_at(directory).await.unwrap();
        auth::bootstrap_headless_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        AppServices::headless(db, state_db, storage, repo, workspaces)
    }
}
