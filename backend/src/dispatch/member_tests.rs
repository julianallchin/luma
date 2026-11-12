//! A venue shared by code, walked command by command as one of its members.
//!
//! Members are collaborators: everything in the venue is theirs to edit, and
//! the server's history says who changed what. Only the venue's door — its
//! share code and who holds a membership — stays the owner's.

use serde_json::{json, Value};

use crate::database::local::auth::arm_write_admission;
use crate::database::local::write_admission::{enter_remote_writes, leave_remote_writes};
use crate::dispatch::{dispatch, AppServices};
use crate::error::CommandError;
use crate::test_support::{signed_in, TEST_OWNER};
use luma_scene::environment::{Floor, VenueEnvironment, VenueHaze};

/// The account that joined [`TEST_OWNER`]'s venue with its code.
const MEMBER: &str = "bbbbbbbb-0000-0000-0000-000000000000";
const DECK: &str = "stage_lab/stage_praticavel_1x1.glb";
const MOVER: &str = "Chauvet/Chauvet-Rogue-R2-Spot.qxf";

async fn run(services: &AppServices, name: &str, args: Value) -> Value {
    dispatch(services, name, &args)
        .await
        .unwrap_or_else(|error| panic!("`{name}` failed: {error}"))
}

/// [`TEST_OWNER`]'s venue with a stage and a mover on it, and a code out.
/// Answers with [`MEMBER`] admitted, as on the member's device once the
/// venue has downloaded.
async fn shared(directory: &std::path::Path) -> (AppServices, String, String) {
    let fixtures_root = crate::resources::source_fixtures_root().expect("the shipped bundle");
    let services = signed_in(directory, fixtures_root).await;
    let venue = run(
        &services,
        "create_venue",
        json!({ "name": "Shared room", "description": null }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stage = place(&services, &venue, None).await;
    hang(&services, &venue, &stage).await;
    run(&services, "rotate_share_code", json!({ "venueId": venue })).await;

    // The membership is the server's to write; it arrives by download.
    let mut download = services.db.0.begin_with("BEGIN IMMEDIATE").await.unwrap();
    crate::sync::triggers::local_only(&mut download)
        .await
        .unwrap();
    enter_remote_writes(&mut download).await.unwrap();
    sqlx::query(
        "INSERT INTO venue_members (id, uid, venue_id, role) VALUES (?3, ?2, ?1, 'member')",
    )
    .bind(&venue)
    .bind(MEMBER)
    .bind(uuid::Uuid::new_v4().to_string())
    .execute(&mut *download)
    .await
    .unwrap();
    leave_remote_writes(&mut download).await.unwrap();
    crate::sync::triggers::uploading(&mut download)
        .await
        .unwrap();
    download.commit().await.unwrap();

    arm_write_admission(&services.db.0, Some(MEMBER))
        .await
        .unwrap();
    (services, venue, stage)
}

async fn place(services: &AppServices, venue: &str, on: Option<&str>) -> String {
    run(
        services,
        "place_free",
        json!({
            "venueId": venue,
            "kind": if on.is_some() { "piece" } else { "stage" },
            "catalogRef": DECK,
            "label": null,
            "surfaceNodeId": on,
            "surfaceSocket": on.map(|_| "top"),
            "mySocket": "bottom",
            "u": 0.0,
            "v": 0.0,
            "yaw": null,
            "trim": null,
            "params": null,
        }),
    )
    .await["nodeId"]
        .as_str()
        .unwrap_or_else(|| panic!("place_free named no node"))
        .to_owned()
}

async fn hang(services: &AppServices, venue: &str, on: &str) -> String {
    let report = run(
        services,
        "distribute",
        json!({
            "venueId": venue,
            "hostNodeId": on,
            "hostSocket": "top",
            "fixturePath": MOVER,
            "modeName": "18 Channel",
            "count": 1,
            "layout": { "kind": "even" },
            "labelPrefix": null,
        }),
    )
    .await;
    report["fixtures"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("nothing was hung: {}", report["refusal"]))
        .to_owned()
}

fn unauthorized(result: Result<Value, CommandError>, what: &str) {
    match result {
        Err(CommandError::Unauthorized(_)) => {}
        other => panic!("a member could {what}: {other:?}"),
    }
}

#[tokio::test]
async fn a_member_edits_the_room_its_rig_and_its_set() {
    let directory = tempfile::tempdir().unwrap();
    let (services, venue, stage) = shared(directory.path()).await;

    // The visualizer's environment, floor and haze.
    let environment = VenueEnvironment::Indoor {
        house_level: 0.25,
        floor: Floor::Concrete,
    };
    let haze = VenueHaze {
        enabled: false,
        ..VenueHaze::default()
    };
    run(
        &services,
        "set_venue_environment",
        json!({ "venueId": venue, "environment": environment }),
    )
    .await;
    run(
        &services,
        "set_venue_haze",
        json!({ "venueId": venue, "haze": haze }),
    )
    .await;
    let stored = run(&services, "get_venue", json!({ "id": venue })).await;
    assert_eq!(stored["role"], "member");
    assert_eq!(
        serde_json::from_value::<VenueEnvironment>(stored["environment"].clone()).unwrap(),
        environment
    );
    assert_eq!(
        serde_json::from_value::<VenueHaze>(stored["haze"].clone()).unwrap(),
        haze
    );

    // The stage: build on it, change it, take a piece away.
    let piece = place(&services, &venue, Some(&stage)).await;
    run(
        &services,
        "set_params",
        json!({ "venueId": venue, "nodeId": piece, "params": {}, "label": "Riser" }),
    )
    .await;
    run(
        &services,
        "duplicate",
        json!({
            "venueId": venue, "nodeId": piece, "parentId": stage,
            "theirSocket": "top", "flip": null,
        }),
    )
    .await;
    run(
        &services,
        "delete_subtree",
        json!({ "venueId": venue, "nodeId": piece }),
    )
    .await;

    // The patch: hang a fixture, rename, readdress, pin and unhang it.
    let riser = place(&services, &venue, Some(&stage)).await;
    let fixture = hang(&services, &venue, &riser).await;
    run(
        &services,
        "rename_patched_fixture",
        json!({ "venueId": venue, "id": fixture, "label": "Spot" }),
    )
    .await;
    run(
        &services,
        "set_fixture_address",
        json!({ "venueId": venue, "id": fixture, "universe": 2, "address": 101 }),
    )
    .await;
    run(
        &services,
        "set_address_pinned",
        json!({ "venueId": venue, "id": fixture, "pinned": true }),
    )
    .await;
    run(&services, "auto_patch", json!({ "venueId": venue })).await;
    run(
        &services,
        "remove_patched_fixture",
        json!({ "venueId": venue, "id": fixture }),
    )
    .await;

    // Groups.
    run(
        &services,
        "generate_venue_groups",
        json!({ "venueId": venue }),
    )
    .await;
    let patched = run(
        &services,
        "get_patched_fixtures",
        json!({ "venueId": venue }),
    )
    .await;
    let mover = patched[0]["id"].as_str().unwrap().to_owned();
    run(
        &services,
        "save_venue_group",
        json!({
            "venueId": venue, "groupId": null, "label": "Member picks",
            "added": [mover], "removed": [],
        }),
    )
    .await;
    let groups = run(&services, "list_groups", json!({ "venueId": venue })).await;
    let picked = groups
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["name"] == "member_picks")
        .expect("the member's group was not saved")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    run(&services, "delete_group", json!({ "id": picked })).await;

    // A song of the member's own, and a score of theirs on it.
    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('song', ?, 'h', '/song.wav')",
    )
    .bind(MEMBER)
    .execute(&services.db.0)
    .await
    .unwrap();
    let score = run(
        &services,
        "create_score",
        json!({
            "requestId": "6f1c1d1e-0000-4000-8000-000000000001",
            "trackId": "song", "venueId": venue, "name": "Mine",
        }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Folders, filed with that song.
    let folder = run(
        &services,
        "create_folder",
        json!({ "venueId": venue, "name": "Openers" }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    run(
        &services,
        "rename_folder",
        json!({ "folderId": folder, "name": "Closers" }),
    )
    .await;
    run(
        &services,
        "set_folder_track",
        json!({ "folderId": folder, "trackId": "song", "linked": true }),
    )
    .await;
    run(&services, "delete_folder", json!({ "folderId": folder })).await;

    // The score: rename it, write clips into it, delete it.
    run(
        &services,
        "rename_score",
        json!({ "scoreId": score, "name": "Still mine" }),
    )
    .await;
    let wash = luma_patterns::presets()
        .clip("Wash")
        .expect("the Wash preset")
        .clip(0., 4.);
    let candidate = luma_patterns::Score {
        clips: [("wash".to_owned(), wash)].into_iter().collect(),
    };
    run(
        &services,
        "edit_score_document",
        json!({
            "scoreId": score,
            "edit": luma_patterns::ScoreEdit::between(&luma_patterns::Score::default(), &candidate),
        }),
    )
    .await;
    run(
        &services,
        "duplicate_score",
        json!({ "requestId": uuid::Uuid::new_v4().to_string(), "scoreId": score, "name": "Still mine copy" }),
    )
    .await;
    run(&services, "delete_score", json!({ "id": score })).await;

    // The Perform set.
    run(
        &services,
        "perform_add_busk_page",
        json!({ "venueId": venue }),
    )
    .await;
    let layout = run(&services, "get_perform_layout", json!({ "venueId": venue })).await;
    run(
        &services,
        "save_perform_layout",
        json!({ "venueId": venue, "layout": layout }),
    )
    .await;
}

#[tokio::test]
async fn only_the_owner_holds_the_venue_s_door() {
    let directory = tempfile::tempdir().unwrap();
    let (services, venue, _stage) = shared(directory.path()).await;
    let code = |services: &AppServices| {
        let pool = services.db.0.clone();
        let venue = venue.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>("SELECT share_code FROM venues WHERE id = ?")
                .bind(venue)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let issued = code(&services).await;
    // What the venue tells its reader agrees with what the guards do: the
    // member may edit it and not manage it, on every read of it.
    let capability = |venue: &Value| venue["capability"].clone();
    let listed = run(&services, "list_venues", json!({})).await;
    let listed = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == venue.as_str())
        .expect("the shared venue is on the member's list")
        .clone();
    let read = run(&services, "get_venue", json!({ "id": venue })).await;
    for seen in [&listed, &read] {
        assert_eq!(capability(seen), json!({ "edit": true, "manage": false }));
    }

    unauthorized(
        dispatch(&services, "rotate_share_code", &json!({ "venueId": venue })).await,
        "rotate the share code",
    );
    unauthorized(
        dispatch(&services, "revoke_share_code", &json!({ "venueId": venue })).await,
        "revoke the share code",
    );
    unauthorized(
        dispatch(
            &services,
            "list_venue_members",
            &json!({ "venueId": venue }),
        )
        .await,
        "list the venue's members",
    );
    unauthorized(
        dispatch(
            &services,
            "remove_venue_member",
            &json!({ "venueId": venue, "uid": MEMBER }),
        )
        .await,
        "remove a member",
    );
    assert_eq!(code(&services).await, issued);

    // Back on the owner's side, every one of them works.
    arm_write_admission(&services.db.0, Some(TEST_OWNER))
        .await
        .unwrap();
    let read = run(&services, "get_venue", json!({ "id": venue })).await;
    assert_eq!(capability(&read), json!({ "edit": true, "manage": true }));
    let members = run(&services, "list_venue_members", json!({ "venueId": venue })).await;
    assert_eq!(members.as_array().unwrap().len(), 2);
    run(
        &services,
        "remove_venue_member",
        json!({ "venueId": venue, "uid": MEMBER }),
    )
    .await;
    run(&services, "revoke_share_code", json!({ "venueId": venue })).await;
    assert_eq!(code(&services).await, None);
}
