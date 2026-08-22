//! The pattern graph editor, driven end to end against a seeded library.
//!
//! The point of this test is that a *position is a document*. Everything else
//! on this screen is a picture of the graph, but dragging a node is a write:
//! it takes a revision, sends a canonicalized graph through
//! `save_pattern_graph_document`, and takes a new revision back. So the test
//! moves a card, reads its bounds, then leaves the screen and comes back —
//! because only the second reading can tell a repaint from a save.
//!
//! # Why the fixture goes through the seam
//!
//! Unlike the track browser's, this fixture cannot be written as SQL. A
//! pattern's graph is an authored Git document with a content-addressed
//! revision, and a row inserted behind that would be a document the editor
//! reads and can never write back to. `create_pattern` plus one
//! `save_pattern_graph_document` is both shorter and the only version that
//! produces a document the screen can actually edit — which is also what makes
//! it a fair test of the seam.

#![cfg(feature = "app")]

// Only for `support::script` — this test seeds its own fixture, but the
// navigation helpers it drives the app with are the suite's, not its own.
mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use serde_json::{json, Value};

/// Idempotency keys for the two fixture writes. Both must be UUIDs — the
/// authored store validates them — and both are fixed, so re-seeding the same
/// directory replays rather than duplicates.
const REQUEST_ID: &str = "7f1c2c60-0000-4000-8000-000000000001";
const OPERATION_ID: &str = "7f1c2c60-0000-4000-8000-000000000002";

/// The pattern the editor opens. Named so the list row and the window title
/// are both findable.
const PATTERN: &str = "Fixture Chain";

/// Three node types with three distinct catalogue names, wired in a line.
///
/// Distinct on purpose: a card is named by its *type*, so two `math` nodes
/// would give the harness two cards labelled "Math" and `find` would silently
/// take the first. Until node cards carry a per-instance name, a graph fixture
/// has to keep its titles unique.
const NODES: [(&str, &str); 3] = [("ramp", "Time Ramp"), ("round", "Round"), ("math", "Math")];

/// How far the drag moves the card, in logical pixels. The canvas opens at
/// zoom 1, so window pixels and graph units are the same thing here — which is
/// what lets the assertion be exact rather than approximate.
const DRAG_X: f64 = 100.;

// -- the fixture --------------------------------------------------------------

/// Build a pattern whose graph is `ramp -> round -> math`, at known positions.
///
/// Runs against the same `AppServices` the app opens, because the seam is the
/// only way to author a graph document — see the module docs.
async fn seed(config_dir: &Path) -> String {
    let db = luma_lib::database::local::database::init_app_db_at(config_dir)
        .await
        .expect("failed to open the fixture database");
    let state_db = luma_lib::database::local::state::init_state_db_at(config_dir)
        .await
        .expect("failed to open the fixture state database");
    luma_lib::database::local::auth::bootstrap_host_admission(&db.0, &state_db.0)
        .await
        .expect("failed to arm admission");
    let storage = luma_lib::storage::StorageRoot::from_path(config_dir.to_path_buf());
    let workspaces = Arc::new(
        luma_lib::agent_execution::workspace::PythonWorkspaceService::new(
            storage.agent_workspaces_dir(),
            Arc::new(|| Err("the fixture does not run Python workspaces".to_string())),
        ),
    );
    let services = luma_lib::dispatch::AppServices::headless(
        db,
        state_db,
        storage,
        // Nothing this fixture writes touches a fixture definition, but
        // `AppServices` wants a root; the config directory is one that exists.
        config_dir.to_path_buf(),
        workspaces,
    );

    let pattern = call(
        &services,
        "create_pattern",
        json!({ "requestId": REQUEST_ID, "name": PATTERN, "description": null }),
    )
    .await;
    let id = pattern["id"].as_str().expect("a created pattern has an id");

    let document = call(
        &services,
        "get_pattern_graph_document",
        json!({ "id": id, "implementationId": null }),
    )
    .await;

    let nodes: Vec<Value> = NODES
        .iter()
        .enumerate()
        .map(|(index, (type_id, _))| {
            json!({
                "id": format!("n{index}"),
                "typeId": type_id,
                "params": {},
                "positionX": 40.0 + index as f64 * 220.0,
                "positionY": 60.0 + index as f64 * 40.0,
            })
        })
        .collect();
    call(
        &services,
        "save_pattern_graph_document",
        json!({
            "id": id,
            "implementationId": document["implementationId"],
            "operationId": OPERATION_ID,
            "baseRevision": document["revision"],
            "graph": {
                "nodes": nodes,
                "edges": [
                    {"id": "e0", "fromNode": "n0", "fromPort": "out",
                     "toNode": "n1", "toPort": "in"},
                    {"id": "e1", "fromNode": "n1", "fromPort": "out",
                     "toNode": "n2", "toPort": "a"},
                ],
                "args": [],
            },
        }),
    )
    .await;

    id.to_string()
}

async fn call(services: &luma_lib::dispatch::AppServices, name: &str, args: Value) -> Value {
    luma_lib::dispatch::dispatch(services, name, &args)
        .await
        .unwrap_or_else(|error| panic!("fixture command {name} failed: {error}"))
}

// -- the harness --------------------------------------------------------------

/// A library of its own, seeded, so the run cannot see — or corrupt — the
/// developer's. Named after the process so two runs never share one.
fn fixture_config_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-graph-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the fixture runtime")
        .block_on(seed(&dir));
    dir
}

fn harness() -> Harness {
    std::env::set_var("LUMA_CONFIG_DIR", fixture_config_dir());
    let root: gpui_agent::RootFactory = Arc::new(|_: &mut Window, cx: &mut App| -> AnyView {
        luma_app::init(cx);
        let library = luma_app::Library::open().expect("failed to open the fixture library");
        cx.new(|cx| luma_app::Luma::new(library, cx)).into()
    });
    Harness::headless(
        Config {
            mode: Mode::Headless,
            call_timeout: Duration::from_secs(30),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the harness")
}

/// Open the pattern, read the cards, drag one, read them again, then leave and
/// come back and read them a third time.
///
/// Every reading is `{status, cards}` — the toolbar's own account of the
/// document beside the geometry it drew — so a save that only changed the
/// label, or a drag that only moved the picture, both fail.
const SCRIPT: &str = r#"
    function open() {
        nav.patterns();
        app.frames(6);
        nav.step("the pattern Fixture Chain", "row", "Fixture Chain");
        nav.expand();
        app.frames(8);
        return read();
    }

    function read() {
        const shot = app.snapshot();
        const cards = {};
        for (const card of shot.findAll({ role: "card" })) {
            cards[card.label] = { x: card.bounds.x, y: card.bounds.y };
        }
        return {
            status: shot.findAll({ role: "text" })
                        .map((n) => n.label)
                        .filter((l) => l.endsWith("NODES") || l === "SAVING"),
            cards,
        };
    }

    const opened = open();

    // Drag by a displacement: a canvas has no control at the destination, so
    // there is no node to name as the target.
    const card = app.snapshot().find({ role: "card", label: "Round" });
    app.drag(card, { dx: 100, dy: 0 });
    app.frames(8);
    const moved = read();

    // Leave the screen entirely and come back. A repaint would survive the
    // first; only a write survives this.
    nav.closeTab();
    nav.pattern("Fixture Chain");
    app.frames(8);
    const reopened = read();

    ({ opened, moved, reopened })
"#;

#[test]
fn a_node_dragged_on_the_canvas_moves_and_stays_moved() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 1. The canvas names every node card it painted.
    let opened = &out["opened"];
    assert_eq!(opened["status"], json!(["3 NODES"]));
    for (_, title) in NODES {
        assert!(
            opened["cards"][title].is_object(),
            "the canvas did not name a card for {title}: {opened:#}"
        );
    }

    // 2. The drag moved exactly the card it took hold of, exactly as far as it
    //    was told, and nothing else on the canvas.
    let moved = &out["moved"];
    assert_eq!(
        at(moved, "Round").0 - at(opened, "Round").0,
        DRAG_X,
        "the dragged card did not move by the drag distance"
    );
    assert_eq!(at(moved, "Round").1, at(opened, "Round").1, "y drifted");
    for (_, title) in [NODES[0], NODES[2]] {
        assert_eq!(
            at(moved, title),
            at(opened, title),
            "{title} moved, and it was not the one dragged"
        );
    }

    // 3. Reopening the pattern re-reads the document, so the position that
    //    comes back is the one that was written — not the one still on screen.
    let reopened = &out["reopened"];
    assert_eq!(reopened["status"], json!(["3 NODES"]));
    for (_, title) in NODES {
        assert_eq!(
            at(reopened, title),
            at(moved, title),
            "{title} came back from the document in a different place"
        );
    }
}

/// One card's top-left, from a reading.
fn at(reading: &Value, title: &str) -> (f64, f64) {
    let card = &reading["cards"][title];
    (
        card["x"]
            .as_f64()
            .unwrap_or_else(|| panic!("{title} has no x: {reading:#}")),
        card["y"]
            .as_f64()
            .unwrap_or_else(|| panic!("{title} has no y")),
    )
}
