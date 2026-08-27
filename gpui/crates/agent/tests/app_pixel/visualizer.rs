//! The 3D stage view draws, and redraws when the camera moves.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test visualizer
//! ```
//!
//! Pixel-only for the same reason `pixel.rs` is — headless mode has no
//! renderer, so `app.screenshot()` throws — and doubly so here: the thing under
//! test *is* a picture. A viewport that painted nothing, or painted the same
//! thing whatever the camera did, would pass every node-tree assertion in the
//! suite and still be entirely broken.
//!
//! One test to a fixture: each carries its own library directory on the pump
//! thread's [`luma_ui::runtime::Runtime`], so several coexist in this binary.
//! What still does not coexist is a process-wide cache in the Luma lib keyed on
//! something two fixtures share — see [`support::Fixture::track_hash`].
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;
use support::Fixture;

/// A venue with a rig in it. The 3D view is opened over the track browser,
/// which is the screen that knows a venue and no score.
fn harness(name: &'static str) -> Harness {
    Fixture::new(name, 20, Vec::new())
        .with_rig()
        .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// Open a venue-naming tab, which is what raises the stage over it, and settle
/// enough frames for the rig load and the first GPU frame to land.
///
/// Every acting call carries `restale: "match"`: the viewport asks for an
/// animation frame at the top of every render, so a frame is *always* one
/// behind by the time a script acts on it. That is the redraw working, not a
/// stale click.
fn open_viewport(harness: &mut Harness) {
    run(
        harness,
        &support::script(
            r#"
            // The stage is a view of the tab below it, so a venue-naming
            // tab is what puts one on screen. The patch names a room and
            // no score, which is the unlit rig this test wants.
            nav.universe("Test Venue");
            app.frames(4, { waitMs: 60 });
            nav.expand();
            app.frames(8, { waitMs: 60 });
        "#,
        ),
    );
}

/// Mean luminance of a PNG, and the fraction of pixels that differ from
/// another shot by more than a threshold. Reading the file rather than trusting
/// the harness is the point: this asserts on what was drawn.
fn pixels(path: &str) -> image::RgbaImage {
    image::open(path)
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
        .to_rgba8()
}

fn mean_luma(image: &image::RgbaImage) -> f32 {
    let total: f64 = image
        .pixels()
        .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
        .sum();
    (total / (3.0 * f64::from(image.width() * image.height()))) as f32
}

/// The shared diff at the shared noise floor — see `support::image`.
fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage) -> f32 {
    support::image::differing_fraction(left, right, support::image::CHANNEL_NOISE)
}

/// The gate: the viewport draws something, its independent sun control changes
/// the lighting, and orbiting changes the camera view.
///
/// Both halves matter and neither implies the other. A viewport wired to a
/// stale image passes "non-black" forever; one that renders a fresh frame with
/// the camera ignored passes "not blank" and fails here.
#[test]
fn orbiting_changes_what_is_drawn() {
    let mut harness = harness("visualizer");
    open_viewport(&mut harness);

    let before = run(&mut harness, "app.screenshot()");
    let before = pixels(before["path"].as_str().expect("a screenshot has a path"));
    assert!(
        mean_luma(&before) > 1.0,
        "the viewport drew a black frame (mean luma {})",
        mean_luma(&before)
    );

    run(
        &mut harness,
        r#"
            const lab = app.snapshot().find({ role: "toggle", label: "Open Renderer Lab" });
            if (!lab) { throw new Error("the renderer lab trigger is not on screen"); }
            app.click(lab, { restale: "match" });
            app.frames(2);
            const snapshot = app.snapshot();
            for (const label of [
                "Sun azimuth", "Sun elevation", "Sun intensity",
                "Sun color red", "Sun color green", "Sun color blue",
                "Background red", "Background green", "Background blue",
                "Ambient color red", "Ambient color green", "Ambient color blue",
                "Ambient intensity", "Haze density", "Haze steps", "Haze resolution"
            ]) {
                if (!snapshot.find({ role: "slider", label })) {
                    throw new Error(`renderer lab is missing ${label}`);
                }
            }
            for (const label of ["Sun", "Sun shadows", "Environment", "Fixture haze", "Editor grid"]) {
                if (!snapshot.find({ role: "checkbox", label })) {
                    throw new Error(`renderer lab is missing ${label}`);
                }
            }
            // The timing readouts live in the frame-stats panel, unfolded.
            // The reading is the label, so this matches its shape rather than
            // a fixed name — a node named "Renderer CPU and GPU timing" would
            // satisfy an assertion like this while publishing no numbers.
            // `^CPU` and not the full split, because "CPU/GPU timing
            // unavailable" is the honest reading on a frame the renderer has
            // not timed yet and is still this node doing its job.
            const stats = snapshot.find({ role: "toggle", label: "Frame stats" });
            if (!stats) { throw new Error("the stage has no frame-stats panel"); }
            app.click(stats, { restale: "match" });
            app.frames(2);
            const unfolded = app.snapshot();
            if (!unfolded.find((n) => n.role === "text" && /^CPU/.test(n.label))) {
                throw new Error("frame stats are missing separate CPU/GPU timing");
            }
            app.click(unfolded.find({ role: "toggle", label: "Frame stats" }), { restale: "match" });
            app.frames(2);
            const sun = app.snapshot().find({ role: "checkbox", label: "Sun" });
            if (!sun) { throw new Error("the sun control is not in the renderer lab"); }
            app.click(sun, { restale: "match" });
            app.frames(2);
            const close = app.snapshot().find({ role: "toggle", label: "Close Renderer Lab" });
            app.click(close, { restale: "match" });
            app.frames(4, { waitMs: 30 });
        "#,
    );
    let sunless = run(&mut harness, "app.screenshot()");
    let sunless = pixels(sunless["path"].as_str().expect("a screenshot has a path"));
    assert!(
        mean_luma(&sunless) < mean_luma(&before),
        "turning the sun off did not lower mean luminance: {:.2} -> {:.2}",
        mean_luma(&before),
        mean_luma(&sunless)
    );
    assert!(
        differing_fraction(&before, &sunless) > 0.005,
        "turning the sun off did not materially change the rendered frame"
    );

    run(
        &mut harness,
        r#"
            app.click(app.snapshot().find({ role: "toggle", label: "Open Renderer Lab" }), { restale: "match" });
            app.frames(2);
            const restoredSun = app.snapshot().find({ role: "checkbox", label: "Sun" });
            if (!restoredSun) { throw new Error("the renderer lab did not preserve the sun-off state"); }
            app.click(restoredSun, { restale: "match" });
            app.frames(2);
            app.click(app.snapshot().find({ role: "toggle", label: "Close Renderer Lab" }), { restale: "match" });
            app.frames(4, { waitMs: 30 });
        "#,
    );
    let restored = run(&mut harness, "app.screenshot()");
    let restored = pixels(restored["path"].as_str().expect("a screenshot has a path"));
    assert!(
        (mean_luma(&restored) - mean_luma(&before)).abs() < 0.5,
        "restoring the sun did not restore frame energy: {:.2} -> {:.2}",
        mean_luma(&before),
        mean_luma(&restored)
    );
    assert!(
        differing_fraction(&before, &restored) < 0.005,
        "restoring the sun did not restore the initial image"
    );

    // Left-drag on the viewport is orbit. The drag starts at the centre of
    // the "Stage" node, which is the viewport's own bounds.
    run(
        &mut harness,
        r#"
            const stage = app.snapshot().find({ role: "card", label: "Stage" });
            if (!stage) { throw new Error("the viewport is not on screen"); }
            app.drag(stage, { dx: 220, dy: 60 }, { steps: 20, restale: "match" });
            app.frames(4, { waitMs: 16 });
        "#,
    );

    let after = run(&mut harness, "app.screenshot()");
    let after = pixels(after["path"].as_str().expect("a screenshot has a path"));
    let moved = differing_fraction(&before, &after);
    assert!(
        moved > 0.02,
        "orbiting changed only {:.3}% of the frame",
        moved * 100.0
    );
}

/// The idle gate: a still stage stops submitting frames once the temporal
/// haze has settled — the FPS readout says `IDLE` — and a camera drag wakes
/// it. Without the gate a still stage re-marches the haze at display rate for
/// nobody, which is a spinning fan on any laptop.
#[test]
fn a_still_stage_rests_and_a_drag_wakes_it() {
    let mut harness = harness("visualizer-idle");
    open_viewport(&mut harness);

    let rested = run(
        &mut harness,
        r#"
        (() => {
            const idle = () => app.snapshot().find(
                (n) => n.role === "text" && n.label === "FPS IDLE");
            for (let i = 0; i < 120; i++) {
                if (idle()) return "rested";
                app.frames(1, { waitMs: 16 });
            }
            return "never rested";
        })()
        "#,
    );
    assert_eq!(
        rested.as_str(),
        Some("rested"),
        "a still, paused stage kept rendering"
    );

    let woke = run(
        &mut harness,
        r#"
        (() => {
            const stage = app.snapshot().find({ role: "card", label: "Stage" });
            if (!stage) { throw new Error("the viewport is not on screen"); }
            app.drag(stage, { dx: 80, dy: 0 }, { steps: 5, restale: "match" });
            app.frames(3, { waitMs: 16 });
            const idle = app.snapshot().find(
                (n) => n.role === "text" && n.label === "FPS IDLE");
            return idle ? "still idle" : "woke";
        })()
        "#,
    );
    assert_eq!(
        woke.as_str(),
        Some("woke"),
        "a camera drag did not wake the resting stage"
    );
}
