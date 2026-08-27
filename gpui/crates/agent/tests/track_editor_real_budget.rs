//! What one frame of the track editor costs on a real score, while the view
//! is moving.
//!
//! ```sh
//! LUMA_REAL_CONFIG=/path/to/a/library/copy \
//! LUMA_REAL_VENUE=Club LUMA_REAL_TRACK="Black Hole" \
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel \
//!     --test track_editor_real_budget -- --ignored --nocapture
//! ```
//!
//! # Why this exists next to `app_pixel track_editor_budget`
//!
//! The synthetic budget sweeps a *shape* — lanes, clips, one length — and
//! answers "what does a busy score cost". It cannot answer "why is this one
//! slow", because what differs between two scores is content: how many clips
//! carry a decoded preview, how short they are, how many rows a venue puts in
//! each heatmap. Only the user's own library has that, so this opens it.
//!
//! # Read-only, and why the copy is not optional
//!
//! Opening a library runs migrations, so `LUMA_REAL_CONFIG` must be a COPY.
//! See `visualizer_real_score_window.rs`, which this mirrors.
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
    // The preview resample is sized by the body's device pixels, so the window
    // is part of the measurement: `LUMA_REAL_WINDOW=2800x1600` for a retina-
    // sized canvas.
    let window_size = std::env::var("LUMA_REAL_WINDOW").ok().and_then(|spec| {
        let (w, h) = spec.split_once('x')?;
        Some(gpui::size(
            gpui::px(w.parse().ok()?),
            gpui::px(h.parse().ok()?),
        ))
    });
    let config = Config {
        mode: Mode::Pixel,
        window_size: window_size.unwrap_or(Config::default().window_size),
        call_timeout: Duration::from_secs(900),
        runtime: Runtime {
            config_dir: Some(config_dir),
            fixtures_root,
            reduced_motion: true,
            motion_scale: 1.0,
            stage_gpu: None,
        },
        ..Config::default()
    };
    Harness::headless(config, root).expect("failed to start the harness")
}

/// Frame-time percentiles of one gesture, from `app.timings()`.
fn leg(frames: &Value) -> String {
    let mut total: Vec<f64> = frames
        .as_array()
        .expect("frames")
        .iter()
        .map(|f| f["total"].as_f64().unwrap_or(0.))
        .collect();
    let mut draw: Vec<f64> = frames
        .as_array()
        .expect("frames")
        .iter()
        .map(|f| f["draw"].as_f64().unwrap_or(0.))
        .collect();
    total.sort_by(|a, b| a.total_cmp(b));
    draw.sort_by(|a, b| a.total_cmp(b));
    let pct = |v: &[f64], p: f64| v[((v.len() as f64 * p) as usize).min(v.len() - 1)];
    format!(
        "{} frames  total p50 {:.2} p95 {:.2} max {:.2}  draw p50 {:.2} p95 {:.2} max {:.2}",
        total.len(),
        pct(&total, 0.5),
        pct(&total, 0.95),
        total.last().copied().unwrap_or(0.),
        pct(&draw, 0.5),
        pct(&draw, 0.95),
        draw.last().copied().unwrap_or(0.),
    )
}

#[test]
#[ignore = "needs a real library copy via LUMA_REAL_CONFIG"]
fn a_real_score_reports_what_scrolling_and_zooming_cost() {
    let venue = env("LUMA_REAL_VENUE");
    let track = env("LUMA_REAL_TRACK");
    // The stage above the editor is on by default in the app, and it repaints
    // with every editor frame; `LUMA_REAL_STAGE=1` keeps it in the measurement.
    let stage = std::env::var("LUMA_REAL_STAGE").is_ok_and(|v| v == "1");
    let mut harness = harness();
    let result = harness.exec(
        &format!(
            r#"
            {NAV}
            until("the library to settle", (s) =>
                s.find({{ role: "row", label: {track:?} }}) !== undefined
                    || s.find({{ role: "row", label: {venue:?} }}) !== undefined);
            if (app.snapshot().find({{ role: "row", label: {track:?} }}) === undefined) {{
                nav.venue({venue:?});
            }}
            nav.track({track:?});
            until("the timeline", (s) => s.find({{ role: "card", label: "Waveform" }}) !== undefined);
            nav.expand();
            if (!{stage}) nav.stageOff();
            else nav.step("the frame-stats panel", "toggle", "Frame stats");
            // Let the previews land before measuring.
            app.frames(60, {{ waitMs: 50 }});

            function measure(run) {{
                const from = app.frames(1).frame;
                run();
                return app
                    .timings()
                    .frames.filter((f) => f.frame > from)
                    .map((f) => ({{ total: f.parkedMs + f.drawMs, draw: f.drawMs }}));
            }}
            const waveform = () => app.snapshot().find({{ role: "card", label: "Waveform" }});
            const cards = () => app.snapshot().findAll({{ role: "card" }});

            app.scroll(waveform(), {{ dy: -800, steps: 20, modifiers: ["platform"] }});
            const clips = cards().filter((c) => !c.label.endsWith(" preview")).length - 2;
            const previews = cards().filter((c) => c.label.endsWith(" preview")).length;

            const scrollOut = measure(() => app.scroll(waveform(), {{ dx: -900, steps: 60 }}));
            const zoomIn = measure(() => app.scroll(waveform(), {{ dy: 300, steps: 60, modifiers: ["platform"] }}));
            const scrollIn = measure(() => app.scroll(waveform(), {{ dx: -900, steps: 60 }}));
            const zoomOut = measure(() => app.scroll(waveform(), {{ dy: -300, steps: 60, modifiers: ["platform"] }}));

            // With the stage on: what it reports while the editor scrolls,
            // one step at a time so each reading is one frame's worth.
            const label = (prefix) => {{
                const n = app.snapshot().find((n) => n.role === "text" && n.label.startsWith(prefix));
                return n ? n.label : null;
            }};
            const stage = [];
            if ({stage}) {{
                app.frames(30, {{ waitMs: 32 }});
                stage.push({{ at: "rest", fps: label("FPS "), gpu: label("CPU "), ui: label("UI ") }});
                for (let i = 0; i < 40; i++) {{
                    app.scroll(waveform(), {{ dx: -15, steps: 1 }});
                    app.frames(1, {{ waitMs: 8 }});
                    stage.push({{ at: i, fps: label("FPS "), gpu: label("CPU "), ui: label("UI ") }});
                }}
            }}
            ({{ clips, previews, scrollOut, zoomIn, scrollIn, zoomOut, stage, mode: app.timings().mode }})
        "#
        ),
        Duration::from_secs(900),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out = result.result;
    println!(
        "\n{venue} / {track} (stage {stage}): {} clips, {} previews ({})\n  scroll @ zoom-out : {}\n  zoom in           : {}\n  scroll @ zoom-in  : {}\n  zoom out          : {}\n",
        out["clips"],
        out["previews"],
        out["mode"],
        leg(&out["scrollOut"]),
        leg(&out["zoomIn"]),
        leg(&out["scrollIn"]),
        leg(&out["zoomOut"]),
    );
    for row in out["stage"].as_array().into_iter().flatten() {
        println!(
            "  stage {:>4}: {:?}  {:?}  {:?}",
            row["at"], row["fps"], row["gpu"], row["ui"]
        );
    }
}
