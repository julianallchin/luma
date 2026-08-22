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

/// The gate: the viewport draws something, and it draws something *different*
/// when the camera moves.
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
