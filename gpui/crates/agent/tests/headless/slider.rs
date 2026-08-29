//! The value slider, dragged.
//!
//! Every slider in the app is one `luma_ui::luma_slider`, and for a long time
//! that function painted a value without accepting one: the web control's
//! interaction lived in an invisible range `<input>` that was never ported, so
//! a user who dragged any slider anywhere got nothing. This drags one for
//! real, through the same pointer path a person uses, and asserts the number
//! moved.
//!
//! Art-Net's Max Brightness is the subject because it is the slider that
//! reaches furthest: the drag has to travel through the settings write seam
//! and come back out of a fresh `get_settings`. A test on a slider whose value
//! lives in a struct in the view would pass on a control wired to nothing.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;

fn harness() -> Harness {
    support::Fixture::new("slider-drag", 1, vec![])
        .without_track()
        .open(Mode::Headless)
}

/// Open Art-Net settings and read the slider plus the number it draws.
///
/// The reading is a node of its own (`"<id> = <value>"`) rather than part of
/// the slider's label: the slider is addressed by name, and a name that
/// carried the value would change under every drag.
const SCRIPT: &str = r#"
    nav.venue("Test Venue");

    function openArtnet() {
        nav.settings();
        nav.step("the Art-Net settings section", "toggle", "Art-Net / DMX");
        return until("the loaded Art-Net settings", (s) =>
            s.find(isMaxBrightness) !== undefined ? s : undefined);
    }

    // Addressed by a predicate rather than by an exact label: the slider's
    // name carries its current reading, so an equality match would have to
    // know the answer before asking the question.
    function isMaxBrightness(n) {
        return n.role === "slider" && n.label.indexOf("Max Brightness") === 0;
    }

    function reading(shot) {
        const from = shot === undefined ? app.snapshot() : shot;
        const node = from.find((n) =>
            n.role === "text" && n.label.indexOf("max_dimmer = ") === 0);
        return node === undefined ? null : Number(node.label.split(" = ")[1]);
    }

    const opened = openArtnet();
    const slider = opened.find(isMaxBrightness);
    const before = reading(opened);

    // From the middle of the slab, a third of its width to the left. The
    // mapping is absolute — the value follows the pointer's position in the
    // box, which is what the fill bar draws — so the pointer's final position
    // says what the value must be, and nowhere near where it started.
    const width = slider.bounds.width;
    const dx = Math.round(width / 3);
    const expected = Math.round(100 * (0.5 - dx / width));
    app.drag(slider, { dx: -dx, dy: 0 }, { steps: 8 });

    // Waiting for the value the pointer *ended* on, not merely for a change:
    // a drag is many moves and each one writes, so "it is different now" can
    // be satisfied by a value from halfway along the path. Landing on the end
    // point is the assertion — `until` throws with the tree if it never does.
    // One unit of slack for the rounding between pixels and percent.
    const settled = until(`Max Brightness at ${expected}`, (s) => {
        const now = reading(s);
        return now !== null && Math.abs(now - expected) <= 1 ? s : undefined;
    });
    const dragged = reading(settled);

    // Leave and come back. This read is served by a fresh `get_settings`, so
    // it can only say what the database says — a slider that moved its own
    // paint and nothing else fails here.
    nav.dismiss();
    const reopened = reading(openArtnet());

    ({ before, dragged, reopened, expected })
"#;

#[test]
fn dragging_a_slider_moves_its_value_and_writes_it_through() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(60));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let before = out["before"].as_f64().expect("a reading before the drag");
    let dragged = out["dragged"].as_f64().expect("a reading after the drag");
    // The schema default, and the reason the drag below travels left.
    assert_eq!(before, 100.0, "Max Brightness did not start at its default");
    assert!(
        dragged < before,
        "dragging left did not lower the value: {before} then {dragged}"
    );

    // Where the pointer actually ended, within a pixel's worth of value. The
    // script already waited for exactly this, so reaching here means it
    // arrived; asserting it again is what keeps the wait from being read as
    // decoration when someone edits the predicate.
    let expected = out["expected"].as_f64().expect("the drag's end point");
    assert!(
        (dragged - expected).abs() <= 1.0,
        "the value did not follow the pointer: wanted about {expected}, got {dragged}"
    );

    assert_eq!(
        out["reopened"].as_f64(),
        Some(dragged),
        "the dragged value did not survive a fresh read of the settings seam"
    );
}
