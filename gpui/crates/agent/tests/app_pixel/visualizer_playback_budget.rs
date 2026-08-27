//! What a frame of the stage costs the UI thread *while a track is playing*.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --release --test visualizer_playback_budget
//! ```
//!
//! `visualizer_budget.rs` measures a camera orbit over a still rig. Playback is
//! a different path and a heavier one: the score is evaluated on the UI thread
//! once per frame ([`Library::sample_universe`]), every fixture's pose changes,
//! and so the renderer's cluster grid and every fixture shadow map are dirty on
//! every frame rather than occasionally.
//!
//! The distinction this test exists to draw is **stall versus slowdown**. A
//! renderer that cannot keep up shows as a low, *even* frame rate: the worker
//! thread is behind, the UI thread still answers. A UI-thread stall shows as a
//! spiky one — a median inside the budget with a maximum many times it. Only
//! the second reads as a freeze, and only the second is fixed on this side of
//! the presentation seam.
//!
//! Pixel-only, and one test to a binary, for the reasons `visualizer.rs` gives.
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

/// Long enough that the measured window is entirely inside the clip: a frame
/// sampled past the end is an unlit frame, and an unlit frame is cheap for the
/// wrong reason.
const SECONDS: u32 = 30;

/// A venue-sized rig, not the four movers the other stage tests use. Every
/// renderer cost this test is about — cluster occupancy, fixture shadow passes,
/// draw count, and the score's own per-primitive work — scales on this number,
/// and four of anything measures none of them.
const RIG: usize = 120;

/// Overlapping lit clips, all spanning the track.
///
/// `Scene::composite` walks every annotation whose span contains the playhead
/// and composites it, synchronously on the UI thread, so this is the axis the
/// score costs on. One clip — what every other stage test uses — measures the
/// cheapest possible score and tells you nothing about a real track.
const CLIPS: usize = 12;

fn harness() -> Harness {
    Fixture::new(
        "visualizer-playback-budget",
        SECONDS,
        (0..CLIPS)
            .map(|lane| {
                Clip::new(
                    format!("pattern-pulse-{lane}"),
                    format!("Pulse {lane}"),
                    0.,
                    f64::from(SECONDS),
                )
                .lit()
                .lane(lane as i64)
            })
            .collect(),
    )
    .with_rig_of(RIG)
    .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// A stage with a score on it, playing.
fn open_playing_stage(harness: &mut Harness) {
    run(
        harness,
        &format!(
            r#"
            {NAV}
            // Only the track editor names a `(track, venue)`, and only a
            // `(track, venue)` puts a *lit* stage on screen.
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse 0" }}) !== undefined);
            nav.expand();
            app.frames(10, {{ waitMs: 60 }});
            const readout = (s) =>
                s.find((n) => n.role === "text" && n.label.includes("FIXTURES"));
            const shot = until("the rig's readout", (s) => readout(s) !== undefined);
            if (!readout(shot).label.includes("LIVE")) {{
                throw new Error(`the stage is not lit: ${{readout(shot).label}}`);
            }}
            nav.step("the Play button", "button", "Play");
            // Settle past the first frames, which carry the score's cold caches.
            app.frames(20, {{ waitMs: 55 }});
        "#
        ),
    );
}

#[test]
fn playing_reports_its_frame_cost() {
    let mut harness = harness();
    open_playing_stage(&mut harness);

    let report = run(
        &mut harness,
        r#"
            const from = app.snapshot().frame;
            // Two hundred settled frames of playback with nobody touching the
            // camera: whatever this costs is what the score and the renderer
            // cost, with no input work mixed in.
            app.frames(200, { waitMs: 16 });
            const frames = app.timings().frames.filter((f) => f.frame >= from);
            const draw = frames.map((f) => f.drawMs).sort((a, b) => a - b);
            const at = (q) => draw[Math.min(draw.length - 1, Math.floor(draw.length * q))];
            ({
                frames: draw.length,
                medianDrawMs: at(0.5),
                p95DrawMs: at(0.95),
                p99DrawMs: at(0.99),
                maxDrawMs: draw[draw.length - 1],
                // How many frames cost more than four times the median. A
                // renderer that is merely slow has none of these; a UI thread
                // that stalls has a handful, and they are what a freeze is.
                spikes: draw.filter((ms) => ms > at(0.5) * 4).length,
                // Proof the measurement is of the rig it claims: a fixture
                // count that silently fell back to a handful would make every
                // number here meaningless and every assertion pass.
                readout: (app.snapshot().find((n) => n.role === "text" && n.label.includes("FIXTURES")) || {}).label,
            })
        "#,
    );

    let frames = report["frames"].as_u64().expect("frames were timed");
    assert!(frames >= 100, "only {frames} frames were timed: {report}");
    let readout = report["readout"].as_str().unwrap_or_default();
    assert!(
        readout.starts_with(&format!("{RIG} FIXTURES")) && readout.contains("LIVE"),
        "the stage under measurement is not the lit {RIG}-fixture rig: {readout:?}"
    );
    println!("playback frame cost: {report}");

    // Scaled to the run's own median, like `visualizer_budget`'s, because a
    // debug frame and a release frame here differ by more than an order of
    // magnitude and no absolute number covers both.
    let median = report["medianDrawMs"].as_f64().unwrap_or_default();
    let max = report["maxDrawMs"].as_f64().unwrap_or(f64::MAX);
    assert!(
        max < median * 10.0,
        "a frame of playback stalled far past its neighbours: {report}"
    );
}
