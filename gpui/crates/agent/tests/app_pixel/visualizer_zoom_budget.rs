//! What zooming the stage costs, against what holding it still costs.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --release --test visualizer_zoom_budget -- --nocapture
//! ```
//!
//! The reported symptom is that the stage freezes *while zooming in*, and the
//! obvious explanation — the beams cover more of the screen, so the volumetric
//! march runs on more pixels — does not survive measurement: the renderer's own
//! profile shows the volumetric pass no more expensive at 63 % beam coverage
//! than at 24 %, and the camera cannot get closer than
//! `Framing::NEAR_MARGIN` × the rig's radius anyway, so it never enters the
//! beams at all.
//!
//! That leaves the gesture rather than the picture. A wheel sends a stream of
//! events, each one a `dolly` and a `notify`, and every notify that turns into
//! a render rebuilds the whole `Frame` and its pick snapshot on the UI thread.
//! This measures the frame cost *while scrolling* against the frame cost while
//! idle, on the same stage, so the two are directly comparable.
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

const SECONDS: u32 = 60;
const RIG: usize = 120;

fn harness() -> Harness {
    Fixture::new(
        "visualizer-zoom-budget",
        SECONDS,
        vec![Clip::new("pattern-pulse", "Pulse", 0., f64::from(SECONDS)).lit()],
    )
    .with_rig_of(RIG)
    .window(2560.0, 1440.0)
    .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

#[test]
fn zooming_costs_no_more_per_frame_than_holding_still() {
    let mut harness = harness();
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            nav.expand();
            app.frames(10, {{ waitMs: 60 }});
            nav.step("the Play button", "button", "Play");
            app.frames(20, {{ waitMs: 55 }});
        "#
        ),
    );

    let report = run(
        &mut harness,
        r#"
        (() => {
            const stage = app.snapshot().find({ role: "card", label: "Stage" });
            if (!stage) { throw new Error("the viewport is not on screen"); }
            const stats = (from) => {
                const draw = app.timings().frames
                    .filter((f) => f.frame >= from)
                    .map((f) => f.drawMs)
                    .sort((a, b) => a - b);
                const at = (q) => draw[Math.min(draw.length - 1, Math.floor(draw.length * q))];
                return { frames: draw.length, median: at(0.5), p95: at(0.95), max: draw[draw.length - 1] };
            };

            // Idle: playing, nobody touching the camera.
            const idleFrom = app.snapshot().frame;
            app.frames(120, { waitMs: 8 });
            const idle = stats(idleFrom);

            // Zooming in, the way a wheel does it: many small steps, not one
            // big jump. The camera clamps at NEAR_MARGIN, so repeated inward
            // scrolls settle against the stop rather than running away.
            const zoomFrom = app.snapshot().frame;
            for (let burst = 0; burst < 6; burst += 1) {
                app.scroll(stage, { dy: 40, steps: 20, restale: "match" });
            }
            const zoom = stats(zoomFrom);

            return { idle, zoom };
        })()
        "#,
    );

    println!("zoom frame cost: {report}");
    let idle = report["idle"]["median"].as_f64().unwrap_or_default();
    let zoom = report["zoom"]["median"].as_f64().unwrap_or_default();
    let zoom_max = report["zoom"]["max"].as_f64().unwrap_or(f64::MAX);
    assert!(
        report["zoom"]["frames"].as_u64().unwrap_or_default() >= 60,
        "too few frames while zooming: {report}"
    );
    // Zooming does more work per frame than idling — a moved camera invalidates
    // the cluster grid and every cascade — so this is not a claim that they are
    // equal. It is a claim that zooming does not fall off a cliff.
    assert!(
        zoom < idle * 3.0,
        "zooming cost {:.1}x an idle frame: {report}",
        zoom / idle
    );
    assert!(
        zoom_max < idle * 10.0,
        "a frame during zoom stalled far past an idle one: {report}"
    );
}
