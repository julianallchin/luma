//! Capture the 3D view's shots, and time its frames.
//!
//! ```sh
//! cargo test --release -p gpui-agent --features pixel --test visualizer_capture -- --ignored --nocapture
//! ```
//!
//! `#[ignore]` because this asserts nothing — it is the eye and the stopwatch,
//! run when the viewport's look or its cost is the question. `LUMA_SHOTS`
//! names the directory the frames are copied into.
#![cfg(feature = "pixel")]

mod support;

use std::path::PathBuf;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

const SECONDS: u32 = 20;

fn shots_dir() -> PathBuf {
    let dir =
        PathBuf::from(std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-shots".into()));
    std::fs::create_dir_all(&dir).expect("could not make the shots directory");
    dir
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// The brightest of several successive frames, copied out under `name`.
///
/// The fixture's pattern pulses twice a second, so any single frame is as
/// likely to catch the trough as the peak — and a trough is a black rectangle
/// whatever the camera is doing. Shooting a whole pulse period and keeping the
/// brightest frame is what makes these shots about the camera.
fn shot(harness: &mut Harness, name: &str) {
    let mut best: Option<(f64, std::path::PathBuf)> = None;
    for _ in 0..8 {
        let value = run(harness, "app.frames(3, { waitMs: 30 }); app.screenshot()");
        let from =
            std::path::PathBuf::from(value["path"].as_str().expect("a screenshot has a path"));
        let mean = mean_level(&from);
        if best.as_ref().is_none_or(|(seen, _)| mean > *seen) {
            best = Some((mean, from));
        }
    }
    let (mean, from) = best.expect("at least one frame");
    let to = shots_dir().join(format!("{name}.png"));
    std::fs::copy(&from, &to).expect("could not copy the shot");
    println!("shot {} (mean {mean:.2})", to.display());
}

/// Mean channel level over the whole frame.
fn mean_level(path: &std::path::Path) -> f64 {
    let image = image::open(path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()))
        .to_rgb8();
    let sum: f64 = image
        .pixels()
        .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
        .sum();
    sum / f64::from(image.width() * image.height() * 3)
}

#[test]
#[ignore = "capture, not a gate"]
fn capture() {
    let mut harness = Fixture::new(
        "visualizer-capture",
        SECONDS,
        vec![Clip::new("pattern-pulse", "Pulse", 0., f64::from(SECONDS)).lit()],
    )
    .with_rig()
    .open(Mode::Pixel);

    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            app.action("luma::OpenVisualizer");
            nav.expand();
            app.frames(12, {{ waitMs: 60 }});
        "#
        ),
    );
    shot(&mut harness, "L01-open");

    // Let the transport run, then shoot the same lit frame the gate called P05.
    run(
        &mut harness,
        r#"
            nav.step("the Play button", "button", "Play");
            app.frames(30, { waitMs: 55 });
        "#,
    );
    shot(&mut harness, "P05");

    // The orbit that used to end in a flat red field: five 200 px pulls up.
    for i in 1..=5 {
        run(
            &mut harness,
            r#"
                var stage = app.snapshot().find({ role: "card", label: "Stage" });
                app.drag(stage, { dx: 0, dy: -200 }, { steps: 20, restale: "match" });
                app.frames(4, { waitMs: 30 });
            "#,
        );
        shot(&mut harness, &format!("C01-up-{i}"));
    }

    // ...and the same walk downward, which the polar clamp also owns.
    for i in 1..=3 {
        run(
            &mut harness,
            r#"
                var stage = app.snapshot().find({ role: "card", label: "Stage" });
                app.drag(stage, { dx: 60, dy: 200 }, { steps: 20, restale: "match" });
                app.frames(4, { waitMs: 30 });
            "#,
        );
        shot(&mut harness, &format!("C02-down-{i}"));
    }

    // Zoom all the way in: the near bound is what keeps this out of the beams.
    run(
        &mut harness,
        r#"
            for (let i = 0; i < 25; i++) { nav.step("Zoom In", "button", "Zoom In"); }
            app.frames(6, { waitMs: 30 });
        "#,
    );
    shot(&mut harness, "C03-zoomed-in");

    // Back to the opening pose for the shot that shows the default framing.
    run(
        &mut harness,
        &format!(
            r#"
            // Close the tab so the reopen is a fresh view — a revealed tab
            // would keep the zoomed camera this shot must not have.
            nav.closeTab();
            until("the editor", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            app.action("luma::OpenVisualizer");
            nav.expand();
            app.frames(12, {{ waitMs: 60 }});
            nav.step("the Play button", "button", "Play");
            app.frames(30, {{ waitMs: 55 }});
        "#
        ),
    );
    shot(&mut harness, "D01-default-framing");

    // Frame cost, orbiting continuously — the gate's own measurement, reduced
    // here rather than dumped: two hundred frame records say less than four
    // quantiles do.
    let timings = run(
        &mut harness,
        r#"
            var snapshot = app.snapshot();
            var stage = snapshot.find({ role: "card", label: "Stage" });
            var from = snapshot.frame;
            app.drag(stage, { dx: 260, dy: 60 }, { steps: 90, restale: "match" });
            var draw = app.timings().frames
                .filter((f) => f.frame >= from)
                .map((f) => f.drawMs)
                .sort((a, b) => a - b);
            ({
                frames: draw.length,
                p50: draw[Math.floor(draw.length * 0.50)],
                p95: draw[Math.floor(draw.length * 0.95)],
                max: draw[draw.length - 1],
            })
        "#,
    );
    println!("orbit frame cost {timings}");
}
