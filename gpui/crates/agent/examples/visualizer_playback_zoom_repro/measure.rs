//! The measurement behind `main.rs`.

use std::time::Duration;

use gpui_agent::fixture::{Clip, Fixture, TRACK_NAME, VENUE_NAME};
use gpui_agent::{Harness, Mode};
use serde_json::Value;

/// The suite's `until` and `nav.*` helpers, spliced ahead of each script.
const NAV: &str = concat!(
    include_str!("../../tests/support/until.js"),
    include_str!("../../tests/support/nav.js")
);

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
                // Pulse moves every frame, so the score is never a still.
                Clip {
                    preset: Some(("color.constant@1".into(), "Pulse".into())),
                    ..Clip::new(format!("pulse-{lane}"), "Pulse", 0., f64::from(SECONDS))
                }
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

pub fn main() {
    let mut harness = harness();
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the clip", (s) => s.find({{ role: "card", label: "Constant color" }}) !== undefined);
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
                    pres: readLabel("Present "),
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
