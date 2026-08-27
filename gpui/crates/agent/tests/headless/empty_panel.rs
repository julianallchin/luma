//! The panel before its first tab.
//!
//! An empty workspace used to be a second, silent reason to hide the panel —
//! so the surface that offers the first tab was withheld until a tab existed.
//! `Universe setup` has no sidebar path (a track or a pattern is opened by
//! clicking one; a room is not), which made it unreachable outright: the `+`
//! lives in the panel, and the panel was not there.
//!
//! What is asserted is the way out of that state, by both routes a user has:
//! the toggle in the window's corner, and ⌘T.

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
    until("the track list", (s) => s.find({ role: "input", label: "Search tracks\u2026" }) !== undefined);
"#;

const READ: &str = r#"
    function read() {
        const shot = app.snapshot();
        const empty = shot.find({ role: "card", label: "Empty panel" });
        const universe = shot.find({ role: "button", label: "Universe setup" });
        const panel = shot.find({ role: "button", label: "panel-toggle" });
        const add = shot.find({ role: "button", label: "new-tab" });
        return {
            empty: empty === undefined ? null : empty.bounds.width,
            universe: universe === undefined ? null : universe.bounds.width,
            universeEnabled: universe === undefined ? null : universe.enabled,
            panelEnabled: panel === undefined ? null : panel.enabled,
            add: add === undefined ? null : add.bounds.x,
        };
    }
"#;

#[test]
fn an_empty_panel_offers_the_three_ways_to_open_a_tab() {
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

    // All three choices, by their canonical labels — the same list the `+`
    // menu offers, so the two presentations cannot drift.
    let labels: Vec<&str> = out["landedLabels"]
        .as_array()
        .expect("an array of labels")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for expected in ["Universe setup", "Pattern editor", "Track editor"] {
        assert!(
            labels.contains(&expected),
            "the empty panel did not offer {expected:?}: {labels:?}"
        );
    }

    // And exactly one offer: no `+` while the empty state is up, or "no tabs,
    // want a tab" would have two answers.
    assert!(
        out["landed"]["add"].is_null(),
        "the add control appeared beside the empty state: {:#}",
        out["landed"]
    );

    // A venue is selected, so the room itself can always be opened.
    assert_eq!(
        out["landed"]["universeEnabled"].as_bool(),
        Some(true),
        "Universe setup was not offered with a venue selected: {:#}",
        out["landed"]
    );
}

/// Closing the last tab changes *what the panel holds*, not how wide it is.
///
/// It used to change both: the close snapped the pane's width to zero — the
/// leftover from when emptiness hid the panel — and the next frame read that
/// as "open from nothing", so the empty state arrived on a full slide-in from
/// the window's edge. Motion is on here for exactly that reason: with it off,
/// the snap and the recovery land in the same frame and the bug is invisible.
#[test]
fn closing_the_last_tab_does_not_resize_the_panel() {
    let mut harness = fixture("empty-panel-width")
        .with_motion()
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
            function seam() {
                const node = app.snapshot().find({ role: "slider", label: "Workspace width" });
                return node === undefined ? null : node.bounds.x;
            }
            nav.trackEditor("Test Venue", "Aurora");
            until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
            // Long enough for the panel's own entrance (RESIZE is one SWEEP,
            // 270ms) to settle, so the reading below is the resting width.
            app.frames(10, { waitMs: 40 });
            const opened = seam();

            // Every frame of the transition, not just the settled one: a slide
            // that leaves and returns is invisible from the ends.
            app.action("luma::CloseTab");
            const during = [];
            for (let i = 0; i < 24; i++) {
                app.frames(1, { waitMs: 12 });
                during.push(seam());
            }
            const empty = app.snapshot().find({ role: "card", label: "Empty panel" });
            ({ opened, during, empty: empty === undefined ? null : empty.bounds.width })
        "#,
        ),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let opened = out["opened"].as_f64().expect("the seam before the close");
    for (frame, seam) in out["during"]
        .as_array()
        .expect("an array of frames")
        .iter()
        .enumerate()
    {
        assert_eq!(
            seam.as_f64(),
            Some(opened),
            "the panel resized on frame {frame} of the close: {:#}",
            out["during"]
        );
    }
    assert!(
        !out["empty"].is_null(),
        "the panel kept its width but never showed the empty state: {out:#}"
    );
}

#[test]
fn new_tab_reaches_universe_setup_with_no_tabs_open() {
    let mut harness = harness("empty-panel-new-tab");
    let result = harness.exec(
        &support::script(&format!(
            r#"
            {READ}
            {OPEN_VENUE}
            app.frames(2);

            // The regression: ⌘T with an empty workspace produced nothing at
            // all — no menu (the strip that anchors it was not shown) and no
            // panel. It must now land somewhere that can open a room.
            app.action("luma::NewTab");
            app.frames(4);
            const afterNewTab = read();

            app.click(app.snapshot().find({{ role: "button", label: "Universe setup" }}));
            until("the universe tab", (s) =>
                s.find({{ role: "button", label: "panel-toggle" }}) !== undefined &&
                s.find({{ role: "card", label: "Empty panel" }}) === undefined);
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
        !out["afterNewTab"]["universe"].is_null(),
        "⌘T with no tabs open reached nothing that can open one: {:#}",
        out["afterNewTab"]
    );

    // Opening it replaces the empty state with the tab — the panel stops
    // offering a first tab once it has one.
    assert!(
        out["opened"]["empty"].is_null(),
        "the empty state outlived the tab it opened: {:#}",
        out["opened"]
    );
    assert!(
        !out["tabs"].as_array().expect("an array").is_empty(),
        "no tab was opened: {:#}",
        out["tabs"]
    );
}
