//! The window's two fixed corners: the toggles hold still, the clusters move.
//!
//! Three regressions live here, and all three read as "a button moved on its
//! own". The sidebar toggle used to be rendered by whichever region was
//! leftmost, so closing the sidebar clipped it away inside that region's
//! shrinking pane and then re-mounted it in the thread's band beside
//! back/forward — it vanished mid-slide and came back somewhere else. The `+`
//! had two homes for the same reason and moved between them. And a cluster
//! that reserved its room by asking "am I the leftmost region?" snapped 84px
//! left on the first frame of a slide, because a sidebar one pixel open has
//! already stopped being leftmost while its neighbour still starts under the
//! lights. The `+` has one home now — the strip, which is the panel's — so
//! what is asserted about it here is that it leaves with the panel.
//!
//! What is asserted is therefore *position across a state change*, not
//! position: an anchor whose x moves at all has stopped being an anchor.
//!
//! Motion is snapped here (`support::Fixture::open` sets `LUMA_MOTION=off`) for
//! the settled readings; `mid_slide` re-opens with motion on and samples the
//! frames in between, which is where the clipping bug actually showed.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// `name` is per-test, not per-file: the fixture keys its seeded library
/// directory by it, and two tests in one binary run on separate threads — a
/// shared name is two harnesses opening one SQLite file and the second losing
/// the race with "database is locked".
fn harness(name: &'static str, motion: bool) -> Harness {
    let fixture = Fixture::new(
        name,
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .window(1280.0, 800.0);
    if motion {
        fixture.with_motion()
    } else {
        fixture
    }
    .open(Mode::Headless)
}

/// Both toggles, the `+`, and the tab strip's left edge, in one reading.
const READ: &str = r#"
    function read() {
        const shot = app.snapshot();
        const sidebar = shot.find({ role: "button", label: "sidebar-toggle" });
        const panel = shot.find({ role: "button", label: "panel-toggle" });
        const add = shot.find({ role: "button", label: "new-tab" });
        const strip = shot.find({ role: "card", label: "Tab strip" });
        // The thread's own left cluster — the thing the fixed toggle pushes.
        const back = shot.find({ role: "button", label: "Back" });
        const search = shot.find({ role: "input", label: "Search tracks…" });
        return {
            sidebarToggle: sidebar === undefined ? null : sidebar.bounds.x,
            panelToggle: panel === undefined ? null : panel.bounds.x,
            add: add === undefined ? null : add.bounds.x,
            strip: strip === undefined ? null : strip.bounds.x,
            back: back === undefined ? null : back.bounds.x,
            sidebarOpen: search !== undefined && search.bounds.width > 0,
        };
    }
"#;

const SETTLED: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    const bothOpen = read();

    app.action("luma::ToggleSidebar");
    app.frames(2);
    const sidebarClosed = read();
    app.action("luma::ToggleSidebar");
    app.frames(2);
    const sidebarReopened = read();

    app.action("luma::ToggleWorkspace");
    app.frames(2);
    const panelClosed = read();
    app.action("luma::ToggleWorkspace");
    app.frames(2);
    const panelReopened = read();

    ({ bothOpen, sidebarClosed, sidebarReopened, panelClosed, panelReopened })
"#;

/// One frame per step of the slide, so a control that is clipped away
/// part-way through is caught where it actually goes missing.
const MID_SLIDE: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);

    function sample(action, steps) {
        app.action(action);
        const seen = [];
        for (let i = 0; i < steps; i++) {
            app.frames(1, { waitMs: 12 });
            seen.push(read());
        }
        return seen;
    }

    const closing = sample("luma::ToggleSidebar", 16);
    const opening = sample("luma::ToggleSidebar", 16);
    const panelClosing = sample("luma::ToggleWorkspace", 16);
    const panelOpening = sample("luma::ToggleWorkspace", 16);
    ({ closing, opening, panelClosing, panelOpening })
"#;

fn run(name: &'static str, script: &str, motion: bool) -> Value {
    let mut harness = harness(name, motion);
    let result = harness.exec(
        &support::script(&format!("{READ}\n{script}")),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

fn at(value: &Value, state: &str, key: &str) -> f64 {
    value[state][key]
        .as_f64()
        .unwrap_or_else(|| panic!("{state}.{key} was not a number in {:#}", value[state]))
}

#[test]
fn the_toggles_do_not_move_when_the_panels_they_open_do() {
    let out = run("chrome-anchors-settled", SETTLED, false);

    assert!(
        out["bothOpen"]["sidebarOpen"].as_bool() == Some(true) && !out["bothOpen"]["add"].is_null(),
        "the walk did not end with both regions open: {:#}",
        out["bothOpen"]
    );

    // The whole rule, stated twice. Every reading is a different panel
    // configuration; the anchors are the same pixel in all of them.
    let states = [
        "bothOpen",
        "sidebarClosed",
        "sidebarReopened",
        "panelClosed",
        "panelReopened",
    ];
    let left = at(&out, "bothOpen", "sidebarToggle");
    let right = at(&out, "bothOpen", "panelToggle");
    for state in states {
        assert_eq!(
            at(&out, state, "sidebarToggle"),
            left,
            "the sidebar toggle moved in {state}: {:#}",
            out[state]
        );
        assert_eq!(
            at(&out, state, "panelToggle"),
            right,
            "the panel toggle moved in {state}: {:#}",
            out[state]
        );
    }

    // …and it is a *fixed* point, not merely a stable one: the left anchor
    // sits immediately right of the traffic lights, the right one a control's
    // width in from the window's trailing edge.
    assert_eq!(left, 74.0, "the left anchor is not beside the lights");
    assert_eq!(
        right, 1244.0,
        "the right anchor is not at the window's edge"
    );

    // The cluster is what moves. Opening the sidebar pushes the thread's
    // back/forward pair right; closing it returns the pair to the anchor.
    // (The tab strip does not move here: with the panel open the *workspace*
    // band owns the strip, and that band is pinned to the window's right edge.)
    assert!(
        at(&out, "bothOpen", "back") > at(&out, "sidebarClosed", "back"),
        "opening the sidebar did not push the left cluster right: {:#} vs {:#}",
        out["bothOpen"],
        out["sidebarClosed"]
    );
    assert_eq!(
        at(&out, "sidebarReopened", "back"),
        at(&out, "bothOpen", "back"),
        "the cluster did not return to where the sidebar had put it"
    );
    // Closed, the cluster clears the lights and the toggle; open, it rides the
    // sidebar's seam. Both are the same `max`, read at its two ends.
    assert_eq!(
        at(&out, "sidebarClosed", "back"),
        108.0,
        "the cluster does not rest against the left anchor when the sidebar is shut"
    );

    // The strip is the panel's, and the `+` is the strip's. Closing the panel
    // puts all three away together; nothing borrows the strip into the thread's
    // band, which is what used to give the `+` two homes and a seam to move
    // across. ⌘T is the way back, and it opens the panel first — see
    // `empty_panel`.
    for key in ["strip", "add"] {
        assert!(
            out["panelClosed"][key].is_null(),
            "the {key} outlived the panel it belongs to: {:#}",
            out["panelClosed"]
        );
    }
    for state in ["bothOpen", "panelReopened"] {
        assert!(
            at(&out, state, "add") > at(&out, state, "strip"),
            "the add control parted company with its strip in {state}: {:#}",
            out[state]
        );
    }
}

#[test]
fn neither_toggle_blinks_out_part_way_through_a_slide() {
    let out = run("chrome-anchors-slide", MID_SLIDE, true);

    for phase in ["closing", "opening", "panelClosing", "panelOpening"] {
        let frames = out[phase]
            .as_array()
            .unwrap_or_else(|| panic!("{phase} was not an array in {:#}", out[phase]));
        let left = frames[0]["sidebarToggle"].as_f64();
        let right = frames[0]["panelToggle"].as_f64();
        for (i, frame) in frames.iter().enumerate() {
            assert!(
                !frame["sidebarToggle"].is_null(),
                "the sidebar toggle vanished on frame {i} of {phase}: {frame:#}"
            );
            assert!(
                !frame["panelToggle"].is_null(),
                "the panel toggle vanished on frame {i} of {phase}: {frame:#}"
            );
            assert_eq!(
                frame["sidebarToggle"].as_f64(),
                left,
                "the sidebar toggle drifted on frame {i} of {phase}"
            );
            assert_eq!(
                frame["panelToggle"].as_f64(),
                right,
                "the panel toggle drifted on frame {i} of {phase}"
            );
        }
    }

    // The pushed cluster tracks the panel's edge, and only ever one way per
    // slide: the jump this rework removed showed up as a single frame going
    // backwards while every other frame went forwards.
    for (phase, forward) in [("closing", false), ("opening", true)] {
        let frames = out[phase].as_array().expect("an array of frames");
        let strip: Vec<f64> = frames
            .iter()
            .filter_map(|frame| frame["back"].as_f64())
            .collect();
        for pair in strip.windows(2) {
            let (previous, next) = (pair[0], pair[1]);
            if forward {
                assert!(
                    next >= previous,
                    "the cluster moved left during {phase}: {previous} then {next} in {strip:?}"
                );
            } else {
                assert!(
                    next <= previous,
                    "the cluster moved right during {phase}: {previous} then {next} in {strip:?}"
                );
            }
        }
    }
}
