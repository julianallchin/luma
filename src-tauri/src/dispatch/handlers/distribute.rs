//! `distribute` across the host boundary.
//!
//! One verb, one transaction. The write scope is the venue, and it is opened
//! once for the whole distribution: the rows, the nodes, the edges and the
//! addresses either all land or none do, which is what "never partial
//! placement" means at this layer rather than a loop that tries to undo itself.

use std::path::Path;

use crate::database::local::venue_access::{VenueAccess, VenueResource, Write};
use crate::dispatch::{AppServices, CommandError};
use crate::models::distribute::{DistributeLayout, DistributeReport};
use crate::services::distribute as distribute_service;
use crate::services::groups::invalidate_venue_fixture_cache;

/// Patch, name, place and group `count` fixtures along one host face.
///
/// `host_node_id` of `None` is the venue root, whose two synthesized planes are
/// `floor` and `rig`; on a truss the faces are `face_-y` (underneath, beam
/// down), `face_+y`, `face_-z` and `face_+z`. Beam is the mount normal, so the
/// face *is* the aim.
///
/// # Errors
/// A face the host does not have, a joint its polarity forbids, a mode the
/// definition does not declare — all raised before anything is written. A row
/// that does not fit is **not** an error: it comes back as a report with
/// `ok: false` and the length that would make it fit.
#[allow(clippy::too_many_arguments)]
pub async fn distribute(
    services: &AppServices,
    venue_id: String,
    host_node_id: Option<String>,
    host_socket: Option<String>,
    fixture_path: String,
    mode_name: String,
    count: usize,
    layout: DistributeLayout,
    label_prefix: Option<String>,
) -> Result<DistributeReport, CommandError> {
    let relative = confine(&fixture_path)?;
    crate::venue_graph::ensure_migrated(&services.db.0, &venue_id, &services.fixtures_root).await?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;

    let report = distribute_service::distribute(
        &mut access,
        &services.fixtures_root,
        distribute_service::Request {
            host_node: host_node_id.as_deref(),
            host_socket: host_socket.as_deref(),
            fixture_path: relative,
            mode_name: &mode_name,
            count,
            layout: layout.into(),
            label_prefix: label_prefix.as_deref(),
        },
    )
    .await
    .map_err(CommandError::Invalid)?;

    // A refusal never reaches here with rows behind it, but it still opened a
    // transaction; committing an empty one is cheaper than branching, and the
    // patch republish below is a no-op when nothing changed.
    let patch = crate::database::local::fixtures::get_patched_fixtures(&mut access).await?;
    access.commit().await?;
    crate::database::local::venue_graph::graph_committed();
    if let Some(artnet) = services.artnet.as_ref() {
        artnet.update_patch(patch);
    }
    invalidate_venue_fixture_cache();
    Ok(report.into())
}

/// Reject a definition path that would escape the fixtures root.
///
/// The same constraint [`super::fixtures`] applies, at the same seam and for
/// the same reason: the path is joined onto a root directory, so it is
/// constrained where it is joined rather than trusted where it came from.
fn confine(path: &str) -> Result<&str, CommandError> {
    let relative = Path::new(path);
    let escapes = relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    });
    if escapes {
        return Err(CommandError::Invalid(format!(
            "fixture path escapes the fixtures root: {path}"
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices, CommandError};

    /// A real definition, because the fit rule reads its QLC+ physical block:
    /// the Rogue R2 Spot is 343 mm across, so eight of them claim 2.744 m and a
    /// 4 m truss holds them with room to spare.
    const MOVER: &str = "Chauvet/Chauvet-Rogue-R2-Spot.qxf";
    const MOVER_MODEL: &str = "Rogue R2 Spot";
    const MOVER_MODE: &str = "18 Channel";
    const MOVER_WIDTH_M: f64 = 0.343;
    const TRUSS: &str = "truss/straight";
    const DECK: &str = "stage_lab/stage_praticavel_2x1x1.glb";

    // -----------------------------------------------------------------------
    // The headline case
    // -----------------------------------------------------------------------

    /// Eight movers on a 4 m truss face: eight rows, eight nodes, contiguous
    /// addresses in physical order, `Rogue R2 Spot 1..8`, and a group path each.
    #[tokio::test]
    async fn eight_movers_on_a_four_metre_truss() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(&services, &venue, &run, "face_-y", 8, even())
            .await
            .unwrap();

        assert_eq!(report["refusal"], json!(null), "{report}");
        let placed = report["fixtures"].as_array().unwrap();
        assert_eq!(placed.len(), 8);

        // Eight patch rows and eight graph nodes, one apiece.
        assert_eq!(patch(&services, &venue).await.len(), 8);
        let ids: Vec<&str> = placed.iter().map(|f| f["id"].as_str().unwrap()).collect();
        let nodes = graph_nodes(&services, &venue).await;
        for id in &ids {
            assert!(nodes.contains(&(*id).to_string()), "{id} has no venue node");
        }

        // The labels continue the venue's per-model numbering from one.
        let labels: Vec<&str> = placed
            .iter()
            .map(|f| f["label"].as_str().unwrap())
            .collect();
        let expected: Vec<String> = (1..=8).map(|n| format!("{MOVER_MODEL} {n}")).collect();
        assert_eq!(labels, expected);

        // Addresses are contiguous, and they run in the order the row runs in.
        let along: Vec<f64> = placed
            .iter()
            .map(|f| f["alongM"].as_f64().unwrap())
            .collect();
        assert!(
            along.windows(2).all(|w| w[0] < w[1]),
            "the row is not in physical order: {along:?}"
        );
        let addresses: Vec<u64> = placed
            .iter()
            .map(|f| f["address"].as_u64().unwrap())
            .collect();
        assert!(
            addresses.windows(2).all(|w| w[1] == w[0] + 18),
            "addresses are not contiguous at the mode's width: {addresses:?}"
        );
        let universes: Vec<u64> = placed
            .iter()
            .map(|f| f["universe"].as_u64().unwrap())
            .collect();
        assert!(
            universes.windows(2).all(|w| w[0] == w[1]),
            "one run, one universe: {universes:?}"
        );

        // Every fixture is filed somewhere, and they are all filed together —
        // one distribution is one row of the derived tree.
        let paths: Vec<&Vec<Value>> = placed
            .iter()
            .map(|f| f["groupPath"].as_array().unwrap())
            .collect();
        assert!(
            paths.iter().all(|path| !path.is_empty()),
            "a fixture landed in no group: {paths:?}"
        );
        assert!(
            paths.windows(2).all(|w| w[0] == w[1]),
            "one distribution split across group rows: {paths:?}"
        );
    }

    /// The row spans the face it was given, and stays on it. Measured off the
    /// reported offsets rather than asserted against literals: the margin is
    /// half a fixture, and the fixture's width comes out of its `.qxf`.
    #[tokio::test]
    async fn the_row_fills_the_face_and_stays_on_it() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(&services, &venue, &run, "face_-y", 8, even())
            .await
            .unwrap();
        let along = offsets(&report);

        let half = MOVER_WIDTH_M / 2.0;
        assert!((along[0] + 2.0 - half).abs() < 1e-6, "{along:?}");
        assert!((along[7] - 2.0 + half).abs() < 1e-6, "{along:?}");
        assert!(
            ((along[0] + along[7]) / 2.0).abs() < 1e-9,
            "the row is off centre: {along:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Fit
    // -----------------------------------------------------------------------

    /// The acceptance test, whole: a refusal states a length, that length is
    /// fed into the run's `span` through `set_params`, and the *same* call then
    /// succeeds. No literal length is asserted anywhere in it.
    #[tokio::test]
    async fn the_stated_need_extends_the_run_into_a_fit() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 1.0).await;

        let refusal = spread(&services, &venue, &run, "face_-y", 8, even())
            .await
            .unwrap();
        assert_eq!(refusal["refusal"]["kind"], json!("tooLong"), "{refusal}");
        assert!(refusal["fixtures"].as_array().unwrap().is_empty());
        assert_eq!(
            patch(&services, &venue).await.len(),
            0,
            "a refusal wrote rows"
        );

        let fit = &refusal["refusal"];
        let needed = fit["neededM"].as_f64().unwrap();
        assert!((fit["availableM"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(fit["extendNodeId"], json!(run));

        dispatch(
            &services,
            "set_params",
            &json!({
                "venueId": venue,
                "nodeId": run,
                "params": { "span": needed },
                "label": null,
            }),
        )
        .await
        .expect("the run would not extend");

        let retry = spread(&services, &venue, &run, "face_-y", 8, even())
            .await
            .unwrap();
        assert_eq!(
            retry["refusal"],
            json!(null),
            "extending to the stated {needed} m still did not fit: {retry}"
        );
        assert_eq!(retry["fixtures"].as_array().unwrap().len(), 8);
    }

    /// A refusal is a report, not an error, and it leaves the database exactly
    /// as it found it — including the graph, not just the patch.
    #[tokio::test]
    async fn a_refusal_writes_nothing_at_all() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 1.0).await;
        let nodes_before = graph_nodes(&services, &venue).await.len();

        spread(&services, &venue, &run, "face_-y", 8, even())
            .await
            .unwrap();
        spread(
            &services,
            &venue,
            &run,
            "face_-y",
            4,
            json!({ "kind": "spacing", "metres": 2.0 }),
        )
        .await
        .unwrap();

        assert_eq!(patch(&services, &venue).await.len(), 0);
        assert_eq!(graph_nodes(&services, &venue).await.len(), nodes_before);
    }

    /// Spacing that overruns is refused by the band it produces, and its stated
    /// need makes *that* call fit too — the fit rule is one rule, not one per
    /// layout.
    #[tokio::test]
    async fn a_spacing_that_overruns_states_a_need_that_works() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 2.0).await;
        let layout = json!({ "kind": "spacing", "metres": 1.0 });

        let refusal = spread(&services, &venue, &run, "face_-y", 5, layout.clone())
            .await
            .unwrap();
        assert_eq!(refusal["refusal"]["kind"], json!("tooLong"), "{refusal}");
        let needed = refusal["refusal"]["neededM"].as_f64().unwrap();

        dispatch(
            &services,
            "set_params",
            &json!({ "venueId": venue, "nodeId": run, "params": { "span": needed }, "label": null }),
        )
        .await
        .unwrap();
        let retry = spread(&services, &venue, &run, "face_-y", 5, layout)
            .await
            .unwrap();
        assert_eq!(retry["refusal"], json!(null), "{retry}");
    }

    /// An exact fit is a fit; one more is not. Both measured against the same
    /// definition's own width rather than a number typed here.
    #[tokio::test]
    async fn an_exact_fit_is_admitted_and_one_more_is_not() {
        let (_dir, services, venue) = room().await;
        // Eleven bodies is 3.773 m; twelve is 4.116 m.
        let run = truss(&services, &venue, 4.0).await;

        assert_eq!(
            spread(&services, &venue, &run, "face_-y", 11, even())
                .await
                .unwrap()["refusal"],
            json!(null)
        );
        let over = spread(&services, &venue, &run, "face_-y", 12, even())
            .await
            .unwrap();
        assert_eq!(over["refusal"]["kind"], json!("tooLong"), "{over}");
    }

    // -----------------------------------------------------------------------
    // Layouts and hosts
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spacing_is_the_pitch_it_says_it_is() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(
            &services,
            &venue,
            &run,
            "face_-y",
            4,
            json!({ "kind": "spacing", "metres": 0.75 }),
        )
        .await
        .unwrap();
        let along = offsets(&report);
        for pair in along.windows(2) {
            assert!((pair[1] - pair[0] - 0.75).abs() < 1e-9, "{along:?}");
        }
        assert!(
            ((along[0] + along[3]) / 2.0).abs() < 1e-9,
            "off centre: {along:?}"
        );
    }

    /// A span narrows the segment; the same count then sits inside that half of
    /// the truss rather than across all of it.
    #[tokio::test]
    async fn a_span_lays_the_row_inside_its_fraction() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(
            &services,
            &venue,
            &run,
            "face_-y",
            4,
            json!({ "kind": "span", "from": 0.5, "to": 1.0 }),
        )
        .await
        .unwrap();
        let along = offsets(&report);
        assert!(along[0] >= 0.0, "the row escaped its span: {along:?}");
        assert!(along[3] <= 2.0, "the row escaped its span: {along:?}");
        assert!(
            ((along[0] + along[3]) / 2.0 - 1.0).abs() < 1e-9,
            "not centred on the span: {along:?}"
        );
    }

    /// Beam is the mount normal, so the face chooses the aim. The root's two
    /// planes pin the sign — floor up, grid down — and a truss's opposite faces
    /// pin the relation, which is all a stick bolted to a deck corner can say
    /// about world directions.
    #[tokio::test]
    async fn the_face_chooses_which_way_the_row_points() {
        let (_dir, services, venue) = room().await;
        distribute_on_root(&services, &venue, "floor", 2).await;
        assert!(
            facings(&services, &venue)
                .await
                .iter()
                .all(|(_, z)| *z > 0.9),
            "a row on the floor should aim up"
        );

        let (_dir, services, venue) = room().await;
        distribute_on_root(&services, &venue, "rig", 2).await;
        assert!(
            facings(&services, &venue)
                .await
                .iter()
                .all(|(_, z)| *z < -0.9),
            "a row on the grid should aim down"
        );

        // Opposite faces of a truss aim opposite ways, whichever way the truss
        // itself is turned.
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let under = spread(&services, &venue, &run, "face_-y", 1, even())
            .await
            .unwrap();
        let over = spread(&services, &venue, &run, "face_+y", 1, even())
            .await
            .unwrap();
        let beams = beams(&services, &venue).await;
        let a = beams[under["fixtures"][0]["id"].as_str().unwrap()];
        let b = beams[over["fixtures"][0]["id"].as_str().unwrap()];
        let sum = [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        assert!(
            sum.iter().all(|component| component.abs() < 1e-9),
            "under {a:?} and over {b:?} are not opposite"
        );
    }

    /// A deck top is a host like any other, and it is a *bounded* one — its
    /// length is the measured GLB's, so an overlong row on it is refused too.
    #[tokio::test]
    async fn a_deck_top_hosts_a_row_and_can_be_overrun() {
        let (_dir, services, venue) = room().await;
        let deck = place_deck(&services, &venue).await;

        let report = spread(&services, &venue, &deck, "top", 3, even())
            .await
            .unwrap();
        assert_eq!(report["refusal"], json!(null), "{report}");
        assert_eq!(report["fixtures"].as_array().unwrap().len(), 3);

        let over = spread(&services, &venue, &deck, "top", 40, even())
            .await
            .unwrap();
        assert_eq!(
            over["refusal"]["kind"],
            json!("tooLong"),
            "a deck is not forty movers long"
        );
    }

    /// The floor is a plane: no ends, so nothing overruns it however many are
    /// asked for. `null` host is the root, `null` socket is the floor.
    #[tokio::test]
    async fn the_floor_is_a_host_and_never_refuses() {
        let (_dir, services, venue) = room().await;
        let report = dispatch(
            &services,
            "distribute",
            &json!({
                "venueId": venue,
                "hostNodeId": null,
                "hostSocket": null,
                "fixturePath": MOVER,
                "modeName": MOVER_MODE,
                "count": 30,
                "layout": { "kind": "spacing", "metres": 1.0 },
                "labelPrefix": null,
            }),
        )
        .await
        .unwrap();
        assert_eq!(report["refusal"], json!(null), "{report}");
        assert_eq!(report["fixtures"].as_array().unwrap().len(), 30);
    }

    // -----------------------------------------------------------------------
    // Edges of the vocabulary
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn one_fixture_sits_in_the_middle_of_the_face() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(&services, &venue, &run, "face_-y", 1, even())
            .await
            .unwrap();
        assert_eq!(report["refusal"], json!(null));
        assert!(offsets(&report)[0].abs() < 1e-9);
    }

    /// Nothing asked for, nothing written — and no error either.
    #[tokio::test]
    async fn a_count_of_none_places_none() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(&services, &venue, &run, "face_-y", 0, even())
            .await
            .unwrap();
        assert_eq!(report["refusal"], json!(null), "{report}");
        assert!(report["fixtures"].as_array().unwrap().is_empty());
        assert_eq!(patch(&services, &venue).await.len(), 0);
    }

    /// A label prefix replaces the model as the naming term — the same rule,
    /// counting a different word.
    #[tokio::test]
    async fn a_label_prefix_names_the_row() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = dispatch(
            &services,
            "distribute",
            &json!({
                "venueId": venue,
                "hostNodeId": run,
                "hostSocket": "face_-y",
                "fixturePath": MOVER,
                "modeName": MOVER_MODE,
                "count": 3,
                "layout": even(),
                "labelPrefix": "Key",
            }),
        )
        .await
        .unwrap();
        let labels: Vec<&str> = report["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Key 1", "Key 2", "Key 3"]);
    }

    /// A second distribution continues the venue's numbering rather than
    /// restarting it — the counter is the venue's, not the call's.
    #[tokio::test]
    async fn a_second_distribution_continues_the_numbering() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        spread(&services, &venue, &run, "face_-y", 3, even())
            .await
            .unwrap();
        let second = spread(&services, &venue, &run, "face_+y", 2, even())
            .await
            .unwrap();
        let labels: Vec<&str> = second["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            labels,
            [format!("{MOVER_MODEL} 4"), format!("{MOVER_MODEL} 5")]
        );
    }

    /// Same call twice on two identical venues gives the same rows, ids aside.
    #[tokio::test]
    async fn the_same_call_twice_gives_the_same_rig() {
        let describe = |report: &Value| -> Vec<(String, f64, u64, u64)> {
            report["fixtures"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    (
                        f["label"].as_str().unwrap().to_string(),
                        f["alongM"].as_f64().unwrap(),
                        f["universe"].as_u64().unwrap(),
                        f["address"].as_u64().unwrap(),
                    )
                })
                .collect()
        };
        let (_dir, services, venue) = room().await;
        let other = venue_named(&services, "Twin").await;
        let a = truss(&services, &venue, 4.0).await;
        let b = truss(&services, &other, 4.0).await;
        let first = spread(&services, &venue, &a, "face_-y", 6, even())
            .await
            .unwrap();
        let second = spread(&services, &other, &b, "face_-y", 6, even())
            .await
            .unwrap();
        assert_eq!(describe(&first), describe(&second));
    }

    /// A face the host does not have is a hard error, before any write.
    #[tokio::test]
    async fn a_face_the_host_does_not_have_is_refused_by_name() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let error = spread(&services, &venue, &run, "face_-w", 4, even())
            .await
            .expect_err("a stick has no face `-w`");
        let CommandError::Invalid(message) = &error else {
            panic!("expected a refusal, got {error}");
        };
        assert!(message.contains("face_-w"), "{message}");
        assert_eq!(patch(&services, &venue).await.len(), 0);
    }

    /// A truss *end* is a bolting socket, not a surface. Polarity refuses the
    /// joint rather than this command listing which sockets are faces.
    #[tokio::test]
    async fn a_truss_end_is_not_a_face_to_hang_from() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        spread(&services, &venue, &run, "end_a", 2, even())
            .await
            .expect_err("a fixture was clamped to a bolt plate");
        assert_eq!(patch(&services, &venue).await.len(), 0);
    }

    /// A node id is not authorization: another venue's truss is refused before
    /// anything is written.
    #[tokio::test]
    async fn a_host_in_another_venue_is_refused() {
        let (_dir, services, venue) = room().await;
        let other = venue_named(&services, "Other room").await;
        let theirs = truss(&services, &other, 4.0).await;
        spread(&services, &venue, &theirs, "face_-y", 4, even())
            .await
            .expect_err("a row reached across venues");
        assert_eq!(patch(&services, &venue).await.len(), 0);
    }

    // -----------------------------------------------------------------------
    // The other door: the patch page's non-placed add
    // -----------------------------------------------------------------------

    /// The seam both critics found. A fixture patched *outside* a distribution
    /// must still be a node in the graph — with no edge, so the resolver
    /// reports it unplaced and the tray can find it to drag onto a truss.
    #[tokio::test]
    async fn a_fixture_patched_on_the_patch_page_lands_in_the_tray() {
        let (_dir, services, venue) = room().await;
        let id = patch_one(&services, &venue, 1, 1).await;

        assert!(
            graph_nodes(&services, &venue).await.contains(&id),
            "the patch page's add left no venue node"
        );
        let resolved = dispatch(
            &services,
            "get_resolved_venue",
            &json!({ "venueId": venue }),
        )
        .await
        .unwrap();
        let unplaced: Vec<&str> = resolved["unplaced"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["nodeId"].as_str().unwrap())
            .collect();
        assert_eq!(unplaced, [id.as_str()], "the tray is empty: {resolved}");
        assert!(
            !resolved["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["id"] == json!(id)),
            "a tray fixture got a pose"
        );
    }

    /// And it can then be dragged onto a truss, which is what the tray is for.
    #[tokio::test]
    async fn a_tray_fixture_can_be_reattached_to_a_truss() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let id = patch_one(&services, &venue, 1, 1).await;

        let report = dispatch(
            &services,
            "reattach",
            &json!({
                "venueId": venue,
                "nodeId": id,
                "parentId": run,
                "mySocket": "clamp",
                "theirSocket": "face_-y",
                "yaw": null,
            }),
        )
        .await
        .expect("the tray fixture would not hang");
        assert_eq!(report["refusal"], json!(null), "{report}");
        assert!(report["venue"]["unplaced"].as_array().unwrap().is_empty());
    }

    /// The patch page's add takes the venue's next `<model> <n>` from the
    /// backend — the frontend no longer has a naming rule to disagree with.
    #[tokio::test]
    async fn the_backend_names_a_patch_page_add() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        spread(&services, &venue, &run, "face_-y", 2, even())
            .await
            .unwrap();
        patch_one(&services, &venue, 9, 1).await;

        let labels: Vec<String> = patch(&services, &venue)
            .await
            .iter()
            .map(|f| f["label"].as_str().unwrap().to_string())
            .collect();
        assert!(
            labels.contains(&format!("{MOVER_MODEL} 3")),
            "the third mover was not named third: {labels:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Two rows on one run
    // -----------------------------------------------------------------------

    /// The rule, verbatim: two distributions on one run interleave in
    /// **physical** order, not creation order. The second row is laid at the
    /// *near* end of the truss, so an allocator that appended would give it the
    /// higher addresses; the one that derives gives it the lower ones.
    #[tokio::test]
    async fn two_rows_on_one_run_interleave_by_position() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;

        let far = spread(&services, &venue, &run, "face_-y", 2, span(0.6, 1.0))
            .await
            .unwrap();
        let near = spread(&services, &venue, &run, "face_-y", 2, span(0.0, 0.4))
            .await
            .unwrap();
        assert_eq!(near["refusal"], json!(null), "{near}");

        // The second report already knows it took the low addresses.
        assert!(
            offsets(&near).iter().all(|u| *u < 0.0),
            "the near row is not at the near end: {:?}",
            offsets(&near)
        );

        // And the patch, read back, runs in face order across both rows.
        let mut row: Vec<(f64, &str)> = far["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .chain(near["fixtures"].as_array().unwrap())
            .map(|f| (f["alongM"].as_f64().unwrap(), f["id"].as_str().unwrap()))
            .collect();
        row.sort_by(|a, b| a.0.total_cmp(&b.0));
        let ids: Vec<&str> = row.iter().map(|(_, id)| *id).collect();
        let addresses = addresses_of(&services, &venue, &ids).await;
        assert!(
            addresses.windows(2).all(|w| w[1] == w[0] + 18),
            "sorted along the truss, the addresses are {addresses:?}"
        );
    }

    /// The first row's *stored* addresses move when the second row lands under
    /// them — the whole point of re-deriving rather than appending.
    #[tokio::test]
    async fn the_earlier_row_is_re_addressed_by_the_later_one() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let far = spread(&services, &venue, &run, "face_-y", 2, span(0.6, 1.0))
            .await
            .unwrap();
        let ids: Vec<&str> = far["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap())
            .collect();
        assert_eq!(addresses_of(&services, &venue, &ids).await, [1, 19]);

        spread(&services, &venue, &run, "face_-y", 2, span(0.0, 0.4))
            .await
            .unwrap();
        assert_eq!(
            addresses_of(&services, &venue, &ids).await,
            [37, 55],
            "the earlier row did not make room"
        );
    }

    /// A pinned address is not re-derived, here as anywhere else: the row
    /// around it flows past.
    #[tokio::test]
    async fn a_pinned_address_survives_a_later_row() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let far = spread(&services, &venue, &run, "face_-y", 1, span(0.8, 1.0))
            .await
            .unwrap();
        let pinned = far["fixtures"][0]["id"].as_str().unwrap().to_string();
        dispatch(
            &services,
            "set_fixture_address",
            &json!({ "venueId": venue, "id": pinned, "universe": 1, "address": 200 }),
        )
        .await
        .unwrap();

        spread(&services, &venue, &run, "face_-y", 2, span(0.0, 0.4))
            .await
            .unwrap();
        assert_eq!(addresses_of(&services, &venue, &[&pinned]).await, [200]);
    }

    /// Two `even` rows both want the whole face, so the second is refused —
    /// and the refusal names what is in the way rather than stacking on it.
    #[tokio::test]
    async fn a_second_row_over_the_first_is_refused_by_name() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        spread(&services, &venue, &run, "face_-y", 4, even())
            .await
            .unwrap();

        let over = spread(&services, &venue, &run, "face_-y", 4, even())
            .await
            .unwrap();
        assert_eq!(over["refusal"]["kind"], json!("overlap"), "{over}");
        let held: Vec<&str> = over["refusal"]["heldBy"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            held,
            [
                "Rogue R2 Spot 1",
                "Rogue R2 Spot 2",
                "Rogue R2 Spot 3",
                "Rogue R2 Spot 4"
            ]
        );
        assert_eq!(
            patch(&services, &venue).await.len(),
            4,
            "a refusal wrote rows"
        );

        // The other face of the same truss is a different face, and free.
        let under = spread(&services, &venue, &run, "face_+y", 4, even())
            .await
            .unwrap();
        assert_eq!(under["refusal"], json!(null), "{under}");
    }

    /// A row beside another on the same face is not an overlap.
    #[tokio::test]
    async fn two_rows_that_clear_each_other_are_both_admitted() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        spread(&services, &venue, &run, "face_-y", 2, span(0.6, 1.0))
            .await
            .unwrap();
        let near = spread(&services, &venue, &run, "face_-y", 2, span(0.0, 0.4))
            .await
            .unwrap();
        assert_eq!(near["refusal"], json!(null), "{near}");
    }

    // -----------------------------------------------------------------------
    // Deleting
    // -----------------------------------------------------------------------

    /// The dual of creating one. A deleted fixture is gone from the patch *and*
    /// from the graph — the resolver stops posing it, the derived tree stops
    /// counting it, and nothing is left to draw.
    #[tokio::test]
    async fn deleting_a_fixture_takes_its_node_with_it() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(&services, &venue, &run, "face_-y", 3, even())
            .await
            .unwrap();
        let victim = report["fixtures"][1]["id"].as_str().unwrap().to_string();

        dispatch(
            &services,
            "remove_patched_fixture",
            &json!({ "venueId": venue, "id": victim }),
        )
        .await
        .expect("the fixture would not delete");

        assert_eq!(patch(&services, &venue).await.len(), 2);
        assert!(!graph_nodes(&services, &venue).await.contains(&victim));
        assert_eq!(facings(&services, &venue).await.len(), 2, "still posed");
        assert_eq!(
            group_members(&services, &venue).await,
            2,
            "the derived tree still counts it"
        );
    }

    /// And the patch page's own add, deleted the same way, leaves no node —
    /// the tray does not fill up with fixtures nobody has.
    #[tokio::test]
    async fn deleting_a_tray_fixture_leaves_no_node() {
        let (_dir, services, venue) = room().await;
        let id = patch_one(&services, &venue, 1, 1).await;
        dispatch(
            &services,
            "remove_patched_fixture",
            &json!({ "venueId": venue, "id": id }),
        )
        .await
        .unwrap();

        assert!(
            graph_nodes(&services, &venue).await.is_empty() || {
                !graph_nodes(&services, &venue).await.contains(&id)
            }
        );
        let resolved = dispatch(
            &services,
            "get_resolved_venue",
            &json!({ "venueId": venue }),
        )
        .await
        .unwrap();
        assert!(
            resolved["unplaced"].as_array().unwrap().is_empty(),
            "the tray still holds a deleted fixture: {resolved}"
        );
    }

    /// Pulling a truss down loses the rig its shape, not its lights: the
    /// fixtures survive as inventory, in the tray, ready to hang somewhere
    /// else.
    #[tokio::test]
    async fn deleting_a_truss_trays_its_lights() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(&services, &venue, &run, "face_-y", 3, even())
            .await
            .unwrap();
        let ids: Vec<String> = report["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap().to_string())
            .collect();

        let venue_after = dispatch(
            &services,
            "delete_subtree",
            &json!({ "venueId": venue, "nodeId": run }),
        )
        .await
        .expect("the truss would not come down");

        assert_eq!(
            patch(&services, &venue).await.len(),
            3,
            "the lights went too"
        );
        let unplaced: Vec<&str> = venue_after["unplaced"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["nodeId"].as_str().unwrap())
            .collect();
        for id in &ids {
            assert!(unplaced.contains(&id.as_str()), "{id} is not in the tray");
        }
        assert!(
            !venue_after["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["id"] == json!(run)),
            "the truss is still there"
        );
    }

    /// `delete_subtree` aimed straight at a fixture *does* delete it — that is
    /// the builder saying "this light, gone", and it goes through the same one
    /// door the patch page uses.
    #[tokio::test]
    async fn deleting_a_fixture_node_deletes_the_fixture() {
        let (_dir, services, venue) = room().await;
        let run = truss(&services, &venue, 4.0).await;
        let report = spread(&services, &venue, &run, "face_-y", 3, even())
            .await
            .unwrap();
        let victim = report["fixtures"][0]["id"].as_str().unwrap().to_string();

        dispatch(
            &services,
            "delete_subtree",
            &json!({ "venueId": venue, "nodeId": victim }),
        )
        .await
        .unwrap();

        assert_eq!(patch(&services, &venue).await.len(), 2);
        assert!(!graph_nodes(&services, &venue).await.contains(&victim));
    }

    // -----------------------------------------------------------------------
    // Plumbing
    // -----------------------------------------------------------------------

    async fn distribute_on_root(services: &AppServices, venue: &str, socket: &str, count: usize) {
        let report = dispatch(
            services,
            "distribute",
            &json!({
                "venueId": venue,
                "hostNodeId": null,
                "hostSocket": socket,
                "fixturePath": MOVER,
                "modeName": MOVER_MODE,
                "count": count,
                "layout": { "kind": "spacing", "metres": 1.0 },
                "labelPrefix": null,
            }),
        )
        .await
        .expect("the root plane refused a row");
        assert_eq!(report["refusal"], json!(null), "{report}");
    }

    async fn beams(
        services: &AppServices,
        venue: &str,
    ) -> std::collections::BTreeMap<String, [f64; 3]> {
        dispatch(
            services,
            "get_fixture_facings",
            &json!({ "venueId": venue }),
        )
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let d = f["direction"].as_array().unwrap();
            (
                f["id"].as_str().unwrap().to_string(),
                [
                    d[0].as_f64().unwrap(),
                    d[1].as_f64().unwrap(),
                    d[2].as_f64().unwrap(),
                ],
            )
        })
        .collect()
    }

    fn even() -> Value {
        json!({ "kind": "even" })
    }

    fn span(from: f64, to: f64) -> Value {
        json!({ "kind": "span", "from": from, "to": to })
    }

    /// The stored DMX address of every named fixture, in the order named —
    /// read off the patch rather than off a report, because what a distribution
    /// *said* and what the database *holds* is exactly the thing under test.
    async fn addresses_of(services: &AppServices, venue: &str, ids: &[&str]) -> Vec<u64> {
        let rows = patch(services, venue).await;
        ids.iter()
            .map(|id| {
                rows.iter()
                    .find(|row| row["id"] == json!(id))
                    .unwrap_or_else(|| panic!("no fixture {id} in the patch"))["address"]
                    .as_u64()
                    .unwrap()
            })
            .collect()
    }

    /// How many fixtures the derived group tree accounts for, counted once.
    async fn group_members(services: &AppServices, venue: &str) -> usize {
        let tree = dispatch(services, "list_group_tree", &json!({ "venueId": venue }))
            .await
            .unwrap();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for node in tree.as_array().unwrap() {
            for fixture in node["fixtures"].as_array().unwrap() {
                seen.insert(fixture.as_str().unwrap().to_string());
            }
        }
        seen.len()
    }

    fn offsets(report: &Value) -> Vec<f64> {
        report["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["alongM"].as_f64().unwrap())
            .collect()
    }

    async fn spread(
        services: &AppServices,
        venue: &str,
        host: &str,
        socket: &str,
        count: usize,
        layout: Value,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "distribute",
            &json!({
                "venueId": venue,
                "hostNodeId": host,
                "hostSocket": socket,
                "fixturePath": MOVER,
                "modeName": MOVER_MODE,
                "count": count,
                "layout": layout,
                "labelPrefix": null,
            }),
        )
        .await
    }

    /// A stick of `span` metres, bolted to a deck corner.
    ///
    /// Bolted rather than free-standing because a truss's only held sockets are
    /// its two ends, and an end is a `truss_end` joint: there is no socket on a
    /// stick that mates a surface, so "a truss lying on the floor" is not
    /// something the catalog can express yet. It does not matter here — a face
    /// runs the truss's own span axis whichever way the truss is turned.
    async fn truss(services: &AppServices, venue: &str, span: f64) -> String {
        let deck = place_deck(services, venue).await;
        let run = dispatch(
            services,
            "attach",
            &json!({
                "venueId": venue,
                "kind": "run",
                "catalogRef": TRUSS,
                "label": null,
                "parentId": deck,
                "mySocket": "end_a",
                "theirSocket": "corner_fl",
                "yaw": null,
                "params": { "span": span },
            }),
        )
        .await
        .expect("the truss was refused")["nodeId"]
            .as_str()
            .unwrap()
            .to_string();
        run
    }

    async fn place_deck(services: &AppServices, venue: &str) -> String {
        dispatch(
            services,
            "place_free",
            &json!({
                "venueId": venue,
                "kind": "stage",
                "catalogRef": DECK,
                "label": null,
                "surfaceNodeId": null,
                "surfaceSocket": null,
                "mySocket": "bottom",
                "u": 0.0,
                "v": 0.0,
                "yaw": null,
                "trim": null,
            }),
        )
        .await
        .expect("the deck was refused")["nodeId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn patch_one(services: &AppServices, venue: &str, universe: i64, address: i64) -> String {
        dispatch(
            services,
            "patch_fixture",
            &json!({
                "venueId": venue,
                "universe": universe,
                "address": address,
                "numChannels": 18,
                "manufacturer": "Chauvet",
                "model": MOVER_MODEL,
                "modeName": MOVER_MODE,
                "fixturePath": MOVER,
                "label": null,
            }),
        )
        .await
        .expect("the patch was refused")["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn patch(services: &AppServices, venue: &str) -> Vec<Value> {
        dispatch(
            services,
            "get_patched_fixtures",
            &json!({ "venueId": venue }),
        )
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
    }

    async fn facings(services: &AppServices, venue: &str) -> Vec<(String, f64)> {
        dispatch(
            services,
            "get_fixture_facings",
            &json!({ "venueId": venue }),
        )
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["id"].as_str().unwrap().to_string(),
                f["direction"].as_array().unwrap()[2].as_f64().unwrap(),
            )
        })
        .collect()
    }

    async fn graph_nodes(services: &AppServices, venue: &str) -> Vec<String> {
        dispatch(services, "get_venue_graph", &json!({ "venueId": venue }))
            .await
            .unwrap()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect()
    }

    async fn room() -> (tempfile::TempDir, AppServices, String) {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        let venue = venue_named(&services, "Golden room").await;
        (directory, services, venue)
    }

    async fn venue_named(services: &AppServices, name: &str) -> String {
        dispatch(
            services,
            "create_venue",
            &json!({ "name": name, "description": null }),
        )
        .await
        .expect("the venue was not created")["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// A headless host over a temporary database, pointed at the **real**
    /// fixtures root: the fit rule reads a definition's physical block, so a
    /// stub definition would pin half the answer with a number typed here.
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
        AppServices::headless(db, state_db, storage, fixtures_root(), workspaces)
    }

    fn fixtures_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/fixtures/2511260420")
    }
}
