//! Bindings learned, kept, and applied for each kind of hardware.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;

use super::{Address, Event, Mode, Profile};
use crate::dispatch::{dispatch, AppServices};
use crate::sync::triggers::Attribution;

async fn library() -> (tempfile::TempDir, AppServices, String) {
    let directory = tempfile::tempdir().unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let services = crate::test_support::signed_in(directory.path(), repo).await;
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
        port: None,
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

fn value(services: &AppServices, venue: &str, control: &str) -> f64 {
    services.controls.venue(venue).value(control)
}

#[tokio::test]
async fn a_learned_fader_follows_its_input_and_is_kept() {
    let (_dir, services, venue) = library().await;
    learn(&services, &venue, "drop", "fader", cc(7, 0.9)).await;
    assert_eq!(
        value(&services, &venue, "drop"),
        0.0,
        "the learning move sets nothing"
    );

    services.inputs.receive(cc(7, 0.4));
    assert!((value(&services, &venue, "drop") - 0.4).abs() < 1e-9);
    services.inputs.receive(cc(8, 1.0));
    assert!(
        (value(&services, &venue, "drop") - 0.4).abs() < 1e-9,
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
async fn learning_an_input_again_changes_its_binding() {
    let (_dir, services, venue) = library().await;
    learn(&services, &venue, "drop", "fader", cc(20, 0.5)).await;
    let first = services.inputs.state().bindings[0].id.clone();
    learn(&services, &venue, "drop", "momentary", cc(20, 1.0)).await;
    let bindings = services.inputs.state().bindings;
    assert_eq!(bindings.len(), 1, "{bindings:?}");
    assert_eq!(
        (bindings[0].id.as_str(), bindings[0].mode),
        (first.as_str(), Mode::Momentary)
    );

    // Another control may share the input: one pad can drive two things.
    learn(&services, &venue, "rise", "fader", cc(20, 0.5)).await;
    assert_eq!(services.inputs.state().bindings.len(), 2);
}

#[tokio::test]
async fn pads_press_latching_keys_set_and_any_input_can_invert() {
    let (_dir, services, venue) = library().await;
    learn(&services, &venue, "flash", "momentary", cc(20, 1.0)).await;
    learn(&services, &venue, "strobe", "toggle", cc(21, 1.0)).await;

    services.inputs.receive(cc(20, 0.8));
    assert_eq!(value(&services, &venue, "flash"), 1.0);
    services.inputs.receive(cc(20, 0.0));
    assert_eq!(value(&services, &venue, "flash"), 0.0);

    // A latching key sends its state: on, then off at the next press.
    services.inputs.receive(cc(21, 1.0));
    assert_eq!(value(&services, &venue, "strobe"), 1.0);
    services.inputs.receive(cc(21, 0.0));
    assert_eq!(value(&services, &venue, "strobe"), 0.0);

    // By its control: the two can be learned in one millisecond, and bindings
    // made in one millisecond have no oldest.
    let flash = services
        .inputs
        .state()
        .bindings
        .into_iter()
        .find(|binding| binding.control == "flash")
        .expect("flash is bound")
        .id;
    dispatch(
        &services,
        "input_update_binding",
        &json!({ "venueId": venue, "bindingId": flash, "mode": "momentary", "invert": true }),
    )
    .await
    .unwrap();
    services.inputs.receive(cc(20, 0.0));
    assert_eq!(
        value(&services, &venue, "flash"),
        1.0,
        "inverted, released is on"
    );

    dispatch(
        &services,
        "input_unbind",
        &json!({ "venueId": venue, "bindingId": flash }),
    )
    .await
    .unwrap();
    services.inputs.receive(cc(20, 1.0));
    assert_eq!(
        value(&services, &venue, "flash"),
        1.0,
        "an unbound input moves nothing"
    );
}

#[tokio::test]
async fn a_pad_plays_a_button_as_the_button_says() {
    use crate::controls::perform;
    use crate::models::perform::{PerformLayout, PerformPattern};

    let (_dir, services, venue) = library().await;
    // A pattern whose pad is a toggle: a pad on a controller flips it on
    // the press, as the one on screen does.
    let pattern = PerformPattern {
        id: "wash".into(),
        venue_id: venue.clone(),
        name: "Wash".into(),
        selection: luma_patterns::Selection::All,
        z_index: 0,
        blend_mode: luma_patterns::BlendMode::Replace,
        seed: 1,
        graph: crate::controls::tests::with_pad_at_once(
            luma_patterns::presets()
                .clip("Wash")
                .expect("the shipped Wash")
                .graph
                .clone(),
        ),
    };
    let pad = perform::pad(&pattern).expect("a pad");
    services.controls.venue(&venue).install(
        &[pattern],
        &PerformLayout::default(),
        Default::default(),
    );
    learn(&services, &venue, &pad, "momentary", cc(30, 1.0)).await;
    learn(&services, &venue, &pad, "toggle", cc(31, 1.0)).await;

    for expected in [1.0, 0.0] {
        services.inputs.receive(cc(30, 1.0));
        services.inputs.receive(cc(30, 0.0));
        assert_eq!(
            value(&services, &venue, &pad),
            expected,
            "each press flips it"
        );
    }
    // A soft hit is a press: a pad's note carries how hard, not whether.
    let soft = 20.0 / 127.0;
    services.inputs.receive(cc(30, soft));
    services.inputs.receive(cc(30, 0.0));
    assert_eq!(
        value(&services, &venue, &pad),
        1.0,
        "a soft hit flips it too"
    );
    services.inputs.receive(cc(30, soft));
    services.inputs.receive(cc(30, 0.0));
    assert_eq!(value(&services, &venue, &pad), 0.0);
    services.inputs.receive(cc(31, 1.0));
    assert_eq!(
        value(&services, &venue, &pad),
        1.0,
        "a latching key sets its state"
    );
    services.inputs.receive(cc(31, 1.0));
    assert_eq!(
        value(&services, &venue, &pad),
        1.0,
        "and sets it again, not flips it"
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
        port: None,
    };
    learn(&services, &venue, "tilt", "fader", glove(0.2)).await;
    services.inputs.receive(glove(0.75));
    assert!((value(&services, &venue, "tilt") - 0.75).abs() < 1e-9);
}

/// A learn whose caller stopped waiting is over, so the next input moves
/// what it is bound to instead of vanishing into a learn nobody hears.
#[tokio::test]
async fn a_learn_nobody_waits_for_ends() {
    let (_dir, services, _venue) = library().await;
    let waiting = services
        .inputs
        .learn("drop", super::Mode::Fader, None, Attribution::PERSON);
    let stopped = tokio::time::timeout(Duration::from_millis(50), waiting).await;
    assert!(stopped.is_err(), "nothing was heard");
    assert_eq!(services.inputs.state().learning, None);
}

/// `event` as it came on MIDI port `port`.
fn on(port: &str, event: Event) -> Event {
    Event {
        port: Some(port.into()),
        ..event
    }
}

#[tokio::test]
async fn a_controller_is_named_after_its_port_and_hears_only_it() {
    let (_dir, services, venue) = library().await;
    learn(
        &services,
        &venue,
        "drop",
        "fader",
        on("Pads MIDI 1", cc(7, 0.9)),
    )
    .await;
    learn(
        &services,
        &venue,
        "rise",
        "fader",
        on("Pads MIDI 1", cc(8, 0.9)),
    )
    .await;
    let state = services.inputs.state();
    assert_eq!(state.controllers.len(), 1, "both on one controller");
    let pads = &state.controllers[0];
    assert_eq!(
        (pads.name.as_str(), pads.port.as_str()),
        ("Pads MIDI 1", "Pads MIDI 1")
    );
    assert!(!pads.connected, "a headless hub opens no port");
    assert!(state
        .bindings
        .iter()
        .all(|binding| binding.controller_id == pads.id));

    services.inputs.receive(on("Other", cc(7, 0.4)));
    assert_eq!(value(&services, &venue, "drop"), 0.0, "another port's CC 7");
    services.inputs.receive(on("Pads MIDI 1", cc(7, 0.4)));
    assert!((value(&services, &venue, "drop") - 0.4).abs() < 1e-9);
}

#[tokio::test]
async fn a_page_maps_onto_a_controllers_grid_in_place_of_what_it_had() {
    use crate::models::perform::{Cell, CellContent, LayoutPage};
    let (_dir, services, venue) = library().await;
    let pads = services
        .inputs
        .add_controller("Pads", "", Attribution::PERSON)
        .await
        .unwrap();
    let profile = Profile::parse(
        r#"{"controls": [
            {"address": {"kind": "midi_note", "channel": 0, "number": 0}, "column": 0, "row": 0},
            {"address": {"kind": "midi_note", "channel": 0, "number": 1}, "column": 1, "row": 0}]}"#,
    )
    .unwrap();
    services
        .inputs
        .change_controller(&pads.id, Attribution::PERSON, |controller| {
            controller.profile = profile;
            Ok(())
        })
        .await
        .unwrap();
    let page = |cells| LayoutPage {
        id: "page".into(),
        name: "Main".into(),
        columns: 2,
        rows: 1,
        one_at_a_time: Vec::new(),
        cells,
    };
    let at = |id: &str, column, content| Cell {
        id: id.into(),
        column,
        row: 0,
        width: 1,
        height: 1,
        content,
    };
    let first = page(vec![
        at("tap", 0, CellContent::Tap),
        at("clear", 1, CellContent::Clear),
    ]);
    let bound = services
        .inputs
        .map_page(&pads.id, &first, &[], Attribution::PERSON)
        .await
        .unwrap();
    assert_eq!(bound.len(), 2);
    // Another page on the same pads replaces the first's bindings.
    let second = page(vec![at("select", 0, CellContent::Select)]);
    services
        .inputs
        .map_page(&pads.id, &second, &[], Attribution::PERSON)
        .await
        .unwrap();
    let controls: Vec<String> = services
        .inputs
        .state()
        .bindings
        .into_iter()
        .map(|binding| binding.control)
        .collect();
    assert_eq!(controls, vec![crate::controls::perform::SELECT.to_owned()]);
    services.inputs.receive(cc(99, 1.0));
    let note = |number| Event {
        address: Address::MidiNote { channel: 0, number },
        value: 1.0,
        port: None,
    };
    services.inputs.receive(note(0));
    assert!(
        services.controls.venue(&venue).playing().selecting,
        "the pad presses Select"
    );

    // A controller with no profile cannot be mapped, and says why.
    let bare = services
        .inputs
        .add_controller("Bare", "", Attribution::PERSON)
        .await
        .unwrap();
    let refused = services
        .inputs
        .map_page(&bare.id, &first, &[], Attribution::PERSON)
        .await
        .unwrap_err();
    assert!(refused.contains("profile"), "{refused}");
    // Removing a controller takes its bindings.
    services
        .inputs
        .remove_controller(&pads.id, Attribution::PERSON)
        .await
        .unwrap();
    assert!(services.inputs.state().bindings.is_empty());
}

#[tokio::test]
async fn bindings_kept_here_move_into_one_controller() {
    let (_dir, services, venue) = library().await;
    sqlx::query(
        "INSERT INTO control_bindings_unsynced (id, venue_id, control, address_json, mode, invert)
         VALUES ('old-1', ?, 'drop', '{\"kind\":\"midi_cc\",\"channel\":0,\"number\":7}', 'fader', 1),
                ('old-2', ?, 'rise', '{\"kind\":\"midi_cc\",\"channel\":0,\"number\":8}', 'toggle', 0)",
    )
    .bind(&venue)
    .bind(&venue)
    .execute(&services.db.0)
    .await
    .unwrap();
    services.inputs.open(&venue).await.unwrap();
    let state = services.inputs.state();
    assert_eq!(state.controllers.len(), 1);
    assert_eq!(state.controllers[0].name, "Controller");
    let mut kept: Vec<(String, Mode, bool)> = state
        .bindings
        .iter()
        .map(|binding| (binding.control.clone(), binding.mode, binding.invert))
        .collect();
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        kept,
        vec![
            ("drop".to_owned(), Mode::Fader, true),
            ("rise".to_owned(), Mode::Toggle, false)
        ]
    );
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_bindings_unsynced")
        .fetch_one(&services.db.0)
        .await
        .unwrap();
    assert_eq!(left, 0, "moved, not copied");
    // Opened again, nothing moves twice.
    services.inputs.open(&venue).await.unwrap();
    assert_eq!(services.inputs.state().controllers.len(), 1);
    services.inputs.receive(cc(7, 0.25));
    assert!(
        (value(&services, &venue, "drop") - 0.75).abs() < 1e-9,
        "still inverted"
    );
}

/// A library from before controllers synced: its bindings survive the
/// migration, and move into a controller as an ordinary write, which the
/// upload queue carries like any other. A migration's own writes never
/// upload, which is why the move is not the migration's.
#[tokio::test]
async fn bindings_from_before_controllers_upload_as_they_move() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(directory.path().join("old.db"))
                .create_if_missing(true),
        )
        .await
        .unwrap();
    let migrator = sqlx::migrate!("./migrations");
    migrator.run_to(20261106120000, &pool).await.unwrap();
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', 'user-1', 'Room')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO control_bindings (id, venue_id, control, address_json, mode, invert)
         VALUES ('old', 'venue', 'drop', '{\"kind\":\"midi_cc\",\"channel\":0,\"number\":7}', 'fader', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    migrator.run(&pool).await.unwrap();
    crate::database::local::auth::arm_write_admission(&pool, Some("user-1"))
        .await
        .unwrap();
    {
        // The one connection, with the upload queue on it, as the app's
        // writers have.
        let mut connection = pool.acquire().await.unwrap();
        sqlx::raw_sql(
            "CREATE TEMP TABLE powersync_crud (seq INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT, \
             id TEXT, type TEXT, data TEXT, metadata TEXT);",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        crate::sync::triggers::install(&mut connection)
            .await
            .unwrap();
    }
    let queued = |table: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT op FROM powersync_crud WHERE type = ?")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap()
        }
    };
    assert!(
        queued("control_bindings").await.is_empty(),
        "the migration uploads nothing"
    );

    super::store::adopt(&pool, "venue").await.unwrap();
    let (controllers, bindings) = super::store::read(&pool, "venue").await.unwrap();
    assert_eq!(controllers.len(), 1);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].controller_id, controllers[0].id);
    assert_eq!(queued("perform_controllers").await, vec!["PUT"]);
    assert_eq!(queued("control_bindings").await, vec!["PUT"]);
}

#[tokio::test]
async fn controllers_synced_from_another_device_reach_the_hub() {
    let (_dir, services, venue) = library().await;
    // Written under the hub, as a download writes it.
    super::store::add_controller(
        &services.db.0,
        &venue,
        Attribution::PERSON,
        "Pads",
        "Pads MIDI 1",
    )
    .await
    .unwrap();
    assert!(services.inputs.state().controllers.is_empty());
    super::synced();
    let deadline = Instant::now() + Duration::from_secs(10);
    while services.inputs.state().controllers.is_empty() {
        assert!(
            Instant::now() < deadline,
            "the hub never read the synced controller"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(services.inputs.controllers()[0].name, "Pads");
}

#[tokio::test]
async fn a_controller_is_renamed_and_never_to_nothing() {
    let (_dir, services, venue) = library().await;
    let id = services
        .inputs
        .add_controller("Pads", "", Attribution::PERSON)
        .await
        .unwrap()
        .id;
    let rename = |name: &str| json!({ "venueId": venue, "controllerId": id, "name": name });
    dispatch(&services, "input_rename_controller", &rename("  Grid "))
        .await
        .unwrap();
    assert_eq!(services.inputs.state().controllers[0].name, "Grid");
    assert!(dispatch(&services, "input_rename_controller", &rename(" "))
        .await
        .is_err());
    assert_eq!(services.inputs.state().controllers[0].name, "Grid");
}
