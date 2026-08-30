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
//! One test to a fixture: each carries its own library directory on the pump
//! thread's [`luma_ui::runtime::Runtime`], so several coexist in this binary.
//! What still does not coexist is a process-wide cache in the Luma lib keyed on
//! something two fixtures share — see [`support::Fixture::track_hash`].
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::Fixture;

/// A venue with a rig in it. The 3D view is opened over the track browser,
/// which is the screen that knows a venue and no score.
fn harness(name: &'static str) -> Harness {
    let fixture = Fixture::new(name, 20, Vec::new()).with_rig();
    // The recorded numbers below, and the table in
    // `docs/design/presentation-seam.md`, are at two sizes: the default window
    // and a full screen. Presentation cost is pixel-linear, so a seam change
    // that looks free in a small pane need not be, and one size alone would
    // hide that. `LUMA_BUDGET_WINDOW=2560x1440` reproduces the larger row.
    let sized = match std::env::var("LUMA_BUDGET_WINDOW") {
        Ok(spec) => {
            let (width, height) = spec
                .split_once('x')
                .expect("LUMA_BUDGET_WINDOW is WIDTHxHEIGHT");
            fixture.window(
                width.parse().expect("window width"),
                height.parse().expect("window height"),
            )
        }
        Err(_) => fixture,
    };
    sized.open(Mode::Pixel)
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
        &support::script(
            r#"
            // The stage is a view of the tab below it, so a venue-naming
            // tab is what puts one on screen. The patch names a room and
            // no score, which is the unlit rig this test wants.
            nav.patch("Test Venue");
            app.frames(4, { waitMs: 60 });
            nav.expand();
            app.frames(8, { waitMs: 60 });
        "#,
        ),
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
/// Release, on an M-series retina window at 1200x800 with four movers, a frame
/// of this orbit is ~6.9 ms median / ~7.6 ms p95, and ~8.9 / ~12.0 under a
/// steeper two-axis drag — inside the 16.7 ms frame either way, with room for a
/// rig several times this size.
///
/// It was ~27 ms, and the readback was blamed. The readback was not the cost:
/// three things were, in order — a full-screen haze march at native resolution
/// (`LIVE_HAZE_RESOLUTION`), every base-colour texture re-uploaded with a fresh
/// CPU-built mip chain each frame (the renderer's material cache), and a
/// byte-at-a-time RGBA→BGRA swizzle over the whole surface (now the output
/// texture's own format). The actual copy out of the mapped buffer is ~0.1 ms
/// per megapixel, which is what the v2 zero-copy path stands to delete.
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
                // A frame cost means nothing without the pixel count it was
                // paid for: presentation is pixel-linear, so a number recorded
                // without its viewport size cannot be compared to anything.
                stageWidth: Math.round(stage.bounds.width),
                stageHeight: Math.round(stage.bounds.height),
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
