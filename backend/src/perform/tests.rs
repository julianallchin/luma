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
async fn an_unknown_track_is_unmatched_until_pinned() {
    let (_dir, services, venue) = library().await;
    let rkbx = Rkbx::start(&services, &venue).await;
    rkbx.load(1, "Not In The Library", "Nobody", 90.0);
    until(&services, |state| {
        state
            .decks
            .iter()
            .any(|d| d.id == 2 && d.track == DeckMatch::Unmatched)
    })
    .await;

    services.perform.assign(2, Some("track".into())).unwrap();
    let state = until(&services, |state| matched(state, 2).is_some()).await;
    assert!(matches!(
        matched(&state, 2),
        Some(DeckMatch::Matched { manual: true, .. })
    ));
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
