//! The panel before its first tab.
//!
//! An empty workspace used to be a second, silent reason to hide the panel —
//! so the surface that offers the first tab was withheld until a tab existed.
//!
//! What is asserted is the way out of that state, by both routes a user has:
//! the toggle in the window's corner, and ⌘T. The venue page is not a tab:
//! the sidebar's Venue row opens it, and the panel's offer does not.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

fn harness(name: &'static str) -> Harness {
    fixture(name).open(Mode::Headless)
}

/// `name` is per-test: the fixture keys its seeded library directory by it,
/// and two harnesses on one name race for the same SQLite file.
fn fixture(name: &'static str) -> Fixture {
    Fixture::new(
        name,
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .window(1280.0, 800.0)
}

/// Land on a venue with no tabs open: the sidebar's list, nothing opened from
/// it. `nav.venue` stops exactly there.
const OPEN_VENUE: &str = r#"
    nav.venue("Test Venue");
    until("the track list", (s) => s.find({ role: "input", label: "Search tracks" }) !== undefined);
"#;

const READ: &str = r#"
    function read() {
        const shot = app.snapshot();
        const empty = shot.find({ role: "card", label: "Empty panel" });
        const venue = shot.find({ role: "card", label: "Test Venue Venue" });
        const panel = shot.find({ role: "button", label: "panel-toggle" });
        const add = shot.find({ role: "button", label: "new-tab" });
        return {
            empty: empty === undefined ? null : empty.bounds.width,
            venue: venue === undefined ? null : venue.bounds.width,
            strip: shot.find({ role: "card", label: "Tab strip" }) !== undefined,
            panelEnabled: panel === undefined ? null : panel.enabled,
            add: add === undefined ? null : add.bounds.x,
        };
    }
"#;

#[test]
fn an_empty_panel_offers_the_ways_to_open_a_tab() {
    let mut harness = harness("empty-panel-offers");
    let result = harness.exec(
        &support::script(&format!(
            r#"
            {READ}
            function labels() {{
                return app.snapshot().nodes
                    .filter((n) => n.role === "button")
                    .map((n) => n.label);
            }}
            {OPEN_VENUE}
            app.frames(2);
            const landed = read();
            const landedLabels = labels();

            // The toggle is the door, and a door swings both ways: shut the
            // panel, then bring it back. A toggle that only closed would be
            // the regression again, one press later.
            app.click(app.snapshot().find({{ role: "button", label: "panel-toggle" }}));
            app.frames(4);
            const shut = read();
            app.click(app.snapshot().find({{ role: "button", label: "panel-toggle" }}));
            app.frames(4);
            const reopened = read();

            ({{ landed, landedLabels, shut, reopened }})
        "#
        )),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    assert_eq!(
        out["landed"]["panelEnabled"].as_bool(),
        Some(true),
        "the panel toggle was inert with no tabs, so the empty state had no door: {:#}",
        out["landed"]
    );

    // With no tabs the panel rests open onto its empty state — that is the
    // whole point of the state existing.
    assert!(
        !out["landed"]["empty"].is_null(),
        "the panel did not open onto its empty state: {:#}",
        out["landed"]
    );
    assert!(
        out["shut"]["empty"].is_null(),
        "the toggle did not put the empty panel away: {:#}",
        out["shut"]
    );
    assert!(
        !out["reopened"]["empty"].is_null(),
        "the toggle closed the empty panel and could not bring it back: {:#}",
        out["reopened"]
    );

    // Every choice, by its canonical label — the same list the `+` menu
    // offers, so the two presentations cannot drift.
    let labels: Vec<&str> = out["landedLabels"]
        .as_array()
        .expect("an array of labels")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for expected in ["Track editor"] {
        assert!(
            labels.contains(&expected),
            "the empty panel did not offer {expected:?}: {labels:?}"
        );
    }
    assert!(
        !labels.contains(&"Venue"),
        "the empty panel offered the venue as a tab: {labels:?}"
    );

    // And exactly one offer: no `+` while the empty state is up, or "no tabs,
    // want a tab" would have two answers.
    assert!(
        out["landed"]["add"].is_null(),
        "the add control appeared beside the empty state: {:#}",
        out["landed"]
    );
}

/// The last tab collapses the panel; reopening reveals choices, not a new tab.
#[test]
fn closing_the_last_tab_springs_closed_and_reopens_empty() {
    let mut harness = fixture("empty-panel-width")
        .with_motion()
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        function seam() {
            const n = app.snapshot().find({ role: "slider", label: "Workspace width" });
            return n === undefined ? null : n.bounds.x;
        }
        nav.trackEditor("Test Venue", "Aurora");
        until("track tab", s => s.nodes.some(n => n.role === "button" && n.label.startsWith("Close ")));
        app.frames(20, { waitMs: 40 });
        const opened = seam();
        app.action("luma::CloseTab");
        const closing = [];
        for (let i = 0; i < 20; i++) {
            app.frames(1, { waitMs: 40 }); closing.push(seam());
        }
        const closed = seam();
        app.action("luma::ToggleWorkspace");
        const opening = [];
        for (let i = 0; i < 20; i++) {
            app.frames(1, { waitMs: 40 }); opening.push(seam());
        }
        const shot = app.snapshot();
        ({ opened, closing, closed, opening, reopened: seam(),
           empty: shot.find({ role: "card", label: "Empty panel" }) !== undefined,
           tabs: shot.nodes.filter(n => n.role === "button" && n.label.startsWith("Close ")).length })
    "#), Duration::from_secs(300));
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    let opened = out["opened"].as_f64().expect("open seam");
    assert!(out["closed"].is_null(), "{out:#}");
    for direction in ["closing", "opening"] {
        assert!(
            out[direction]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_f64)
                .any(|x| x > opened + 2.0 && x < 1270.0),
            "no intermediate spring frames: {out:#}"
        );
    }
    assert!(
        (out["reopened"].as_f64().unwrap() - opened).abs() < 1.0,
        "{out:#}"
    );
    assert_eq!(out["empty"], true, "{out:#}");
    assert_eq!(out["tabs"], 0, "{out:#}");
}

#[test]
fn new_tab_with_no_tabs_reaches_the_empty_panel_and_the_venue_row_opens_the_room() {
    let mut harness = harness("empty-panel-new-tab");
    let result = harness.exec(
        &support::script(&format!(
            r#"
            {READ}
            {OPEN_VENUE}
            app.frames(2);

            // The regression: ⌘T with an empty workspace produced nothing at
            // all — no menu (the strip that anchors it was not shown) and no
            // panel. It must now land on the panel's offer.
            app.action("luma::ToggleWorkspace");
            app.frames(4);
            app.action("luma::NewTab");
            app.frames(4);
            const afterNewTab = read();

            nav.venuePage("Test Venue");
            app.frames(4);
            const opened = read();
            const tabs = app.snapshot().nodes
                .filter((n) => n.role === "button" && n.label.startsWith("Close "))
                .map((n) => n.label);
            ({{ afterNewTab, opened, tabs }})
        "#
        )),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    assert!(
        !out["afterNewTab"]["empty"].is_null(),
        "⌘T with no tabs open reached nothing that can open one: {:#}",
        out["afterNewTab"]
    );

    // The venue page replaces the empty state, and it is a place, not a tab:
    // no strip, no chip, nothing to close.
    assert!(
        out["opened"]["empty"].is_null() && !out["opened"]["venue"].is_null(),
        "the Venue row did not open the venue page: {:#}",
        out["opened"]
    );
    assert_eq!(
        out["opened"]["strip"], false,
        "the venue page drew a tab strip: {:#}",
        out["opened"]
    );
    assert!(
        out["tabs"].as_array().expect("an array").is_empty(),
        "the venue page opened as a closable tab: {:#}",
        out["tabs"]
    );
}
