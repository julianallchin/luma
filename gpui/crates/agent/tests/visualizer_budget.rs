//! What a frame of continuous orbiting costs the 3D stage view.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test visualizer_budget
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
    // Generous, because a debug-build frame here is a full wgpu submit and a
    // readback and the measurement is deliberately a hundred of them.
    let result = harness.exec(code, Duration::from_secs(600));
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
        r#"
            app.frames(4, { waitMs: 60 });
            const venue = app.snapshot().find({ role: "card" });
            if (!venue) { throw new Error("no venue card on the welcome screen"); }
            app.click(venue, { restale: "match" });
            app.frames(4, { waitMs: 60 });
            app.action("luma::OpenVisualizer");
            app.frames(8, { waitMs: 60 });
        "#,
    );
}

/// What a frame of continuous orbiting costs on the CPU.
///
/// Not an assertion about a budget — `app.timings` measures gpui's scene build
/// and this element's prepaint, which is where the GPU submit and the readback
/// block, but not the GPU's own time (see `api.d.ts`). It is a recorded number
/// that will move when the presentation seam changes, which is the point:
/// spec §7.4 budgets the readback at 3 ms and nothing measured it before.
///
/// Optimised, on an M-series retina window, a frame of this is ~22 ms median /
/// ~26 ms p95 — over the 16 ms the spec wants, and dominated by the readback
/// and the BGRA copy of a full-resolution surface, which is exactly the cost
/// the v2 zero-copy path exists to delete.
#[test]
fn orbiting_reports_its_frame_cost() {
    let mut harness = harness("visualizer-budget");
    open_viewport(&mut harness);

    let report = run(
        &mut harness,
        r#"
            const snapshot = app.snapshot();
            const stage = snapshot.find({ role: "card", label: "Stage" });
            if (!stage) { throw new Error("the viewport is not on screen"); }
            const from = snapshot.frame;
            // 120 settled frames of continuous orbit — two seconds of it, if a
            // frame cost what the budget says. Broken into strokes because one
            // acting call must answer inside the harness's call timeout, and a
            // debug-build frame here is nowhere near 16 ms; the camera is left
            // where each stroke ended, so the sequence is still one orbit.
            for (let stroke = 0; stroke < 6; stroke += 1) {
                app.drag(stage, { dx: 40, dy: 0 }, { steps: 20, restale: "match" });
            }
            const frames = app.timings().frames.filter((f) => f.frame >= from);
            const draw = frames.map((f) => f.drawMs).sort((a, b) => a - b);
            ({
                frames: draw.length,
                medianDrawMs: draw[Math.floor(draw.length / 2)],
                p95DrawMs: draw[Math.floor(draw.length * 0.95)],
                maxDrawMs: draw[draw.length - 1],
                totalMs: frames.reduce((s, f) => s + f.drawMs + f.parkedMs, 0),
            })
        "#,
    );

    let frames = report["frames"].as_u64().expect("frames were timed");
    assert!(
        frames >= 60,
        "only {frames} frames were timed while orbiting"
    );
    println!("orbit frame cost: {report}");

    // The hang detector, scaled to the run's own median rather than to a
    // wall-clock number. An optimised frame here costs ~22 ms and an
    // unoptimised one ~450 ms, so any absolute threshold is either unreachable
    // in debug or useless in release; what is build-independent is that a
    // steady orbit has no frame an order of magnitude off its neighbours.
    let median = report["medianDrawMs"].as_f64().unwrap_or_default();
    let max = report["maxDrawMs"].as_f64().unwrap_or(f64::MAX);
    assert!(
        max < median * 10.0,
        "one frame stalled far past the rest of the orbit: {report}"
    );
}
