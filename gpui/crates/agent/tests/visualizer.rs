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
//! One test to a binary, because [`support::Fixture`] seeds through
//! `LUMA_CONFIG_DIR` and that is process-global — two fixtures in one process
//! are one library with both their contents, racing on the same file.
#![cfg(feature = "pixel")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
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
    let result = harness.exec(code, Duration::from_secs(60));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// Open the venue, then the 3D view over it, and settle enough frames for the
/// rig load and the first GPU frame to land.
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
            nav.venue("Test Venue");
            app.frames(4, { waitMs: 60 });
            app.action("luma::OpenVisualizer");
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

fn differing_fraction(a: &image::RgbaImage, b: &image::RgbaImage) -> f32 {
    assert_eq!(a.dimensions(), b.dimensions(), "shots are different sizes");
    let differing = a
        .pixels()
        .zip(b.pixels())
        .filter(|(p, q)| (0..3).any(|c| i32::from(p[c]).abs_diff(i32::from(q[c])) > 8))
        .count();
    differing as f32 / (a.width() * a.height()) as f32
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
            for (const label of ["Sun azimuth", "Sun elevation", "Sun intensity", "Ambient intensity", "Haze density"]) {
                if (!snapshot.find({ role: "slider", label })) {
                    throw new Error(`renderer lab is missing ${label}`);
                }
            }
            for (const label of ["Sun", "Sun shadows", "Environment", "Fixture haze", "Editor grid"]) {
                if (!snapshot.find({ role: "checkbox", label })) {
                    throw new Error(`renderer lab is missing ${label}`);
                }
            }
            if (!snapshot.find({ role: "text", label: "Renderer draw timing" })) {
                throw new Error("renderer lab is missing draw timing");
            }
            const sun = snapshot.find({ role: "checkbox", label: "Sun" });
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
