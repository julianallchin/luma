//! A real score, played through the real renderer, sampled second by second.
//!
//! ```sh
//! LUMA_REAL_CONFIG=/path/to/a/library/copy \
//! LUMA_REAL_VENUE=Club LUMA_REAL_TRACK="…" \
//! LUMA_REAL_FROM=45 LUMA_REAL_TO=53 \
//! CARGO_TARGET_DIR=…/target-pixel cargo test -p gpui-agent --features pixel \
//!     --test visualizer_real_score_window -- --ignored --nocapture
//! ```
//!
//! # Why this exists next to the synthetic instruments
//!
//! `visualizer_playback_zoom_repro` sweeps a *shape* — rig size, clip count —
//! and answers "what does this configuration cost". It cannot answer "why is
//! this show slow at 0:49", because the thing that changes at 0:49 is content:
//! which fixtures a clip selects, and what kind of fixtures those are. Only the
//! user's own library has that.
//!
//! # Read-only, and why the copy is not optional
//!
//! Opening a library runs migrations, so pointing this at a live one would
//! write to it. `LUMA_REAL_CONFIG` must be a COPY. Nothing here writes to the
//! library either way, but the app underneath it does not promise that.
//!
//! # Ignored by default
//!
//! It needs a library that only exists on one machine, so it is not a gate. It
//! reports; it asserts only that it measured the rig it claims to have measured.
#![cfg(feature = "pixel")]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use luma_ui::runtime::Runtime;
use serde_json::Value;
use support::NAV;

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set"))
}

/// The window this test is about, and a run-up before it. The run-up is the
/// whole point: a number from inside the window means nothing without the
/// seconds either side of it measured the same way in the same process.
fn window() -> (f64, f64) {
    let parse = |key: &str, fallback: f64| {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    (parse("LUMA_REAL_FROM", 45.0), parse("LUMA_REAL_TO", 53.0))
}

fn harness() -> Harness {
    let config_dir = PathBuf::from(env("LUMA_REAL_CONFIG"));
    assert!(
        config_dir.join("luma.db").is_file(),
        "LUMA_REAL_CONFIG must be a library copy containing luma.db"
    );
    let fixtures_root = std::env::var("LUMA_REAL_FIXTURES").ok().map(PathBuf::from);
    let root: gpui_agent::RootFactory =
        Arc::new(move |window: &mut Window, cx: &mut App| -> AnyView {
            luma_app::init(cx);
            let library = luma_app::Library::open().expect("open the copied library");
            let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
            cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
                .into()
        });
    // The volumetric march is fill-bound, so the window is part of the
    // measurement, not scenery: the same score costs a different amount on a
    // different display and this is the knob that says which one was measured.
    let window_size = std::env::var("LUMA_REAL_WINDOW").ok().and_then(|spec| {
        let (w, h) = spec.split_once('x')?;
        Some(gpui::size(
            gpui::px(w.parse().ok()?),
            gpui::px(h.parse().ok()?),
        ))
    });
    let config = Config {
        mode: Mode::Pixel,
        // Playing through several seconds of a real track is a long call by
        // design; the default would time out mid-window.
        call_timeout: Duration::from_secs(900),
        runtime: Runtime {
            config_dir: Some(config_dir),
            fixtures_root,
            reduced_motion: true,
            motion_scale: 1.0,
            stage_gpu: None,
            cloud: false,
        },
        ..Config::default()
    };
    let config = match window_size {
        Some(size) => Config {
            window_size: size,
            ..config
        },
        None => config,
    };
    Harness::headless(config, root).expect("failed to start the harness")
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(900));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

/// Open the venue's track and put the stage on screen with the lab reporting.
///
/// Shared by both tests so "which screen was measured" cannot differ between
/// them — the two numbers are only comparable if the walk was.
fn open_the_stage(harness: &mut Harness) {
    let venue = env("LUMA_REAL_VENUE");
    let track = env("LUMA_REAL_TRACK");
    let zoom: u32 = std::env::var("LUMA_REAL_ZOOM")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    run(
        harness,
        &format!(
            r#"
            {NAV}
            // A library that already has this venue open lands straight on its
            // track list, and asking for the picker there never finds one. Wait
            // for whichever of the two actually rendered before deciding — the
            // first snapshot of a cold app has neither.
            until("the library to settle", (s) =>
                s.find({{ role: "row", label: {track:?} }}) !== undefined
                    || s.find({{ role: "row", label: {venue:?} }}) !== undefined);
            if (app.snapshot().find({{ role: "row", label: {track:?} }}) === undefined) {{
                nav.venue({venue:?});
            }}
            nav.track({track:?});
            until("the timeline", (s) => s.find({{ role: "card", label: "Waveform" }}) !== undefined);
            nav.expand();
            // The timing labels are only published while the frame-stats
            // panel is unfolded.
            nav.step("the frame-stats panel", "toggle", "Frame stats");
            // Dolly in before measuring. The default opening camera frames the
            // whole rig, which is roughly twice as far out as an operator
            // actually works at — and the volumetric march is fill-bound, so
            // measuring at the opening distance measures a different scene.
            for (let press = 0; press < {zoom}; press += 1) {{
                app.scroll(app.snapshot().find({{ role: "card", label: "Stage" }}),
                    {{ dy: -200 }});
                app.frames(1, {{ waitMs: 16 }});
            }}
            app.frames(10, {{ waitMs: 60 }});
        "#
        ),
    );
}

#[test]
#[ignore = "needs a real library copy via LUMA_REAL_CONFIG"]
fn a_real_score_reports_where_each_second_goes() {
    let (from, to) = window();
    let mut harness = harness();
    open_the_stage(&mut harness);

    // Sampling is driven from the script so each reading is one frame's worth
    // of toolbar state, taken with the transport running rather than paused —
    // a paused stage re-renders the same frame and reports a cost no viewer pays.
    let report = run(
        &mut harness,
        &format!(
            r#"
            (() => {{
                const label = (prefix) => {{
                    const n = app.snapshot().find(
                        (n) => n.role === "text" && n.label.startsWith(prefix));
                    return n ? n.label : null;
                }};
                // "M:SS / M:SS" — the transport's own account of where it is.
                const clock = () => {{
                    const n = app.snapshot().find(
                        (n) => n.role === "text" && /^\d+:\d\d \/ \d+:\d\d$/.test(n.label));
                    if (!n) return null;
                    const [m, s] = n.label.split(" / ")[0].split(":");
                    return Number(m) * 60 + Number(s);
                }};

                app.click(app.snapshot().find({{ role: "button", label: "Play" }}));
                const samples = [];
                // Bounded by frames, not by wall time, so a slow host produces
                // fewer samples rather than an unbounded run.
                for (let i = 0; i < 4000; i++) {{
                    const at = clock();
                    if (at !== null && at >= {from} && at <= {to}) {{
                        samples.push({{
                            t: at,
                            draw: label("DRAW "),
                            ui: label("UI "),
                            pres: label("PRES "),
                            gpu: label("CPU "),
                        }});
                    }}
                    if (at !== null && at > {to}) break;
                    app.frames(1, {{ waitMs: 16 }});
                }}
                return {{ samples, clock: clock() }};
            }})()
        "#
        ),
    );

    let samples = report["samples"].as_array().cloned().unwrap_or_default();
    assert!(
        !samples.is_empty(),
        "the transport never reached {from}s: {report:#}"
    );

    // One row per whole second: the question is which second is different, and
    // a per-frame dump buries that under four hundred lines.
    let mut by_second: std::collections::BTreeMap<i64, Vec<&Value>> =
        std::collections::BTreeMap::new();
    for sample in &samples {
        let t = sample["t"].as_f64().unwrap_or(-1.0) as i64;
        by_second.entry(t).or_default().push(sample);
    }
    println!(
        "samples={} across {} seconds",
        samples.len(),
        by_second.len()
    );
    for (second, rows) in &by_second {
        let number = |row: &Value, key: &str| -> Option<f64> {
            row[key]
                .as_str()?
                .split_whitespace()
                .find_map(|word| word.parse::<f64>().ok())
        };
        let mean = |key: &str| -> f64 {
            let values: Vec<f64> = rows.iter().filter_map(|row| number(row, key)).collect();
            if values.is_empty() {
                return f64::NAN;
            }
            values.iter().sum::<f64>() / values.len() as f64
        };
        println!(
            "  {second:>3}s  n={:<3} draw={:6.2}  ui={:5.2}  pres={:6.2}",
            rows.len(),
            mean("draw"),
            mean("ui"),
            mean("pres"),
        );
        if let Some(gpu) = rows.last().and_then(|row| row["gpu"].as_str()) {
            println!("        {gpu}");
        }
    }
}

/// The zoom report: a real rig, playing, with the camera driven progressively
/// into the beams the way a wheel does it.
///
/// "Any time I zoom in it freezes; I have to keep the spotlights really zoomed
/// out." So the axis is camera distance, and the thing that has to be real is
/// the rig — its fixture classes, its beam geometry, and the haze settings the
/// stage actually opens with. A synthetic line of movers has none of those.
///
/// Reports per zoom step rather than in aggregate, because the claim under test
/// is that cost rises *with* closeness: an average over the whole gesture would
/// show the same number whether the last step cost 2 ms or 200.
#[test]
#[ignore = "needs a real library copy via LUMA_REAL_CONFIG"]
fn zooming_into_the_beams_while_playing_reports_where_the_frame_goes() {
    let (from, _) = window();
    let mut harness = harness();
    open_the_stage(&mut harness);

    let report = run(
        &mut harness,
        &format!(
            r#"
            (() => {{
                const label = (prefix) => {{
                    const n = app.snapshot().find(
                        (n) => n.role === "text" && n.label.startsWith(prefix));
                    return n ? n.label : null;
                }};
                const clock = () => {{
                    const n = app.snapshot().find(
                        (n) => n.role === "text" && /^\d+:\d\d \/ \d+:\d\d$/.test(n.label));
                    if (!n) return null;
                    const [m, s] = n.label.split(" / ")[0].split(":");
                    return Number(m) * 60 + Number(s);
                }};
                // The stage's own zoom affordance rather than a wheel over the
                // viewport: a scroll that silently is not bound to the camera
                // would make every number below a measurement of nothing, and
                // this button calls `dolly` by construction.
                const zoomIn = () => {{
                    app.scroll(app.snapshot().find({{ role: "card", label: "Stage" }}),
                        {{ dy: -200 }});
                }};
                const reading = (step, phase) => ({{
                    step,
                    phase,
                    t: clock(),
                    draw: label("DRAW "),
                    ui: label("UI "),
                    pres: label("PRES "),
                    gpu: label("CPU "),
                }});
                // Settled: what being at this zoom costs.
                const settled = (step) => {{
                    app.frames(12, {{ waitMs: 16 }});
                    return reading(step, "settled");
                }};

                app.click(app.snapshot().find({{ role: "button", label: "Play" }}));
                // Into the window the show actually complains about, so the
                // zoom is measured against the same content the user zooms over.
                for (let i = 0; i < 4000 && (clock() === null || clock() < {from}); i++) {{
                    app.frames(1, {{ waitMs: 16 }});
                }}

                // Proof the gesture is a gesture. If forty presses of the
                // stage's own zoom leave the picture identical, every timing
                // below describes a camera that never moved, and the honest
                // answer is "this measured nothing" rather than "zoom is free".
                const before = app.screenshot();
                const steps = [settled(0)];
                // Two readings per burst, and the pair is the whole point:
                // "moving" is taken with no settle at all, so it describes the
                // frames the gesture itself produced — camera motion is what
                // invalidates the cluster grid and the cascades, and a reading
                // taken twelve frames later has already lost it. "settled" is
                // what simply being this close costs. A cost that shows up in
                // one and not the other says which of the two the user is
                // actually hitting.
                for (let step = 1; step <= 10; step += 1) {{
                    // Several presses per burst so one step is a real change of
                    // distance, not a nudge inside the same cluster cell.
                    for (let press = 0; press < 4; press += 1) {{
                        zoomIn();
                    }}
                    steps.push(reading(step, "moving"));
                    steps.push(settled(step));
                }}
                return {{ steps, before, after: app.screenshot() }};
            }})()
        "#
        ),
    );

    let steps = report["steps"].as_array().cloned().unwrap_or_default();
    assert!(!steps.is_empty(), "no zoom readings: {report:#}");

    let frame = |key: &str| {
        let path = report[key]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("{key} has no screenshot path: {report:#}"));
        image::open(path)
            .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
            .to_rgba8()
    };
    let (before, after) = (frame("before"), frame("after"));
    let changed = before
        .pixels()
        .zip(after.pixels())
        .filter(|(a, b)| a != b)
        .count();
    let fraction = changed as f64 / before.pixels().len() as f64;
    println!("zoom moved {:.1}% of the pixels", fraction * 100.0);
    assert!(
        fraction > 0.01,
        "forty presses of Zoom In changed {:.3}% of the picture — the camera \
         did not move, so the timings below measured nothing",
        fraction * 100.0
    );
    println!("zoom-in, playing, real rig — one row per wheel burst");
    for step in &steps {
        println!(
            "  step {:>2} {:<8} t={:>4}  {}  {}  {}",
            step["step"].as_i64().unwrap_or(-1),
            step["phase"].as_str().unwrap_or("?"),
            step["t"].as_f64().unwrap_or(-1.0),
            step["draw"].as_str().unwrap_or("DRAW —"),
            step["ui"].as_str().unwrap_or("UI —"),
            step["pres"].as_str().unwrap_or("PRES —"),
        );
        if let Some(gpu) = step["gpu"].as_str() {
            println!("            {gpu}");
        }
    }
}
