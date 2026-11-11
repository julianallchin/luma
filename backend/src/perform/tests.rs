//! The session end to end, fed by rkbx_link packets over real UDP.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use rosc::{OscMessage, OscPacket, OscType};
use serde_json::json;

use crate::dispatch::{dispatch, AppServices};
use crate::models::perform::{DeckMatch, PerformSource, PerformState};

/// A signed-in library with one venue and one track, "Strobe" by deadmau5,
/// reading the bundled fixture definitions.
async fn library() -> (tempfile::TempDir, AppServices, String) {
    let directory = tempfile::tempdir().unwrap();
    let fixtures = crate::resources::source_fixtures_root().expect("a bundled fixture library");
    let services = crate::test_support::signed_in(directory.path(), fixtures).await;
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
        selection: luma_patterns::Selection::All,
        z_index: 0,
        blend_mode: wash.blend_mode,
        seed: u64::MAX,
        graph: crate::controls::tests::with_pad_at_once(wash.graph.clone()),
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
    assert!(state.bank.buttons[&pad].on(), "a toggle pad stays on");
    assert_eq!(state.bank.buttons[&pad].value(120.0), 1.0);
    assert_eq!(state.bank.controls[&perform::level("wash")], 1.0);
    assert_eq!(state.bank.controls[perform::MASTER], 1.0);
}

#[tokio::test]
async fn a_color_list_sets_live_colors_and_a_fader_moves_numbers() {
    use crate::controls::perform;
    use crate::models::perform::{
        Cell, CellContent, ControlTarget, ListValue, PerformLayout, PerformPattern,
    };

    let (_dir, services, venue) = library().await;
    let graph = luma_patterns::ClipGraph::from_json(
        &json!({"version": 3, "nodes": {
            "hue": {"kind": "control", "settings": {"name": "main", "kind": "color"}},
            "drop": {"kind": "control", "settings": {"name": "drop"}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "drop"}}},
            "color1": {"kind": "color", "inputs": {
                "color": {"node": "hue"}, "brightness": {"node": "curve1"}}}}})
        .to_string(),
    )
    .unwrap();
    let pattern = PerformPattern {
        graph: crate::controls::tests::with_pad_at_once(graph),
        ..wash("lit")
    };
    save(&services, &venue, &pattern).await.unwrap();
    let node = |node: &str| ControlTarget::Node {
        pattern_id: "lit".into(),
        node: node.into(),
    };
    let with = |content: CellContent| {
        let mut layout = PerformLayout::generated(std::slice::from_ref(&pattern));
        let page = &mut layout.pages[0];
        let (column, row) = page.free_spot(1, 1).unwrap();
        page.cells.push(Cell {
            id: "cell".into(),
            column,
            row,
            width: 1,
            height: 1,
            content,
        });
        layout
    };
    let colors = |target: ControlTarget| {
        with(CellContent::List {
            label: "Colors".into(),
            values: vec![
                ListValue::Color([1., 0., 0.]),
                ListValue::Color([0., 0., 1.]),
            ],
            fade: 0.,
            unit: luma_patterns::press::TimeUnit::Beats,
            targets: vec![target],
            step: None,
        })
    };
    let store = |layout: PerformLayout| {
        let services = &services;
        let args = json!({ "venueId": venue, "layout": layout });
        async move { dispatch(services, "save_perform_layout", &args).await }
    };
    let refused = store(colors(node("drop"))).await.unwrap_err();
    assert_eq!(refused.kind(), "invalid", "a color list sets no fader");
    let fader = with(CellContent::Fader {
        label: "Speeds".into(),
        targets: vec![node("hue")],
    });
    let refused = store(fader).await.unwrap_err();
    assert_eq!(refused.kind(), "invalid", "a fader moves no live color");
    let layout = colors(node("hue"));
    store(layout.clone()).await.unwrap();

    dispatch(&services, "perform_open_set", &json!({ "venueId": venue }))
        .await
        .unwrap();
    let blue = perform::value("cell", 1);
    for down in [true, false] {
        dispatch(
            &services,
            "perform_press",
            &json!({ "name": blue, "down": down }),
        )
        .await
        .unwrap();
    }
    let state = services.perform.state();
    assert_eq!(state.bank.lists["cell"], 1, "the value pressed is on");
    let color = state.bank.colors[&perform::node("lit", "hue")];
    assert_eq!(color.value(120.), [0., 0., 1.], "its target took its color");

    // The live color leaves the graph: the cell lets go of it, and the
    // pattern's change is not refused for it.
    let mut plain = pattern.clone();
    plain.graph.nodes.remove("hue");
    plain
        .graph
        .nodes
        .get_mut("color1")
        .unwrap()
        .inputs
        .remove("color");
    save(&services, &venue, &plain).await.unwrap();
    let tidied = read_layout(&services, &venue).await.unwrap();
    let cell = tidied.pages[0].cells.iter().find(|cell| cell.id == "cell");
    assert_eq!(cell.unwrap().content.targets(), &[] as &[ControlTarget]);
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
) -> Result<serde_json::Value, crate::error::CommandError> {
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

/// Two PARs a metre apart, `left` and `right`, each the one fixture of a
/// group of its name.
async fn two_groups(services: &AppServices, venue: &str) {
    use crate::database::local::groups as groups_db;
    use crate::database::local::venue_access::{VenueAccess, VenueResource, Write};

    let root = crate::venue_graph::root_id(venue);
    for (i, id) in ["left", "right"].iter().enumerate() {
        sqlx::query(
            "INSERT INTO fixtures (id, venue_id, universe, address, num_channels,
                manufacturer, model, mode_name, fixture_path, label)
             VALUES (?, ?, 1, ?, 7, 'Chauvet', 'SlimPAR 64', '7ch',
                'Chauvet/Chauvet-SlimPAR-64.qxf', ?)",
        )
        .bind(id)
        .bind(venue)
        .bind(1 + i as i64 * 8)
        .bind(id)
        .execute(&services.db.0)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venue_nodes
                 (id, venue_id, kind, catalog_ref, label, parent_id, my_socket, their_socket, roll)
             VALUES (?, ?, 'fixture', 'Chauvet/Chauvet-SlimPAR-64.qxf', ?, ?, 'clamp', 'rig', 0.0)",
        )
        .bind(id)
        .bind(venue)
        .bind(id)
        .bind(&root)
        .execute(&services.db.0)
        .await
        .unwrap();
        for (key, value) in [("u", i as f64), ("v", 0.0), ("trim", 2.0)] {
            sqlx::query(
                "INSERT INTO venue_node_params (node_id, venue_id, key, value) VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(venue)
            .bind(key)
            .bind(value)
            .execute(&services.db.0)
            .await
            .unwrap();
        }
        let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(venue))
            .await
            .unwrap();
        let group = groups_db::create_group(&mut access, Some(id), None, None, None)
            .await
            .unwrap();
        groups_db::add_member_to_group(&mut access, id, &group.id, groups_db::WHOLE_FIXTURE)
            .await
            .unwrap();
        access.commit().await.unwrap();
    }
}

/// The id of `venue`'s group called `name`.
async fn group_id(services: &AppServices, venue: &str, name: &str) -> String {
    sqlx::query_scalar("SELECT id FROM fixture_groups WHERE venue_id = ? AND name = ?")
        .bind(venue)
        .bind(name)
        .fetch_one(&services.db.0)
        .await
        .unwrap()
}

/// Open `venue`'s set of `pattern`, laid out with a group cell on each of
/// `left` and `right` and `extra`.
async fn open_on_groups(
    services: &AppServices,
    venue: &str,
    pattern: &crate::models::perform::PerformPattern,
    extra: Vec<crate::models::perform::CellContent>,
) {
    use crate::models::perform::{Cell, CellContent, PerformLayout};

    save(services, venue, pattern).await.unwrap();
    let mut layout = PerformLayout::generated(std::slice::from_ref(pattern));
    let page = &mut layout.pages[0];
    let mut groups = Vec::new();
    for name in ["left", "right"] {
        groups.push(CellContent::Group {
            label: String::new(),
            group: group_id(services, venue, name).await,
        });
    }
    for (n, content) in groups.into_iter().chain(extra).enumerate() {
        let (column, row) = page.free_spot(1, 1).expect("room");
        page.cells.push(Cell {
            id: format!("cell-{n}"),
            column,
            row,
            width: 1,
            height: 1,
            content,
        });
    }
    dispatch(
        services,
        "save_perform_layout",
        &json!({ "venueId": venue, "layout": layout }),
    )
    .await
    .unwrap();
    dispatch(services, "perform_open_set", &json!({ "venueId": venue }))
        .await
        .unwrap();
}

/// The brightest head of `fixture` now, and its color.
fn head(services: &AppServices, fixture: &str) -> (f32, [f32; 3]) {
    let frame = services.render_engine.sample(0.0).unwrap_or_default();
    frame
        .primitives
        .iter()
        .filter(|(id, _)| id.starts_with(&format!("{fixture}:")))
        .map(|(_, head)| (head.dimmer, head.color))
        .fold(
            (0.0, [1.0; 3]),
            |best, head| if head.0 >= best.0 { head } else { best },
        )
}

/// Tap control `name`: press and let go.
fn tap(services: &AppServices, name: &str) {
    services.perform.press(name, true).unwrap();
    services.perform.press(name, false).unwrap();
}

/// Wait until `ready` holds for the heads of left and right.
async fn lit_until(
    services: &AppServices,
    ready: impl Fn((f32, [f32; 3]), (f32, [f32; 3])) -> bool,
) -> ((f32, [f32; 3]), (f32, [f32; 3])) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let heads = (head(services, "left"), head(services, "right"));
        if ready(heads.0, heads.1) {
            return heads;
        }
        assert!(Instant::now() < deadline, "never lit so: {heads:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn an_effect_on_a_group_lights_only_its_heads_and_moves_when_it_does() {
    use crate::controls::perform;

    let (_dir, services, venue) = library().await;
    two_groups(&services, &venue).await;
    let pattern = wash("wash");
    open_on_groups(&services, &venue, &pattern, Vec::new()).await;
    let ((left, _), (right, _)) = lit_until(&services, |_, _| true).await;
    assert_eq!((left, right), (0.0, 0.0), "nothing pressed, nothing lit");

    // Left chosen, the pad tapped: the effect is compiled on left, off
    // the frame path, and lights left alone.
    let (on_left, on_right) = (perform::cell("cell-0"), perform::cell("cell-1"));
    let pad = perform::pad(&pattern).expect("a pad");
    tap(&services, &on_left);
    tap(&services, &pad);
    let ((_, _), (right, _)) = lit_until(&services, |left, _| left.0 > 0.0).await;
    assert_eq!(right, 0.0, "nothing outside left");
    assert_eq!(
        services.perform.state().bank.effects["wash"].groups,
        [group_id(&services, &venue, "left").await]
    );

    // Right chosen instead, the pad tapped again: right too.
    tap(&services, &on_left);
    tap(&services, &on_right);
    tap(&services, &pad);
    lit_until(&services, |left, right| left.0 > 0.0 && right.0 > 0.0).await;

    // Right tapped from it: left alone again.
    tap(&services, &pad);
    lit_until(&services, |left, right| left.0 > 0.0 && right.0 == 0.0).await;
}

#[tokio::test]
async fn an_effect_below_takes_the_base_color_of_each_group() {
    use crate::controls::perform;
    use crate::models::perform::{CellContent, ListValue, PerformPattern};

    let (_dir, services, venue) = library().await;
    two_groups(&services, &venue).await;
    let pattern = PerformPattern {
        blend_mode: luma_patterns::BlendMode::Below,
        ..wash("wash")
    };
    const RED: [f64; 3] = [1., 0., 0.];
    const BLUE: [f64; 3] = [0., 0., 1.];
    let base = CellContent::List {
        label: "Base".into(),
        values: vec![ListValue::Color(RED), ListValue::Color(BLUE)],
        fade: 0.,
        unit: luma_patterns::press::TimeUnit::Beats,
        targets: Vec::new(),
        step: None,
    };
    open_on_groups(&services, &venue, &pattern, vec![base]).await;
    let near = |color: [f32; 3], want: [f64; 3]| {
        color
            .iter()
            .zip(want)
            .all(|(got, want)| (f64::from(*got) - want).abs() < 1e-3)
    };

    // The base alone lights nothing, and colors every head.
    tap(&services, &perform::value("cell-2", 0));
    let ((left, color), (right, _)) = lit_until(&services, |left, right| {
        near(left.1, RED) && near(right.1, RED)
    })
    .await;
    assert_eq!((left, right), (0.0, 0.0), "a color, no light: {color:?}");

    // The effect takes it, then left's own once left is blue.
    tap(&services, &perform::pad(&pattern).expect("a pad"));
    lit_until(&services, |left, right| {
        left.0 > 0.0 && near(left.1, RED) && near(right.1, RED)
    })
    .await;
    tap(&services, &perform::cell("cell-0"));
    tap(&services, &perform::value("cell-2", 1));
    lit_until(&services, |left, right| {
        right.0 > 0.0 && near(left.1, BLUE) && near(right.1, RED)
    })
    .await;
}

#[tokio::test]
async fn a_speed_fader_changes_a_chases_rate_on_the_bar() {
    use crate::controls::perform;
    use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
    use crate::models::perform::{PerformLayout, PerformPattern};

    let (_dir, services, venue) = library().await;
    two_groups(&services, &venue).await;
    // A chase whose every a fader sets, 1 beat at 0 to 2 at 1; its light is
    // each event's progress, so 0 where one starts. No pad: it plays.
    let graph = luma_patterns::ClipGraph::from_json(
        &json!({"version": 3, "nodes": {
            "fader": {"kind": "control", "settings": {"name": "speed"}},
            "speed": {"kind": "curve", "inputs": {"x": {"node": "fader"}, "low": 1, "high": 2}},
            "time1": {"kind": "time", "inputs": {"every": {"node": "speed"}}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}})
        .to_string(),
    )
    .unwrap();
    let pattern = PerformPattern {
        graph,
        ..wash("chase")
    };
    let controls = services.controls.venue(&venue);
    let mut access = VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue))
        .await
        .unwrap();
    let (wants, _wanted) = tokio::sync::mpsc::unbounded_channel();
    let (_, compiled) = super::live::compile(
        0,
        &mut access,
        &services.fixtures_root,
        &services.storage,
        controls.clone(),
        std::slice::from_ref(&pattern),
        &PerformLayout::generated(std::slice::from_ref(&pattern)),
        None,
        wants,
    )
    .await
    .unwrap();
    let (mut live, _build) = compiled;

    // Play the set on the tapped tempo's grid, 120 BPM, a sixteenth of a
    // beat a frame, moving the fader mid-bar.
    let now = Instant::now();
    let mut scratch = crate::eval::Arena::default();
    let mut starts = Vec::new();
    for frame in 0..=16 * 16 {
        let beat = f64::from(frame) / 16.;
        if beat == 5.25 {
            controls.set(&perform::node("chase", "fader"), 1.0);
        }
        let time = (beat * 0.5) as f32;
        live.tick(time, now);
        assert!(live.update_levels());
        let lit = live
            .render_onto(Default::default(), time, now, &mut scratch)
            .primitives
            .values()
            .map(|head| head.dimmer)
            .fold(0.0_f32, f32::max);
        if lit < 1e-4 {
            starts.push(beat);
        }
    }
    assert_eq!(
        starts,
        [0., 1., 2., 3., 4., 5., 6., 8., 10., 12., 14., 16.],
        "every beat, then every two from the next they share"
    );
}
