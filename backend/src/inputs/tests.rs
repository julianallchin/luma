//! Bindings learned, kept, and applied in each mode.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::{Address, Event};
use crate::database::local::{auth, database, state};
use crate::dispatch::{dispatch, AppServices};
use luma_patterns::ControlSource as _;

const OWNER: &str = "11111111-2222-3333-4444-555555555555";

async fn library() -> (tempfile::TempDir, AppServices, String) {
    let directory = tempfile::tempdir().unwrap();
    let db = database::init_app_db_at(directory.path()).await.unwrap();
    let state_db = state::init_state_db_at(directory.path()).await.unwrap();
    auth::install_test_session(&state_db.0, OWNER).await;
    auth::bootstrap_headless_admission(&db.0, &state_db.0)
        .await
        .unwrap();
    let storage = crate::storage::StorageRoot::from_path(directory.path().to_path_buf());
    let workspaces = Arc::new(
        crate::agent_execution::workspace::PythonWorkspaceService::new(
            storage.agent_workspaces_dir(),
            Arc::new(|| Err("no Python here".to_string())),
        ),
    );
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let services = AppServices::headless(db, state_db, storage, repo, workspaces);
    let venue = dispatch(
        &services,
        "create_venue",
        &json!({ "name": "Room", "description": null }),
    )
    .await
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // A headless hub opens no device: it applies only what a test hands it.
    services.inputs.open(&venue).await.unwrap();
    (directory, services, venue)
}

fn cc(number: u8, value: f64) -> Event {
    Event {
        address: Address::MidiCc { channel: 0, number },
        value,
    }
}

/// Learn `control` from the input `event`, as a person moving a fader.
async fn learn(services: &AppServices, venue: &str, control: &str, mode: &str, event: Event) {
    let learning = {
        let services = services.inputs.clone();
        let deadline = Instant::now() + Duration::from_secs(10);
        tokio::spawn(async move {
            while services.state().learning.is_none() {
                assert!(Instant::now() < deadline, "learning never started");
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            services.receive(event);
        })
    };
    dispatch(
        services,
        "input_learn",
        &json!({ "venueId": venue, "control": control, "mode": mode }),
    )
    .await
    .unwrap();
    learning.await.unwrap();
}

fn value(venue: &str, control: &str) -> f64 {
    crate::controls::venue(venue).value(control)
}

#[tokio::test]
async fn a_learned_fader_follows_its_input_and_is_kept() {
    let (_dir, services, venue) = library().await;
    learn(&services, &venue, "drop", "fader", cc(7, 0.9)).await;
    assert_eq!(value(&venue, "drop"), 0.0, "the learning move sets nothing");

    services.inputs.receive(cc(7, 0.4));
    assert!((value(&venue, "drop") - 0.4).abs() < 1e-9);
    services.inputs.receive(cc(8, 1.0));
    assert!(
        (value(&venue, "drop") - 0.4).abs() < 1e-9,
        "another CC is not bound"
    );

    // Kept across a reopen.
    services.inputs.close().await;
    services.inputs.open(&venue).await.unwrap();
    let state = services.inputs.state();
    assert_eq!(state.bindings.len(), 1);
    assert_eq!(state.bindings[0].control, "drop");
}

#[tokio::test]
async fn pads_are_momentary_or_toggle_and_any_input_can_invert() {
    let (_dir, services, venue) = library().await;
    learn(&services, &venue, "flash", "momentary", cc(20, 1.0)).await;
    learn(&services, &venue, "strobe", "toggle", cc(21, 1.0)).await;

    services.inputs.receive(cc(20, 0.8));
    assert_eq!(value(&venue, "flash"), 1.0);
    services.inputs.receive(cc(20, 0.0));
    assert_eq!(value(&venue, "flash"), 0.0);

    for expected in [1.0, 0.0] {
        services.inputs.receive(cc(21, 1.0));
        services.inputs.receive(cc(21, 0.0));
        assert_eq!(value(&venue, "strobe"), expected, "each press flips it");
    }

    let flash = services.inputs.state().bindings[0].id.clone();
    dispatch(
        &services,
        "input_update_binding",
        &json!({ "venueId": venue, "bindingId": flash, "mode": "momentary", "invert": true }),
    )
    .await
    .unwrap();
    services.inputs.receive(cc(20, 0.0));
    assert_eq!(value(&venue, "flash"), 1.0, "inverted, released is on");

    dispatch(
        &services,
        "input_unbind",
        &json!({ "venueId": venue, "bindingId": flash }),
    )
    .await
    .unwrap();
    services.inputs.receive(cc(20, 1.0));
    assert_eq!(
        value(&venue, "flash"),
        1.0,
        "an unbound input moves nothing"
    );
}

#[tokio::test]
async fn osc_from_any_device_binds_like_midi() {
    let (_dir, services, venue) = library().await;
    let glove = |value| Event {
        address: Address::Osc {
            address: "/glove/tilt".into(),
            arg: 0,
        },
        value,
    };
    learn(&services, &venue, "tilt", "fader", glove(0.2)).await;
    services.inputs.receive(glove(0.75));
    assert!((value(&venue, "tilt") - 0.75).abs() < 1e-9);
}
