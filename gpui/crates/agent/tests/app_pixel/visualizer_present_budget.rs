//! What *publishing* a stage frame costs the UI thread.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --release --test visualizer_present_budget -- --nocapture
//! ```
//!
//! `visualizer_budget` measures a camera drag, which is the right shape for
//! asking what an interaction costs and the wrong shape for asking what the
//! presentation seam costs. A drag settles far faster than the renderer
//! completes frames, so most of its settles repaint the frame they already had
//! — and repainting a frame you already published is free on every path, which
//! is why that test's median is flat against a five-fold change in viewport
//! area.
//!
//! This one paces settles so that a new frame lands for nearly every one, which
//! is the only condition under which the publish shows up in `drawMs` at all.
//! `LUMA_WITHHOLD_SHARED_SURFACES=1` runs the same measurement over the CPU
//! readback path; `LUMA_PRESENT_WINDOW=2560x1440` runs it full-screen.
//!
//! One test to a fixture: each carries its own library directory on the pump
//! thread's [`luma_ui::runtime::Runtime`], so several coexist in this binary.
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::Fixture;

fn harness() -> Harness {
    let fixture = Fixture::new("visualizer-present-budget", 20, Vec::new()).with_rig();
    let sized = match std::env::var("LUMA_PRESENT_WINDOW") {
        Ok(spec) => {
            let (width, height) = spec
                .split_once('x')
                .expect("LUMA_PRESENT_WINDOW is WIDTHxHEIGHT");
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
    let result = harness.exec(code, Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// The publish cost of a stage frame, at whatever size the window puts it.
///
/// Reported, not asserted, for the same reason `visualizer_budget` reports:
/// the absolute number is build- and machine-dependent, and the comparison
/// that matters is between two runs of this binary with the seam switched.
///
/// What is being compared is real but small. Both halves of the old round trip
/// were pixel-linear, and only one of them was ever on this thread: the atlas
/// upload, inside `Window::draw`. The readback and its row copy are on the
/// renderer's own thread and never appeared in this number — deleting them
/// shows up as renderer latency, not as UI-thread time.
#[test]
fn publishing_a_frame_reports_its_cost() {
    let mut harness = harness();
    run(
        &mut harness,
        &support::script(
            r#"
            nav.universe("Test Venue");
            app.frames(4, { waitMs: 60 });
            nav.expand();
            app.frames(8, { waitMs: 60 });
        "#,
        ),
    );

    let report = run(
        &mut harness,
        r#"
            const snapshot = app.snapshot();
            const stage = snapshot.find({ role: "card", label: "Stage" });
            if (!stage) { throw new Error("the viewport is not on screen"); }
            const from = snapshot.frame;
            // 20 ms between settles is several times a release frame, so the
            // renderer has completed and the viewport has something new to
            // publish nearly every time. That is the whole design of this
            // measurement: an unpaced loop measures repaints, not publishes.
            app.frames(120, { waitMs: 20 });
            const frames = app.timings().frames.filter((f) => f.frame >= from);
            const draw = frames.map((f) => f.drawMs).sort((a, b) => a - b);
            ({
                stageWidth: Math.round(stage.bounds.width),
                stageHeight: Math.round(stage.bounds.height),
                frames: draw.length,
                medianDrawMs: draw[Math.floor(draw.length / 2)],
                p95DrawMs: draw[Math.floor(draw.length * 0.95)],
                maxDrawMs: draw[draw.length - 1],
            })
        "#,
    );

    let frames = report["frames"].as_u64().expect("frames were timed");
    assert!(frames >= 60, "only {frames} frames were timed");
    println!("present cost: {report}");

    let median = report["medianDrawMs"].as_f64().unwrap_or_default();
    let max = report["maxDrawMs"].as_f64().unwrap_or(f64::MAX);
    assert!(
        max < median * 20.0,
        "one frame stalled far past the rest: {report}"
    );
}
