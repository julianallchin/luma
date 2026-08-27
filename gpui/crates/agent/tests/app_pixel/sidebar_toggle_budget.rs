//! What ⌘B costs the UI thread while the stage and an editor are live.
//!
//! ```sh
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel \
//!     --release --test app_pixel sidebar_toggle -- --nocapture
//! ```
//!
//! The reported symptom is that ⌘B is not 120 Hz with the visualizer open. It
//! is a layout change, so it is one of two costs and they need separating: the
//! *UI thread's* — the panel and its editor re-laid-out at a new width every
//! frame of the slide — and the *renderer's*, which is
//! `luma-render/tests/resize_probe.rs` and not visible here at all (`drawMs`
//! never sees the GPU; see `app.timings` in `api.d.ts`).
//!
//! So this measures the half the harness can see, against the same stage
//! holding still, and reports both. A toggle does strictly more work than an
//! idle frame — the whole window relays out — so the assertion is the one
//! `visualizer_zoom_budget` makes: not that they are equal, but that the
//! toggle does not fall off a cliff.

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
        "sidebar-toggle-budget",
        SECONDS,
        vec![Clip::new("pattern-pulse", "Pulse", 0., f64::from(SECONDS)).lit()],
    )
    .with_rig_of(RIG)
    // The whole subject is the *slide*. The suite snaps motion by default so a
    // walk can read final geometry the frame after it acts; here that would
    // turn ⌘B into a single jump and measure the one resize a settled layout
    // costs, which is not the complaint.
    .with_motion()
    .window(2560.0, 1440.0)
    .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

#[test]
fn toggling_the_sidebar_costs_no_cliff_over_holding_still() {
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
            // `DRAW` is the renderer's own submit-to-completion span, and it is
            // the only view this suite has of the half a slide actually costs
            // — `drawMs` is the UI thread and never sees the GPU.
            nav.step("the frame-stats panel", "toggle", "Frame stats");
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
            const draw = () => {
                const n = app.snapshot().find(
                    (n) => n.role === "text" && n.label.startsWith("DRAW "));
                const ms = n && Number(n.label.split(" ")[1]);
                return Number.isFinite(ms) ? ms : null;
            };
            const summarise = (sample) => {
                const v = sample.filter((x) => x !== null).sort((a, b) => a - b);
                const at = (q) => v[Math.min(v.length - 1, Math.floor(v.length * q))];
                return { n: v.length, median: at(0.5), p95: at(0.95), max: v[v.length - 1] };
            };
            const stats = (from) => {
                const draw = app.timings().frames
                    .filter((f) => f.frame >= from)
                    .map((f) => f.drawMs)
                    .sort((a, b) => a - b);
                const at = (q) => draw[Math.min(draw.length - 1, Math.floor(draw.length * q))];
                return { frames: draw.length, median: at(0.5), p95: at(0.95), max: draw[draw.length - 1] };
            };

            // Playing, nobody touching the shell.
            const idleFrom = app.snapshot().frame;
            const idleDraw = [];
            for (let step = 0; step < 60; step += 1) {
                app.frames(2, { waitMs: 8 });
                idleDraw.push(draw());
            }
            const idle = stats(idleFrom);

            // ⌘B, then the frames the slide runs over. `SWEEP` is 270ms, so
            // 40 frames at the 8ms cadence covers a whole slide and lands
            // settled — and the pair of toggles leaves the sidebar where it
            // started, so the two stretches are measured at the same layout.
            const toggleFrom = app.snapshot().frame;
            // Proof the gesture landed: the stage's own width has to move, and
            // to be caught *mid*-slide. A ⌘B that reached no handler would
            // leave every number here equal to the idle stretch and say
            // nothing, which is the failure mode this guards.
            const widths = [];
            const slideDraw = [];
            const slide = () => {
                app.key("cmd-b");
                // `SWEEP` is 270ms and a frame here costs rather more than the
                // 8ms it asks for, so this covers a whole slide and lands
                // settled — the sampling is what says where the slide ended,
                // not the count.
                for (let step = 0; step < 20; step += 1) {
                    app.frames(2, { waitMs: 8 });
                    const stage = app.snapshot().find({ role: "card", label: "Stage" });
                    widths.push(Math.round(stage.bounds.width));
                    // Only while the slide is still moving: the tail of this
                    // loop is settled frames, and folding them in would dilute
                    // the very stretch being measured.
                    if (widths.length < 2 || widths[widths.length - 1] !== widths[widths.length - 2]) {
                        slideDraw.push(draw());
                    }
                }
            };
            for (let round = 0; round < 3; round += 1) {
                slide();
                slide();
            }
            const toggle = stats(toggleFrom);

            return {
                idle,
                toggle,
                stageWidth: Math.round(stage.bounds.width),
                widthsSeen: [...new Set(widths)].sort((a, b) => a - b),
                idleDraw: summarise(idleDraw),
                slideDraw: summarise(slideDraw),
            };
        })()
        "#,
    );

    println!("sidebar toggle frame cost: {report}");
    let idle = report["idle"]["median"].as_f64().unwrap_or_default();
    let toggle = report["toggle"]["median"].as_f64().unwrap_or_default();
    let toggle_max = report["toggle"]["max"].as_f64().unwrap_or(f64::MAX);
    assert!(
        report["toggle"]["frames"].as_u64().unwrap_or_default() >= 120,
        "too few frames while toggling: {report}"
    );
    let widths = report["widthsSeen"]
        .as_array()
        .expect("widths were sampled");
    assert!(
        widths.len() >= 4,
        "the stage never slid, so ⌘B was snapped rather than animated: {report}"
    );
    assert!(
        toggle < idle * 3.0,
        "a toggling frame cost {:.1}x an idle one: {report}",
        toggle / idle
    );
    assert!(
        toggle_max < idle * 12.0,
        "a frame during the slide stalled far past an idle one: {report}"
    );

    // The gate. `DRAW` is the renderer's own span, and it is where the cost
    // was: every frame of a slide used to hand the renderer a width it had
    // never seen, which reallocates eight textures and five presentation
    // surfaces and throws the temporal haze history away. Measured at
    // 2560x1440 with a 120-fixture rig, that was 16.2 ms median / 31.7 ms p95
    // against 6.5 ms holding still.
    //
    // Asserted as a *ratio* against this run's own idle stretch rather than as
    // a millisecond count: these two stretches are seconds apart on the same
    // machine, so a loaded host moves both together and only their ratio says
    // anything about the slide. See `luma_app::visualizer::RenderSize`.
    let idle_draw = report["idleDraw"]["median"].as_f64().unwrap_or_default();
    let slide_draw = report["slideDraw"]["median"].as_f64().unwrap_or_default();
    assert!(
        report["slideDraw"]["n"].as_u64().unwrap_or_default() >= 8,
        "too few renderer readings during the slides: {report}"
    );
    assert!(
        idle_draw > 0.0,
        "the frame-stats panel reported no DRAW at all: {report}"
    );
    assert!(
        slide_draw < idle_draw * 1.5,
        "a renderer frame during the slide cost {:.2}x an idle one: {report}",
        slide_draw / idle_draw
    );
}

/// A slide leaves the stage settled, not running.
///
/// Holding the render size means the stage deliberately draws at a size the
/// layout has left behind, and the prepaint asks for another frame while a hold
/// is outstanding so the hold can end after the shell's tween has stopped
/// asking. That request is the one thing this change adds which could keep a
/// stage awake forever, and `FPS IDLE` is the idle gate saying it did not.
///
/// What this cannot check is the request itself: the harness draws frames of
/// its own accord, so a hold would count down here whether anything asked for
/// those frames or not. The countdown's own arithmetic is covered exhaustively
/// by `luma_app::visualizer`'s `RenderSize` unit tests; this is the end-to-end
/// statement that a ⌘B over a still rig ends with the rig still.
#[test]
fn a_paused_stage_rests_again_after_the_sidebar_slides() {
    let mut harness = Fixture::new("sidebar-toggle-rest", SECONDS, Vec::new())
        .with_rig_of(20)
        .with_motion()
        .open(Mode::Pixel);
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            nav.expand();
            app.frames(10, {{ waitMs: 60 }});
        "#
        ),
    );

    let rested = run(
        &mut harness,
        r#"
        (() => {
            const idle = () => app.snapshot().find(
                (n) => n.role === "text" && n.label === "FPS IDLE");
            const settle = () => {
                for (let i = 0; i < 180; i++) {
                    if (idle()) return true;
                    app.frames(1, { waitMs: 16 });
                }
                return false;
            };
            if (!settle()) { return "never rested to begin with"; }
            app.key("cmd-b");
            // Far enough past `SWEEP` that the slide is over and the shell has
            // stopped asking for frames — from here on, only the stage's own
            // request can carry the hold to its end.
            app.frames(30, { waitMs: 16 });
            return settle() ? "rested again" : "never rested again";
        })()
        "#,
    );
    assert_eq!(
        rested.as_str(),
        Some("rested again"),
        "the stage never settled after the slide, so the held size never ended"
    );
}
