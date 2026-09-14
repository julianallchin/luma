//! The stage's pointer plane belongs to whatever is drawn on top of it.
//!
//! Two gestures that used to reach the camera through something else:
//!
//! - dragging the stage/editor seam **orbited**, because the grip is 5px wide
//!   over a 1px rule and gpui reports every hitbox under the pointer, so the
//!   press landed on the seam and on the viewport at once;
//! - turning the wheel over the floating settings card **dollied**, for the
//!   same reason — `should_handle_scroll` asks whether the viewport is under
//!   the pointer, not whether it is the surface the wheel was aimed at. (That
//!   card used to be the renderer lab; the lab is gone and the View settings
//!   popover floats over the same viewport in the same way.)
//!
//! Pixel-only because the stage's pointer handlers exist only where there is a
//! renderer: headless resolves `stage_gpu` to false and the pane draws a plate
//! with nothing to press. The assertions are still readings rather than
//! screenshots — the `CAMERA` node is the orbit's three numbers, which is the
//! fact under test, where a frame diff would also carry everything else the
//! scene does on its own.
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;
use support::Fixture;

fn harness(name: &'static str) -> Harness {
    Fixture::new(name, 20, Vec::new())
        .with_rig()
        .open(Mode::Pixel)
}

/// Open the stage over a venue tab and hold onto the camera's reading.
///
/// `restale: "match"` throughout: the viewport asks for an animation frame at
/// the top of every render, so a frame is always one behind by the time a
/// script acts on it — see `visualizer.rs`.
const OPEN: &str = r#"
    nav.patch("Test Venue");
    app.frames(6, { waitMs: 60 });

    function camera() {
        const node = app.snapshot().findAll({ role: "text" })
            .find((n) => n.label.startsWith("CAMERA "));
        return node === undefined ? null : node.label;
    }
    function stage() {
        return app.snapshot().find({ role: "card", label: "Stage" }).bounds;
    }
    until("the stage's camera", () => camera() !== null);
"#;

/// Drag the seam between the stage and the editor. It resizes; it does not orbit.
const SEAM_SCRIPT: &str = r#"
    const before = { camera: camera(), stage: stage() };
    // The grip's *leading* pixel, not its centre: the strip that overhangs
    // the stage is the half of it a shared press would land in, and a press at
    // the centre would sit below the viewport's last row and prove nothing.
    const grip = app.snapshot().find({ role: "slider", label: "Stage height" }).bounds;
    app.drag({ x: grip.x + grip.width / 2, y: grip.y + 1 },
             { dx: 0, dy: -90 }, { steps: 10, restale: "match" });
    app.frames(4, { waitMs: 60 });
    ({ before, after: { camera: camera(), stage: stage() } })
"#;

/// Turn the wheel over the floating View settings card. The camera stays.
///
/// Then turn the same wheel over bare stage, where it *is* the camera's. That
/// second half is the control: without it a wheel the harness never delivered
/// would pass the first assertion as loudly as an occluder that works. The card
/// itself has nothing to scroll — it is a fixed-height popover — so "the panel
/// swallowed the wheel" has to be shown by the panel-less case moving.
const VIEW_SCRIPT: &str = r#"
    nav.step("view settings", "toggle", "Render settings");
    const card = until("the view settings card", (s) =>
        s.find({ role: "card", label: "Render settings" }))
        .find({ role: "card", label: "Render settings" }).bounds;
    // The card's own legend row, not one of its controls: a scrub under the
    // pointer would take the wheel as a value change and prove nothing about
    // the surface.
    const legend = { x: card.x + card.width / 2, y: card.y + 12 };
    const before = camera();
    app.scroll(legend, { dy: -160, steps: 8, restale: "match" });
    app.frames(4, { waitMs: 60 });
    const overCard = camera();
    // Left of the card, which hangs off the stage's bottom-right corner and
    // covers its centre.
    const bounds = stage();
    const bare = { x: (bounds.x + card.x) / 2, y: bounds.y + bounds.height / 2 };
    if (bare.x >= card.x) { throw new Error("the card leaves no bare stage to aim at"); }
    app.scroll(bare, { dy: -160, steps: 8, restale: "match" });
    app.frames(4, { waitMs: 60 });
    ({ before, overCard, overStage: camera() })
"#;

fn run(name: &'static str, script: &str) -> Value {
    let mut harness = harness(name);
    let opened = harness.exec(&support::script(OPEN), GPU_LIVENESS_TIMEOUT);
    assert_eq!(opened.error, None, "opening failed:\n{}", opened.stdout);
    let result = harness.exec(script, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

#[test]
fn a_seam_drag_resizes_the_stage_and_does_not_orbit_it() {
    let out = run("visualizer-pointer-seam", SEAM_SCRIPT);
    let (before, after) = (&out["before"], &out["after"]);
    let (was, now) = (
        before["stage"]["height"].as_f64().unwrap(),
        after["stage"]["height"].as_f64().unwrap(),
    );
    assert!(
        (was - now - 90.0).abs() < 8.0,
        "the seam should have taken 90px off the stage: {was} → {now}"
    );
    assert_eq!(
        before["camera"], after["camera"],
        "dragging the seam turned the camera — the grip is sharing its press"
    );
}

#[test]
fn a_wheel_over_the_view_settings_does_not_dolly() {
    let out = run("visualizer-pointer-view", VIEW_SCRIPT);
    assert_eq!(
        out["before"], out["overCard"],
        "a wheel over the View settings dollied the camera — the panel is sharing its scroll"
    );
    assert_ne!(
        out["before"], out["overStage"],
        "a wheel over the bare stage did not dolly — the wheel never reached the viewport, \
         so the panel proved nothing"
    );
}
