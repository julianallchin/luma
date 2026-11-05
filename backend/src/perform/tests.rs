//! The session end to end, fed by rkbx_link packets over real UDP.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rosc::{OscMessage, OscPacket, OscType};
use serde_json::json;

use crate::database::local::{auth, database, state};
use crate::dispatch::{dispatch, AppServices};
use crate::models::perform::{DeckMatch, PerformSource, PerformState};

const OWNER: &str = "11111111-2222-3333-4444-555555555555";

/// A signed-in library with one venue and one track, "Strobe" by deadmau5.
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
    let uid: Option<String> = sqlx::query_scalar("SELECT uid FROM venues WHERE id = ?")
        .bind(&venue)
        .fetch_one(&services.db.0)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, file_path, title, artist)
         VALUES ('track', ?, 'h', '/t.wav', 'Strobe', 'deadmau5')",
    )
    .bind(&uid)
    .execute(&services.db.0)
    .await
    .unwrap();
    (directory, services, venue)
}

/// A fake rkbx_link, sending to a session listening on an OS-picked port.
struct Rkbx {
    socket: UdpSocket,
    to: String,
}

impl Rkbx {
    async fn start(services: &AppServices, venue: &str) -> Self {
        let source = PerformSource::RkbxLink {
            address: Some("127.0.0.1:0".into()),
        };
        services.perform.start(venue.into(), source).await.unwrap();
        let state = services.perform.state();
        let to = state
            .detail
            .strip_prefix("Listening on ")
            .expect("an rkbx_link session says where it listens")
            .to_string();
        Self {
            socket: UdpSocket::bind("127.0.0.1:0").unwrap(),
            to,
        }
    }

    fn send(&self, addr: &str, arg: OscType) {
        let packet = OscPacket::Message(OscMessage {
            addr: addr.into(),
            args: vec![arg],
        });
        let bytes = rosc::encoder::encode(&packet).unwrap();
        self.socket.send_to(&bytes, &self.to).unwrap();
    }

    fn load(&self, deck: u8, title: &str, artist: &str, bpm: f32) {
        self.send(
            &format!("/{deck}/track/title"),
            OscType::String(title.into()),
        );
        self.send(
            &format!("/{deck}/track/artist"),
            OscType::String(artist.into()),
        );
        self.send(&format!("/{deck}/bpm/original"), OscType::Float(bpm));
        self.send(&format!("/{deck}/bpm/current"), OscType::Float(bpm));
    }
}

/// Wait until `ready` holds for the session's state, and return that state.
async fn until(services: &AppServices, ready: impl Fn(&PerformState) -> bool) -> PerformState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = services.perform.state();
        if ready(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "never became ready: {state:#?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn matched(state: &PerformState, deck: u8) -> Option<&DeckMatch> {
    state
        .decks
        .iter()
        .find(|d| d.id == deck)
        .map(|d| &d.track)
        .filter(|track| matches!(track, DeckMatch::Matched { .. }))
}

#[tokio::test]
async fn a_rekordbox_deck_is_matched_without_a_duration_and_followed() {
    let (_dir, services, venue) = library().await;
    let rkbx = Rkbx::start(&services, &venue).await;
    rkbx.load(0, "Strobe", "deadmau5", 128.0);

    let state = until(&services, |state| matched(state, 1).is_some()).await;
    let Some(DeckMatch::Matched {
        track_id, manual, ..
    }) = matched(&state, 1)
    else {
        unreachable!()
    };
    assert_eq!((track_id.as_str(), *manual), ("track", false));
    assert!(state.problem.as_deref().unwrap().contains("osc.msg.n/time"));

    // Two moves close together: the deck is playing, and runs on between them.
    rkbx.send("/0/time", OscType::Float(30.0));
    rkbx.send("/0/time", OscType::Float(30.01));
    let state = until(&services, |state| state.decks[0].playing).await;
    assert!(state.problem.is_none());
    let earlier = state.decks[0].position;
    let later = until(&services, |state| state.decks[0].position > earlier).await;
    assert!(later.decks[0].position > 30.0);

    // rkbx_link goes quiet when the deck stops.
    until(&services, |state| !state.decks[0].playing).await;
}

#[tokio::test]
async fn stopping_forgets_the_source_and_its_decks() {
    let (_dir, services, venue) = library().await;
    let rkbx = Rkbx::start(&services, &venue).await;
    rkbx.load(0, "Strobe", "deadmau5", 128.0);
    until(&services, |state| !state.decks.is_empty()).await;

    services.perform.stop().await;
    let state = services.perform.state();
    assert!(state.source.is_none() && state.decks.is_empty());
}

#[tokio::test]
async fn the_virtual_deck_follows_the_player() {
    use crate::host_audio::HostAudioControl;

    let (_dir, services, venue) = library().await;
    services
        .perform
        .start(venue.clone(), PerformSource::Virtual)
        .await
        .unwrap();
    assert!(services.perform.state().decks.is_empty(), "nothing loaded");

    // Ten seconds of silence, as the library's "Strobe".
    let host = services.host_audio();
    assert!(host.begin_load(1, "track".into()));
    host.finish_load(1, vec![0.0; 48_000 * 2 * 10], 0.0)
        .unwrap();
    host.control(1, HostAudioControl::Play { seconds: 2.0 })
        .unwrap();

    let state = until(&services, |state| matched(state, 1).is_some()).await;
    assert!(matches!(
        matched(&state, 1),
        Some(DeckMatch::Matched { manual: true, .. })
    ));
    let state = until(&services, |state| state.decks[0].playing).await;
    assert_eq!(state.decks[0].title, "Strobe");
    let earlier = state.decks[0].position;
    assert!(earlier >= 2.0, "the player's playhead: {earlier}");
    until(&services, |state| state.decks[0].position > earlier).await;

    host.unload();
    until(&services, |state| state.decks.is_empty()).await;
}

fn wash(id: &str) -> crate::models::perform::PerformPattern {
    let wash = luma_patterns::presets()
        .clip("Wash")
        .expect("the shipped Wash");
    crate::models::perform::PerformPattern {
        id: id.into(),
        venue_id: String::new(),
        name: "Wash".into(),
        selection: serde_json::from_value(json!({ "expression": "all" })).unwrap(),
        z_index: 0,
        blend_mode: wash.blend_mode,
        seed: u64::MAX,
        graph: wash.graph.clone().with_pad(),
    }
}

#[tokio::test]
async fn the_perform_set_saves_lists_changes_and_deletes() {
    use crate::models::perform::{Cell, CellContent, ControlTarget, PerformLayout, PerformPattern};

    let (_dir, services, venue) = library().await;
    let pattern = wash(&uuid::Uuid::new_v4().to_string());
    let saved: PerformPattern =
        serde_json::from_value(save(&services, &venue, &pattern).await.unwrap()).unwrap();
    assert_eq!(saved.venue_id, venue, "stored in the venue it was saved to");
    assert_eq!(saved.seed, u64::MAX, "a full u64 seed survives");

    let mut changed = PerformPattern {
        name: "Front wash".into(),
        ..saved.clone()
    };
    let pad = changed.graph.pad().expect("a pad").node;
    changed
        .graph
        .nodes
        .get_mut(&pad)
        .unwrap()
        .settings
        .insert("mode".into(), "flash".into());
    save(&services, &venue, &changed).await.unwrap();
    let listed: Vec<PerformPattern> = serde_json::from_value(
        dispatch(
            &services,
            "list_perform_patterns",
            &json!({ "venueId": venue }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        listed,
        vec![changed.clone()],
        "saving the same id changes it, its graph's pad among it"
    );

    let nameless = PerformPattern {
        name: String::new(),
        ..pattern.clone()
    };
    assert!(
        save(&services, &venue, &nameless).await.is_err(),
        "a pattern the score checker would refuse is refused here too"
    );

    let mut layout = PerformLayout::generated(&listed);
    let page = &mut layout.pages[0];
    let (column, row) = page.free_spot(1, 1).expect("room for a fader");
    page.cells.push(Cell {
        id: "speeds".into(),
        column,
        row,
        width: 1,
        height: 1,
        content: CellContent::Fader {
            label: "Speeds".into(),
            targets: vec![
                ControlTarget::Level {
                    pattern_id: saved.id.clone(),
                },
                ControlTarget::Named {
                    name: "drop".into(),
                },
            ],
        },
    });
    dispatch(
        &services,
        "save_perform_layout",
        &json!({ "venueId": venue, "layout": layout }),
    )
    .await
    .unwrap();
    assert_eq!(read_layout(&services, &venue).await, Some(layout.clone()));

    dispatch(
        &services,
        "delete_perform_pattern",
        &json!({ "venueId": venue, "patternId": saved.id }),
    )
    .await
    .unwrap();
    let listed = dispatch(
        &services,
        "list_perform_patterns",
        &json!({ "venueId": venue }),
    )
    .await
    .unwrap();
    assert_eq!(listed, json!([]));
    let pruned = read_layout(&services, &venue).await.unwrap();
    assert!(
        pruned.cells_of(&saved.id).next().is_none(),
        "the deleted pattern's cell went with it"
    );
    let left: Vec<&CellContent> = pruned.pages[0]
        .cells
        .iter()
        .map(|cell| &cell.content)
        .collect();
    assert_eq!(
        left,
        vec![
            &CellContent::Tap,
            &CellContent::Master,
            &CellContent::Fader {
                label: "Speeds".into(),
                targets: vec![ControlTarget::Named {
                    name: "drop".into()
                }],
            },
        ],
        "and so did the fader's hold on its level"
    );
}

#[tokio::test]
async fn an_open_set_plays_its_pads_and_faders() {
    use crate::controls::perform;
    use crate::models::perform::PerformLayout;

    let (_dir, services, venue) = library().await;
    let pattern = wash("wash");
    save(&services, &venue, &pattern).await.unwrap();
    let layout = PerformLayout::generated(std::slice::from_ref(&pattern));
    dispatch(
        &services,
        "save_perform_layout",
        &json!({ "venueId": venue, "layout": layout }),
    )
    .await
    .unwrap();
    dispatch(&services, "perform_open_set", &json!({ "venueId": venue }))
        .await
        .unwrap();

    let pad = perform::pad(&pattern).expect("a pad");
    dispatch(
        &services,
        "perform_press",
        &json!({ "name": pad, "down": true }),
    )
    .await
    .unwrap();
    dispatch(
        &services,
        "perform_press",
        &json!({ "name": pad, "down": false }),
    )
    .await
    .unwrap();
    let state = services.perform.state();
    assert!(state.buttons[&pad].on(), "a toggle pad stays on");
    assert_eq!(state.buttons[&pad].value(120.0), 1.0);
    assert_eq!(state.controls[&perform::level("wash")], 1.0);
    assert_eq!(state.controls[perform::MASTER], 1.0);
}

#[tokio::test]
async fn a_layout_cell_must_fit_its_page() {
    use crate::models::perform::PerformLayout;

    let (_dir, services, venue) = library().await;
    let mut layout = PerformLayout::generated(&[]);
    layout.pages[0].cells[0].column = layout.pages[0].columns;
    let refused = dispatch(
        &services,
        "save_perform_layout",
        &json!({ "venueId": venue, "layout": layout }),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.kind(), "invalid");

    let mut layout = PerformLayout::generated(&[]);
    let (first, second) = (
        layout.pages[0].cells[0].clone(),
        &mut layout.pages[0].cells[1],
    );
    (second.column, second.row) = (first.column, first.row);
    let refused = dispatch(
        &services,
        "save_perform_layout",
        &json!({ "venueId": venue, "layout": layout }),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.kind(), "invalid", "two cells over each other");
}

async fn save(
    services: &AppServices,
    venue: &str,
    pattern: &crate::models::perform::PerformPattern,
) -> Result<serde_json::Value, crate::dispatch::CommandError> {
    let args = json!({ "venueId": venue, "pattern": pattern });
    dispatch(services, "save_perform_pattern", &args).await
}

async fn read_layout(
    services: &AppServices,
    venue: &str,
) -> Option<crate::models::perform::PerformLayout> {
    let args = json!({ "venueId": venue });
    serde_json::from_value(
        dispatch(services, "get_perform_layout", &args)
            .await
            .unwrap(),
    )
    .unwrap()
}
