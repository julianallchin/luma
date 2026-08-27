//! The user's repro: a track playing, then zooming in, and where the frame goes.
//!
//! ```sh
//! CARGO_TARGET_DIR=target-pixel cargo test -p gpui-agent --features pixel \
//!     --test visualizer_playback_zoom_repro -- --nocapture
//! ```
//!
//! # Why another stage test
//!
//! `visualizer_playback_budget` measures playback and `visualizer_zoom_budget`
//! measures zooming, each on its own. The reported failure is *both at once* —
//! "playing a track lags hella, unplayable; zooming in freezes" — and the two
//! costs are not independent. Playback already pays the score, the frame
//! assembly and the hit-test rebuild once per frame; a wheel gesture multiplies
//! how many times per displayed frame that happens. A test that never does both
//! cannot see the product.
//!
//! # Why it reports rather than asserts
//!
//! The isolated renderer profile says every one of these cases is inside
//! budget, and the user's hands say otherwise, so the useful output here is an
//! *attribution* and not a pass. The one thing it does assert is that the rig
//! under measurement is the one it claims — every number is meaningless if the
//! stage quietly fell back to four movers or an unlit scene.
//!
//! # Reading the output
//!
//! Four numbers, and the point is which of them moves:
//!
//! - `drawMs` — gpui's element walk on the **UI thread**. `sample`, `build` and
//!   `pick` all happen inside it, so it bounds them.
//! - `parkedMs` — the app settling its async work.
//! - `UI (S/B/P)` — that walk split into score evaluation, frame assembly and
//!   hit-test rebuild.
//! - `PRES` — wall time between frames actually reaching the screen.
//! - `gpu` — the renderer thread's own half: CPU encode, GPU pass total and
//!   cluster binning, read off the Renderer Lab. This is the only one of the
//!   five that zooming can move, because zooming changes fill and nothing else.
//!
//! A UI-thread stall shows as `drawMs` rising with `PRES`. A renderer that
//! cannot keep up shows as `PRES` rising while `drawMs` stays flat. A gesture
//! storm shows as neither rising much while the *count* of renders per gesture
//! explodes, which is why `frames` is reported and not just the percentiles.
#![cfg(feature = "pixel")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

const SECONDS: u32 = 30;

/// A venue-sized rig of movers, because every renderer cost this is about —
/// cluster occupancy, shadow passes, draw count — scales on it.
///
/// Overridable, because one fixture size answers "does this configuration
/// lag" and only a sweep answers "what does it scale on" — and the second is
/// the question, given that the reported content is not this content.
fn rig() -> usize {
    from_env("LUMA_REPRO_RIG", 120)
}

/// Overlapping lit clips spanning the track. `Scene::composite` walks every
/// annotation whose span contains the playhead, so this is the axis the score
/// costs on. Overridable for the same reason as [`rig`].
fn clips() -> usize {
    from_env("LUMA_REPRO_CLIPS", 12)
}

fn from_env(key: &str, fallback: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

/// Roughly a quarter of a laptop screen, which is where the report came from.
///
/// Worth varying deliberately: the volumetric march scales with output pixels,
/// so a small window makes the *renderer* cheaper while leaving every
/// UI-thread cost exactly where it was. If the lag survives shrinking the
/// window, it was never the march.
const WINDOW: (f32, f32) = (760.0, 520.0);

fn harness() -> Harness {
    Fixture::new(
        "visualizer-playback-zoom-repro",
        SECONDS,
        (0..clips())
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
    .with_rig_of(rig())
    .window(WINDOW.0, WINDOW.1)
    .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

#[test]
fn playing_then_zooming_reports_where_the_frame_went() {
    let mut harness = harness();
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
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
            // Every timing label lives in the frame-stats panel and is only
            // published while it is unfolded, so unfold it before measuring.
            nav.step("the frame-stats panel", "toggle", "Frame stats");
            // Past the cold caches, so the measurement is of steady state.
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

            // The toolbar publishes the UI-thread split and the presentation
            // spacing as text, so a script can read what the panel shows.
            const readLabel = (prefix) => {
                const n = app.snapshot().find(
                    (n) => n.role === "text" && n.label.startsWith(prefix));
                return n ? n.label : null;
            };

            const stats = (from) => {
                const rows = app.timings().frames.filter((f) => f.frame >= from);
                const pick = (key) => rows.map((f) => f[key]).sort((a, b) => a - b);
                const q = (a, p) => a[Math.min(a.length - 1, Math.floor(a.length * p))];
                const draw = pick("drawMs");
                const parked = pick("parkedMs");
                return {
                    frames: draw.length,
                    drawMedian: q(draw, 0.5),
                    drawP95: q(draw, 0.95),
                    drawMax: draw[draw.length - 1],
                    parkedMedian: q(parked, 0.5),
                    parkedMax: parked[parked.length - 1],
                    ui: readLabel("UI "),
                    pres: readLabel("PRES "),
                    gpu: readLabel("CPU "),
                };
            };

            // 1. Playing, camera untouched. This is "lags hella" with no gesture.
            const playFrom = app.snapshot().frame;
            app.frames(150, { waitMs: 8 });
            const playing = stats(playFrom);

            // 2. Playing while zooming in, the way a wheel does it: many small
            //    steps rather than one jump. `frames` against the gesture's own
            //    event count is what shows a storm.
            const zoomFrom = app.snapshot().frame;
            let events = 0;
            for (let burst = 0; burst < 6; burst += 1) {
                app.scroll(stage, { dy: 40, steps: 20, restale: "match" });
                events += 20;
            }
            const zooming = stats(zoomFrom);
            zooming.scrollEvents = events;

            // 3. Settled again after the zoom, still playing and now close in.
            //    Separates "the gesture is expensive" from "being zoomed in is".
            const afterFrom = app.snapshot().frame;
            app.frames(150, { waitMs: 8 });
            const zoomedIn = stats(afterFrom);

            return {
                playing,
                zooming,
                zoomedIn,
                readout: (app.snapshot().find(
                    (n) => n.role === "text" && n.label.includes("FIXTURES")) || {}).label,
            };
        })()
        "#,
    );

    let readout = report["readout"].as_str().unwrap_or_default();
    let rig = rig();
    assert!(
        readout.starts_with(&format!("{rig} FIXTURES")) && readout.contains("LIVE"),
        "the stage under measurement is not the lit {rig}-fixture rig: {readout:?}"
    );

    println!("rig={rig} clips={}", clips());
    for phase in ["playing", "zooming", "zoomedIn"] {
        println!(
            "{phase:>9}: {}",
            serde_json::to_string(&report[phase]).unwrap_or_default()
        );
    }
}
