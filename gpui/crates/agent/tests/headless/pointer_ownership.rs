//! One press, one owner.
//!
//! A seam grip is wider than the rule it pulls — a 1px target is not aimable —
//! so it overhangs the panes either side of it. gpui hit-tests in paint order
//! and reports *every* hitbox under the pointer, so for four of the grip's
//! five pixels the surface underneath took the same press: dragging the
//! stage/editor seam orbited the camera, and dragging the workspace seam
//! seeked the transport, because the timeline's ruler was under the overhang.
//!
//! Both halves of the fix are load-bearing and neither is visible in a
//! screenshot, so they are asserted here: the grip blocks the mouse for what
//! is painted behind it, and it is mounted after *both* panes so that "behind
//! it" means both of them.
//!
//! The converse is the same claim from the other side and is what stops a fix
//! that over-reaches: a gesture that starts on the canvas keeps the pointer
//! wherever it wanders, seam included.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// `name` is per-test: the fixture keys its seeded library directory by it,
/// and two harnesses on one name race for the same SQLite file.
fn harness(name: &'static str) -> Harness {
    Fixture::new(
        name,
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .open(Mode::Headless)
}

/// The reading both halves share: where the seam is, how wide the tab body is,
/// and where the playhead sits.
///
/// The playhead is the witness for a leaked press. `timeline_press` in the
/// ruler band seeks the transport, and the ruler runs the full width of the
/// editor — including the strip the workspace grip overhangs.
const READ: &str = r#"
    function read() {
        const shot = app.snapshot();
        const seam = shot.find({ role: "slider", label: "Workspace width" });
        const waveform = shot.find({ role: "card", label: "Waveform" });
        const playhead = shot.find({ role: "slider", label: "Playhead" });
        return {
            seam: seam === undefined ? null : seam.bounds.x,
            tab: waveform === undefined ? null : waveform.bounds.width,
            // Where the playhead is *in the editor*. In window space it would
            // move with the panel's edge, which is the very thing the seam
            // drag is supposed to move.
            playhead: playhead === undefined ? null : playhead.bounds.x - waveform.bounds.x,
        };
    }

    // The middle of the ruler's height at some x — the band `timeline_press`
    // seeks the transport from.
    function atRuler(x) {
        const ruler = app.snapshot().find({ role: "card", label: "Ruler" });
        return { x, y: ruler.bounds.y + ruler.bounds.height / 2 };
    }

    // The strip of the grip that overhangs the *panel*: its trailing pixel.
    // The panel is what the ruler is in, so this is where a shared press
    // would land on the timeline.
    function overPanel() {
        const g = app.snapshot().find({ role: "slider", label: "Workspace width" });
        return g.bounds.x + g.bounds.width - 1;
    }

    nav.trackEditor("Test Venue", "Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
"#;

/// Drag the seam across the editor. The panel moves; nothing under the grip does.
const SEAM_SCRIPT: &str = r#"
    // Park the playhead away from zero first. A press the grip shared would
    // seek the transport to the pointer — which sits on the panel's very edge
    // for the whole gesture — so with the playhead already there the leak has
    // nothing to say. Parked at 200 it says it in one number.
    const wave = app.snapshot().find({ role: "card", label: "Waveform" });
    // A press-and-release at a point: `click` only addresses nodes, and the
    // ruler is a band rather than a control.
    app.drag(atRuler(wave.bounds.x + 200), { dx: 0, dy: 0 }, { steps: 1 });
    app.frames(2);
    const before = read();
    app.drag(atRuler(overPanel()), { dx: -120, dy: 0 }, { steps: 10 });
    app.frames(2);
    const after = read();
    ({ before, after })
"#;

/// The mirror: a scrub that crosses the seam scrubs, and resizes nothing.
const SCRUB_SCRIPT: &str = r#"
    const before = read();
    // Start well inside the ruler and drag *through* the seam, ending on the
    // far side of it. On the way it crosses the grip's whole width.
    const wave = app.snapshot().find({ role: "card", label: "Waveform" });
    app.drag(atRuler(wave.bounds.x + 160), { dx: -150, dy: 0 }, { steps: 10 });
    app.frames(2);
    const after = read();
    ({ before, after })
"#;

fn run(name: &'static str, script: &str) -> Value {
    let mut harness = harness(name);
    let result = harness.exec(
        &support::script(&format!("{READ}\n{script}")),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

fn num(value: &Value, phase: &str, field: &str) -> f64 {
    value[phase][field]
        .as_f64()
        .unwrap_or_else(|| panic!("no {phase}.{field} in {value}"))
}

#[test]
fn a_seam_drag_moves_the_seam_and_nothing_under_it() {
    let out = run("pointer-seam", SEAM_SCRIPT);
    let seam_before = num(&out, "before", "seam");
    let seam_after = num(&out, "after", "seam");
    let tab_before = num(&out, "before", "tab");
    let tab_after = num(&out, "after", "tab");
    let travelled = seam_before - seam_after;
    assert!(
        (100.0..=125.0).contains(&travelled),
        "the seam should have followed the pointer 120px left: {seam_before} → {seam_after}"
    );
    assert!(
        tab_after > tab_before + 100.0,
        "the panel should have widened with it: {tab_before} → {tab_after}"
    );
    // The point of the test: the same press must not have reached the ruler
    // the grip overhangs.
    assert_eq!(
        num(&out, "before", "playhead"),
        num(&out, "after", "playhead"),
        "dragging the seam seeked the transport — the grip is sharing its press"
    );
}

#[test]
fn a_scrub_that_crosses_the_seam_scrubs_and_resizes_nothing() {
    let out = run("pointer-scrub", SCRUB_SCRIPT);
    let landed = num(&out, "after", "playhead");
    assert!(
        (0.0..=30.0).contains(&landed) && landed != num(&out, "before", "playhead"),
        "the scrub should have left the playhead where it ended, ~10px in: {landed}"
    );
    assert_eq!(
        num(&out, "before", "seam"),
        num(&out, "after", "seam"),
        "a scrub that crossed the seam resized the panel"
    );
}
