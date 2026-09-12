use crate::database::local::groups as groups_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::handlers::fixtures::require_changed;
use crate::dispatch::{AppServices, CommandError};
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{FixtureGroup, FixtureGroupNode, GroupTreeNode, MovementConfig};
use crate::models::selection::Selection;
use crate::models::universe::UniverseState;
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
    groups_service::require_unique_name(
        &services.fixtures_root,
        &mut access,
        name.as_deref(),
        None,
    )
    .await
    .map_err(CommandError::Invalid)?;
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
    groups_service::require_unique_name(
        &services.fixtures_root,
        &mut access,
        name.as_deref(),
        Some(&id),
    )
    .await
    .map_err(CommandError::Invalid)?;
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices};

    /// A deck with a `bottom` that sits on the floor and a `top` a fixture or a
    /// tower can clamp to. Real geometry: a stub would pin half the answer.
    const DECK: &str = "stage_lab/stage_praticavel_1x1.glb";
    /// A shipped mover, so the tree's roles come out of the role table rather
    /// than out of the test.
    const MOVER: &str = "resources/fixtures/2511260420/Chauvet/Chauvet-Rogue-R2-Spot.qxf";

    #[tokio::test]
    async fn named_group_replacement_promotes_heads_and_preserves_membership_identity() {
        use super::{
            add_fixture_to_group, create_group, groups_db, groups_service, VenueAccess,
            VenueResource, Write,
        };
        use crate::database::local::venue_access::AuthorizedVenue;
        let (_dir, services, venue) = rig().await;
        let nodes = tree(&services, &venue).await;
        let fixture = nodes[0]["fixtures"][0].as_str().unwrap().to_string();
        let group = create_group(
            &services,
            venue.clone(),
            Some("Head Set".into()),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        add_fixture_to_group(&services, fixture.clone(), group.id.clone(), Some(0))
            .await
            .unwrap();
        let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue))
            .await
            .unwrap();
        let saved = groups_service::set_named_group(
            &services.fixtures_root,
            &mut access,
            "head-set",
            &[fixture.clone()],
            true,
        )
        .await
        .unwrap();
        assert_eq!(saved.id, group.id);
        let before = groups_db::venue_memberships(&mut access).await.unwrap();
        let member = before.iter().find(|m| m.group_id == group.id).unwrap();
        assert_eq!(member.head_index, groups_db::WHOLE_FIXTURE);
        let member_id: String =
            sqlx::query_scalar("SELECT id FROM fixture_group_members WHERE group_id = ?")
                .bind(&group.id)
                .fetch_one(&mut *access.connection())
                .await
                .unwrap();
        groups_service::set_named_group(
            &services.fixtures_root,
            &mut access,
            "head_set",
            &[fixture.clone(), fixture],
            true,
        )
        .await
        .unwrap();
        let after = groups_db::venue_memberships(&mut access).await.unwrap();
        let members: Vec<_> = after.iter().filter(|m| m.group_id == group.id).collect();
        assert_eq!(members.len(), 1);
        let after_id: String =
            sqlx::query_scalar("SELECT id FROM fixture_group_members WHERE group_id = ?")
                .bind(&group.id)
                .fetch_one(&mut *access.connection())
                .await
                .unwrap();
        assert_eq!(after_id, member_id);
        access.commit().await.unwrap();
    }

    #[tokio::test]
    async fn venue_group_save_rolls_back_all_members_on_failure() {
        let (_dir, services, venue) = rig().await;
        let tree = tree(&services, &venue).await;
        let fixture = tree[0]["fixtures"][0].as_str().unwrap();
        let error = dispatch(
            &services,
            "save_venue_group",
            &json!({
                "venueId":venue, "groupId":null, "label":"front wash",
                "added":[fixture, "missing-fixture"], "removed":[]
            }),
        )
        .await;
        assert!(error.is_err());
        let groups = dispatch(&services, "list_groups", &json!({"venueId":venue}))
            .await
            .unwrap();
        assert!(!groups
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["name"] == "front_wash"));
    }

    #[tokio::test]
    async fn venue_group_save_renames_and_changes_members_together() {
        let (_dir, services, venue) = rig().await;
        let nodes = tree(&services, &venue).await;
        let fixture = nodes[0]["fixtures"][0].as_str().unwrap();
        dispatch(
            &services,
            "save_venue_group",
            &json!({
                "venueId":venue,"groupId":null,"label":"front wash","added":[fixture],"removed":[]
            }),
        )
        .await
        .unwrap();
        let groups = dispatch(&services, "list_groups", &json!({"venueId":venue}))
            .await
            .unwrap();
        let id = groups
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == "front_wash")
            .unwrap()["id"]
            .as_str()
            .unwrap();
        assert_eq!(selection(&services, &venue, "front_wash").await, 1);
        dispatch(
            &services,
            "save_venue_group",
            &json!({
                "venueId":venue,"groupId":id,"label":"back wash","added":[],"removed":[fixture]
            }),
        )
        .await
        .unwrap();
        let nodes = tree(&services, &venue).await;
        assert_eq!(at(&nodes, id)["name"], "back_wash");
        assert_eq!(at(&nodes, id)["fixtures"], json!([]));
    }

    #[tokio::test]
    async fn generated_collections_are_flat_and_membership_is_owned_by_the_user() {
        let (_dir, services, venue) = rig().await;
        let before = tree(&services, &venue).await;
        assert!(before.as_array().unwrap().len() > 1);
        assert!(before
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["parentId"].is_null() && g["role"].is_null()));
        let group = &before[0];
        let id = group["id"].as_str().unwrap();
        let name = group["name"].as_str().unwrap();
        let fixture = group["fixtures"][0].as_str().unwrap();
        dispatch(
            &services,
            "save_venue_group",
            &json!({"venueId":venue,"groupId":id,"label":name,"added":[],"removed":[fixture]}),
        )
        .await
        .unwrap();
        let changed = tree(&services, &venue).await;
        assert!(!at(&changed, id)["fixtures"]
            .as_array()
            .unwrap()
            .contains(&json!(fixture)));
        dispatch(
            &services,
            "generate_venue_groups",
            &json!({"venueId":venue}),
        )
        .await
        .unwrap();
        assert_eq!(
            at(&tree(&services, &venue).await, id)["fixtures"],
            at(&changed, id)["fixtures"]
        );
    }
    #[tokio::test]
    async fn existing_venue_snapshot_preserves_canonical_names_and_then_stays_fixed() {
        let (_dir, services, venue) = rig().await;
        let before = tree(&services, &venue).await;
        // A geometry edit must not add/remove or rename any saved group.
        let node = before[0]["fixtures"][0].as_str().unwrap();
        dispatch(
            &services,
            "set_params",
            &json!({"venueId":venue,"nodeId":node,"params":{"u":0.8},"label":null}),
        )
        .await
        .unwrap();
        assert_eq!(before, tree(&services, &venue).await);
    }
    #[tokio::test]
    async fn missing_selectors_can_be_recreated_or_replaced_through_score_history() {
        let (_dir, services, venue) = rig().await;
        let uid: Option<String> = sqlx::query_scalar("SELECT uid FROM venues WHERE id=?")
            .bind(&venue)
            .fetch_one(&services.db.0)
            .await
            .unwrap();
        let track = uuid::Uuid::new_v4().to_string();
        let score = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO tracks (id,uid,track_hash,file_path) VALUES (?,?,'repair-test','/tmp/repair.wav')").bind(&track).bind(&uid).execute(&services.db.0).await.unwrap();
        sqlx::query("INSERT INTO scores (id,uid,track_id,venue_id) VALUES (?,?,?,?)")
            .bind(&score)
            .bind(&uid)
            .bind(&track)
            .bind(&venue)
            .execute(&services.db.0)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO clips (id,uid,score_id,graph,start,duration,seed,selection_json,blend_mode)
             VALUES (?2 || ':flash',?1,?2,'chase',0,4,'0',
                     '{\"expression\":\"missing_wash\"}','replace')",
        )
        .bind(uid.clone().unwrap_or_default())
        .bind(&score)
        .execute(&services.db.0)
        .await
        .unwrap();
        let missing = dispatch(&services, "missing_venue_groups", &json!({"venueId":venue}))
            .await
            .unwrap();
        assert_eq!(missing[0]["name"], "missing_wash");
        assert_eq!(missing[0]["scores"], json!([score]));
        dispatch(
            &services,
            "resolve_venue_group",
            &json!({"venueId":venue,"missing":"missing_wash","replacement":null,"fixtures":[]}),
        )
        .await
        .unwrap();
        assert_eq!(
            dispatch(&services, "missing_venue_groups", &json!({"venueId":venue}))
                .await
                .unwrap(),
            json!([])
        );
        let rows = tree(&services, &venue).await;
        let restored = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == "missing_wash")
            .unwrap();
        dispatch(&services, "delete_group", &json!({"id":restored["id"]}))
            .await
            .unwrap();
        let replacement = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|g| !g["fixtures"].as_array().unwrap().is_empty())
            .unwrap()["name"]
            .as_str()
            .unwrap();
        dispatch(&services,"resolve_venue_group",&json!({"venueId":venue,"missing":"missing_wash","replacement":replacement,"fixtures":[]})).await.unwrap();
        assert_eq!(
            dispatch(&services, "missing_venue_groups", &json!({"venueId":venue}))
                .await
                .unwrap(),
            json!([])
        );
        let stored: String =
            sqlx::query_scalar("SELECT selection_json FROM clips WHERE id = ? || ':flash'")
                .bind(&score)
                .fetch_one(&services.db.0)
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&stored).unwrap()["expression"],
            replacement
        );
        let logged: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM changes WHERE table_name = 'clips' AND row_id = ? || ':flash'",
        )
        .bind(&score)
        .fetch_one(&services.db.0)
        .await
        .unwrap();
        assert!(logged > 0, "a repair is an edit, and the log says so");
    }
    #[tokio::test]
    async fn legacy_conversion_preserves_overridden_names_and_member_sets() {
        use crate::database::local::venue_access::{
            AuthorizedVenue, VenueAccess, VenueResource, Write,
        };
        let (_dir, services, venue) = rig().await;
        let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue))
            .await
            .unwrap();
        crate::database::local::deletes::delete_where(
            access.connection(),
            "fixture_groups",
            "venue_id = ?",
            &[&venue],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE venues SET groups_initialized = 0 WHERE id=?")
            .bind(&venue)
            .execute(&mut *access.connection())
            .await
            .unwrap();
        sqlx::query("UPDATE venue_nodes SET label='same tower' WHERE venue_id=? AND kind='piece'")
            .bind(&venue)
            .execute(&mut *access.connection())
            .await
            .unwrap();
        let derived =
            crate::services::groups::GroupSources::read(&services.fixtures_root, &mut access)
                .await
                .unwrap()
                .tree();
        crate::database::local::group_overrides::put(
            &mut access,
            &crate::database::local::group_overrides::GroupOverride {
                group_id: derived[0].id.clone(),
                path: String::new(),
                label: Some("my lights".into()),
                parent_id: None,
                merged_into: None,
            },
        )
        .await
        .unwrap();
        let before =
            crate::services::groups::GroupSources::read(&services.fixtures_root, &mut access)
                .await
                .unwrap()
                .tree();
        crate::services::groups::snapshot_generated_groups(
            &services.fixtures_root,
            &mut access,
            false,
        )
        .await
        .unwrap();
        access.commit().await.unwrap();
        let after = tree(&services, &venue).await;
        for node in before {
            let saved = after
                .as_array()
                .unwrap()
                .iter()
                .find(|g| g["name"] == node.name)
                .unwrap();
            assert_eq!(saved["name"], node.name);
            assert_eq!(saved["fixtures"], json!(node.fixtures));
            assert_eq!(saved["parentId"], Value::Null);
        }
    }
    async fn tree(services: &AppServices, venue: &str) -> Value {
        dispatch(services, "list_group_tree", &json!({"venueId":venue}))
            .await
            .unwrap()
    }
    fn at<'a>(tree: &'a Value, id: &str) -> &'a Value {
        tree.as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == id)
            .unwrap()
    }
    async fn selection(services: &AppServices, venue: &str, query: &str) -> usize {
        dispatch(
            services,
            "preview_selection_query",
            &json!({"venueId":venue,"query":query,"seed":null}),
        )
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .len()
    }
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

        dispatch(
            &services,
            "generate_venue_groups",
            &json!({"venueId":venue}),
        )
        .await
        .unwrap();
        (directory, services, venue)
    }
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
    /// The principal these fixtures write as. A venue is a synced row and a
    /// synced row has an owner; `VenueAccess` admits only that owner.
    const OWNER: &str = "11111111-2222-3333-4444-555555555555";

    async fn seed(directory: &Path) -> AppServices {
        let db = database::init_app_db_at(directory).await.unwrap();
        let state_db = state::init_state_db_at(directory).await.unwrap();
        auth::install_test_session(&state_db.0, OWNER).await;
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

/// One venue edit, including the name and all membership changes, commits
/// together. A bad fixture or a name conflict cannot leave half a group saved.
pub async fn save_venue_group(
    services: &AppServices,
    venue_id: String,
    group_id: Option<String>,
    label: String,
    added: Vec<String>,
    removed: Vec<String>,
) -> Result<(), CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    groups_service::save_group_changes(
        &services.fixtures_root,
        &mut access,
        group_id.as_deref(),
        &label,
        &added,
        &removed,
    )
    .await
    .map_err(CommandError::Invalid)?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(())
}

pub async fn generate_venue_groups(
    services: &AppServices,
    venue_id: String,
) -> Result<(), CommandError> {
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    crate::services::groups::snapshot_generated_groups(&services.fixtures_root, &mut access, true)
        .await?;
    access.commit().await?;
    invalidate_venue_fixture_cache();
    Ok(())
}
