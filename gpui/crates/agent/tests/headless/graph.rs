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
use super::support;
use support::session;

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
/// The score that puts the seeded track *in* the venue — the sidebar lists
/// membership, not the whole library, so without one there is no row to open
/// a track editor from.
const SCORE_REQUEST_ID: &str = "7f1c2c60-0000-4000-8000-000000000003";

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
    // A venue and a track, so a track editor can be opened first: the graph
    // editor is not openable without a track context (§6/§9 ruling 1 of the
    // graph-editor design doc), so the walk below goes venue → track →
    // pattern rather than straight at the picker. Rows first, while admission
    // is still unarmed — the same window `support::Fixture` writes through.
    let audio = config_dir.join("aurora.wav");
    std::fs::write(&audio, support::wav(8)).expect("failed to write the fixture audio");
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, ?, ?)")
        .bind(support::VENUE)
        .bind(session::PRINCIPAL)
        .bind(support::VENUE_NAME)
        .execute(&db.0)
        .await
        .expect("failed to seed the venue");
    sqlx::query(
        "INSERT INTO tracks
            (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
         VALUES (?, ?, 'graph-aurora-8s', ?, 'Nightliner', 8.0, ?, CURRENT_TIMESTAMP)",
    )
    .bind(support::TRACK)
    .bind(session::PRINCIPAL)
    .bind(support::TRACK_NAME)
    .bind(audio.to_string_lossy().to_string())
    .execute(&db.0)
    .await
    .expect("failed to seed the track");
    session::signed_in(config_dir).await;
    let state_db = luma_lib::database::local::state::init_state_db_at(config_dir)
        .await
        .expect("failed to open the fixture state database");
    luma_lib::database::local::auth::bootstrap_headless_admission(&db.0, &state_db.0)
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

    call(
        &services,
        "create_score",
        json!({
            "requestId": SCORE_REQUEST_ID,
            "trackId": support::TRACK,
            "venueId": support::VENUE,
            "name": "Fixture Score",
        }),
    )
    .await;

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
/// developer's. Named after the process *and the test*: two tests in one
/// binary run concurrently, and a shared name means one test reseeds — and
/// first deletes — the library the other is mid-walk in.
fn fixture_config_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-graph-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the fixture runtime")
        .block_on(seed(&dir));
    dir
}

fn harness(name: &str) -> Harness {
    let config_dir = fixture_config_dir(name);
    let root: gpui_agent::RootFactory = Arc::new(|_: &mut Window, cx: &mut App| -> AnyView {
        luma_app::init(cx);
        let library = luma_app::Library::open().expect("failed to open the fixture library");
        cx.new(|cx| luma_app::Luma::new(library, cx)).into()
    });
    Harness::headless(
        Config {
            mode: Mode::Headless,
            call_timeout: Duration::from_secs(30),
            runtime: support::runtime(config_dir),
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
            context: shot.findAll({ role: "text" })
                         .map((n) => n.label)
                         .filter((l) => l.startsWith("TRACK ")),
            cards,
        };
    }

    // The graph doors need a track context, so the walk opens the track
    // editor first: while no track editor is open the pattern rows are inert
    // (that state has its own test below).
    nav.trackEditor("Test Venue", "Aurora");
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
    let mut harness = harness("drag");
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 1. The canvas names every node card it painted, and the toolbar names
    //    the track the graph is evaluated against — the resolved context is
    //    visible, not implicit (§6).
    let opened = &out["opened"];
    assert_eq!(opened["status"], json!(["3 NODES"]));
    assert!(
        opened["context"]
            .as_array()
            .is_some_and(|texts| texts.iter().any(|t| t == "TRACK AURORA")),
        "the toolbar does not name the evaluation track: {opened:#}"
    );
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

/// The trackless doors: with no track editor open, a pattern row is present
/// but inert with the stated reason, and the `+` menu's Pattern choice says
/// the same thing. Opening a track editor arms both — the same rule, read
/// through both surfaces (§6/§9 ruling 1).
#[test]
fn the_graph_doors_are_inert_until_a_track_editor_is_open() {
    let mut harness = harness("doors");
    let result = harness.exec(
        &support::script(
            r#"
            nav.venue("Test Venue");
            nav.patterns();
            const before = until("the trackless pattern row", (s) =>
                s.find({ role: "row", label: "Fixture Chain" }));
            const row = before.find({ role: "row", label: "Fixture Chain" });
            const reason = before.find((n) =>
                n.role === "text" && n.label === "OPEN A TRACK TO EDIT PATTERNS");
            // A click on the inert row must open nothing.
            app.click(row, { restale: "match" });
            app.frames(6);
            const afterClick = app.snapshot();
            nav.dismiss();

            nav.track("Aurora");
            nav.patterns();
            const armed = until("the armed pattern row", (s) => {
                const node = s.find({ role: "row", label: "Fixture Chain" });
                return node && node.enabled !== false ? s : undefined;
            });
            ({
                rowDisabled: row.enabled === false,
                reasonShown: reason !== undefined,
                clickOpenedNothing:
                    afterClick.find({ role: "text", label: "3 NODES" }) === undefined,
                armedRowEnabled:
                    armed.find({ role: "row", label: "Fixture Chain" }).enabled !== false,
            })
            "#,
        ),
        Duration::from_secs(180),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(out["rowDisabled"], true, "trackless row was not inert");
    assert_eq!(out["reasonShown"], true, "the inert list does not say why");
    assert_eq!(
        out["clickOpenedNothing"], true,
        "the inert row opened a tab"
    );
    assert_eq!(
        out["armedRowEnabled"], true,
        "a track editor did not arm the row"
    );
}

// -- phase 1: hit-tree, selection, delete, undo -------------------------------
//
// The scripts below share one vocabulary with the app: a card node per graph
// card whose `focused` flag is its selection (where the keyboard verbs land
// next), a `button` node per port region ("n1 input in"), and input/select
// nodes per widget slot ("n1 param …") — all registered from the same regions
// the hit test resolves against.

/// The walk every phase-1 test opens with: a track editor for context, then
/// the pattern, expanded.
const OPEN: &str = r#"
    function open() {
        nav.trackEditor("Test Venue", "Aurora");
        nav.patterns();
        app.frames(6);
        nav.step("the pattern Fixture Chain", "row", "Fixture Chain");
        nav.expand();
        app.frames(8);
    }

    /** The titles of the cards reporting focused — the visible selection. */
    function selection() {
        return app.snapshot()
            .findAll((n) => n.role === "card" && n.focused)
            .map((n) => n.label)
            .sort();
    }

    function nodesStatus() {
        return app.snapshot()
            .findAll({ role: "text" })
            .map((n) => n.label)
            .find((l) => l.endsWith("NODES"));
    }
"#;

/// The hit-tree, read through its registrations: a press on a port region
/// resolves to the port's node, and the widget slots are addressable.
#[test]
fn a_port_press_selects_its_node_and_the_regions_are_addressable() {
    let mut harness = harness("hits");
    let script = format!(
        r#"
        {OPEN}
        open();
        const shot = app.snapshot();
        const port = shot.find({{ role: "button", label: "n1 input in" }});
        const outPort = shot.find({{ role: "button", label: "n0 output out" }});
        // Round's operation selector and Math's — every param slot is a
        // region, and every region is named "{{node}} param {{id}}".
        const roundSelect = shot.find({{ role: "select", label: "n1 param operation" }})
            !== undefined;
        const mathSelect = shot.findAll({{ role: "select" }})
            .some((n) => n.label.startsWith("n2 param "));
        app.click(port);
        const afterPort = selection();

        // Plain click selects one card; shift-click widens the selection.
        app.click(app.snapshot().find({{ role: "card", label: "Time Ramp" }}));
        const single = selection();
        app.click(app.snapshot().find({{ role: "card", label: "Math" }}),
                  {{ modifiers: ["shift"] }});
        const widened = selection();
        // Shift-click again takes it back out.
        app.click(app.snapshot().find({{ role: "card", label: "Math" }}),
                  {{ modifiers: ["shift"] }});
        const narrowed = selection();

        ({{
            portFound: port !== undefined,
            outPortFound: outPort !== undefined,
            roundSelect,
            mathSelect,
            afterPort,
            single,
            widened,
            narrowed,
        }})
        "#
    );
    let result = harness.exec(&support::script(&script), Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(out["portFound"], true, "no node for Round's input port");
    assert_eq!(
        out["outPortFound"], true,
        "no node for Time Ramp's output port"
    );
    assert_eq!(
        out["roundSelect"], true,
        "Round's operation slot is not addressable"
    );
    assert_eq!(
        out["mathSelect"], true,
        "Math's select slot is not addressable"
    );
    assert_eq!(
        out["afterPort"],
        json!(["Round"]),
        "a press on n1's port did not resolve to n1"
    );
    assert_eq!(out["single"], json!(["Time Ramp"]));
    assert_eq!(out["widened"], json!(["Math", "Time Ramp"]));
    assert_eq!(out["narrowed"], json!(["Time Ramp"]));
}

/// Delete removes the selection through the document, and undo is a write:
/// the restored graph — and the restored selection — survive leaving the
/// screen.
#[test]
fn delete_removes_the_selection_and_undo_restores_document_and_selection() {
    let mut harness = harness("undo");
    let script = format!(
        r#"
        {OPEN}
        open();
        app.click(app.snapshot().find({{ role: "card", label: "Round" }}));
        app.key("delete");
        app.frames(8);
        const deleted = {{
            status: nodesStatus(),
            round: app.snapshot().find({{ role: "card", label: "Round" }}) !== undefined,
            selection: selection(),
        }};
        app.key("secondary-z");
        app.frames(8);
        const undone = {{ status: nodesStatus(), selection: selection() }};
        app.key("secondary-shift-z");
        app.frames(8);
        const redone = {{ status: nodesStatus() }};
        app.key("secondary-z");
        app.frames(8);
        // Leave the screen entirely and come back: only a written undo
        // survives this.
        nav.closeTab();
        nav.pattern("Fixture Chain");
        app.frames(8);
        const reopened = {{ status: nodesStatus() }};
        ({{ deleted, undone, redone, reopened }})
        "#
    );
    let result = harness.exec(&support::script(&script), Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(
        out["deleted"]["status"], "2 NODES",
        "delete did not remove the node"
    );
    assert_eq!(
        out["deleted"]["round"], false,
        "the deleted card is still drawn"
    );
    assert_eq!(
        out["deleted"]["selection"],
        json!([]),
        "the selection still names a deleted node"
    );
    assert_eq!(
        out["undone"]["status"], "3 NODES",
        "undo did not restore the node"
    );
    assert_eq!(
        out["undone"]["selection"],
        json!(["Round"]),
        "undo did not restore the selection with the document"
    );
    assert_eq!(out["redone"]["status"], "2 NODES", "redo did not re-remove");
    assert_eq!(
        out["reopened"]["status"], "3 NODES",
        "the undone document was not the one written"
    );
}

/// Marquee: a shift-drag across empty canvas sweeps a rect, selection is
/// what the rect crosses, and a bare press on the ground clears it.
#[test]
fn a_marquee_selects_what_it_crosses() {
    let mut harness = harness("marquee");
    let script = format!(
        r#"
        {OPEN}
        open();
        // Only the graph's cards: a snapshot names every card in the window,
        // and a sweep sized to the sidebar's would leave the canvas.
        const titles = ["Time Ramp", "Round", "Math"];
        const cards = app.snapshot()
            .findAll({{ role: "card" }})
            .filter((c) => titles.includes(c.label));
        const left = Math.min(...cards.map((c) => c.bounds.x));
        const top = Math.min(...cards.map((c) => c.bounds.y));
        const right = Math.max(...cards.map((c) => c.bounds.x + c.bounds.width));
        const bottom = Math.max(...cards.map((c) => c.bounds.y + c.bounds.height));
        const ramp = cards.find((c) => c.label === "Time Ramp");

        // A sweep into the first card's top-left quadrant takes only it —
        // rect-intersect, so touching the card is enough.
        app.drag(
            {{ x: left - 15, y: top - 15 }},
            {{ dx: 20 + ramp.bounds.width / 2, dy: 20 + ramp.bounds.height / 2 }},
            {{ modifiers: ["shift"] }},
        );
        const one = selection();

        // A sweep over everything takes all three.
        app.drag(
            {{ x: left - 15, y: top - 15 }},
            {{ dx: right - left + 30, dy: bottom - top + 30 }},
            {{ modifiers: ["shift"] }},
        );
        const all = selection();

        // Multi-delete over the marquee'd selection, then undo brings the
        // graph and the selection back.
        app.key("backspace");
        app.frames(8);
        const deleted = {{ status: nodesStatus(), selection: selection() }};
        app.key("secondary-z");
        app.frames(8);
        const undone = {{ status: nodesStatus(), selection: selection() }};

        // A bare press on empty ground clears what a marquee selected.
        app.drag({{ x: left - 15, y: top - 15 }}, {{ dx: 4, dy: 4 }});
        const cleared = selection();
        ({{ one, all, deleted, undone, cleared }})
        "#
    );
    let result = harness.exec(&support::script(&script), Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(
        out["one"],
        json!(["Time Ramp"]),
        "the partial sweep took the wrong set"
    );
    assert_eq!(
        out["all"],
        json!(["Math", "Round", "Time Ramp"]),
        "the full sweep did not take all three"
    );
    assert_eq!(
        out["deleted"]["status"], "0 NODES",
        "multi-delete left nodes behind"
    );
    assert_eq!(
        out["undone"]["status"], "3 NODES",
        "undo did not restore the sweep's delete"
    );
    assert_eq!(
        out["undone"]["selection"],
        json!(["Math", "Round", "Time Ramp"]),
        "undo did not restore the marquee'd selection"
    );
    assert_eq!(
        out["cleared"],
        json!([]),
        "a ground press did not clear the selection"
    );
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
