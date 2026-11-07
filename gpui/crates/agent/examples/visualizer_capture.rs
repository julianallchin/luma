//! Capture the 3D view's shots, and time its frames.
//!
//! ```sh
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo run --release -p gpui-agent --features pixel \
//!     --example visualizer_capture
//! ```
//!
//! An example, not a test, because it asserts nothing: it is the eye and the
//! stopwatch, run when the viewport's look or its cost is the question. The
//! frames are copied into `<shots>/visualizer` (`LUMA_SHOTS`, else
//! `<temp>/luma-shots`).

use std::time::Duration;

use gpui_agent::fixture::{Fixture, TRACK_NAME, VENUE_NAME};
use gpui_agent::{Harness, Mode, NAV};
use serde_json::json;

const SECONDS: u32 = 20;

/// The brightest of several successive frames, copied out under `name`.
///
/// The fixture's pattern pulses twice a second, so any single frame is as
/// likely to catch the trough as the peak — and a trough is a black rectangle
/// whatever the camera is doing. Shooting a whole pulse period and keeping the
/// brightest frame is what makes these shots about the camera.
fn shot(harness: &mut Harness, name: &str) {
    let mut best: Option<(f64, std::path::PathBuf)> = None;
    for _ in 0..8 {
        let value = harness.run(
            "app.frames(3, { waitMs: 30 }); app.screenshot()",
            Duration::from_secs(300),
        );
        let from =
            std::path::PathBuf::from(value["path"].as_str().expect("a screenshot has a path"));
        let mean = mean_level(&from);
        if best.as_ref().is_none_or(|(seen, _)| mean > *seen) {
            best = Some((mean, from));
        }
    }
    let (mean, from) = best.expect("at least one frame");
    let to = gpui_agent::shots::keep(&from, &format!("visualizer/{name}"))
        .expect("could not keep the shot");
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

fn main() {
    let mut harness = Fixture::from_json(
        "visualizer-capture",
        json!({
            "seconds": SECONDS,
            "clips": [{ "pattern": "pattern-pulse", "name": "Pulse", "start": 0, "end": SECONDS }],
            "rig": 4,
        }),
    )
    .unwrap()
    .open(Mode::Pixel);

    harness.run(
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            nav.agentOff();
            app.frames(12, {{ waitMs: 60 }});
        "#
        ),
        Duration::from_secs(300),
    );
    shot(&mut harness, "L01-open");

    // Let the transport run, then shoot the same lit frame the gate called P05.
    harness.run(
        r#"
            nav.step("the Play button", "button", "Play");
            app.frames(30, { waitMs: 55 });
        "#,
        Duration::from_secs(300),
    );
    shot(&mut harness, "P05");

    // The frame-stats panel unfolded: the graph and the per-phase numbers.
    // Folded back before the camera walk so it occludes nothing there.
    harness.run(
        r#"
            nav.step("the frame-stats panel", "toggle", "Frame stats");
            app.frames(6, { waitMs: 30 });
        "#,
        Duration::from_secs(300),
    );
    shot(&mut harness, "P06-frame-stats");
    harness.run(
        r#"nav.step("the frame-stats panel", "toggle", "Frame stats");"#,
        Duration::from_secs(300),
    );

    // The orbit that used to end in a flat red field: five 200 px pulls up.
    for i in 1..=5 {
        harness.run(
            r#"
                var stage = app.snapshot().find({ role: "card", label: "Stage" });
                app.drag(stage, { dx: 0, dy: -200 }, { steps: 20, restale: "match" });
                app.frames(4, { waitMs: 30 });
            "#,
            Duration::from_secs(300),
        );
        shot(&mut harness, &format!("C01-up-{i}"));
    }

    // ...and the same walk downward, which the polar clamp also owns.
    for i in 1..=3 {
        harness.run(
            r#"
                var stage = app.snapshot().find({ role: "card", label: "Stage" });
                app.drag(stage, { dx: 60, dy: 200 }, { steps: 20, restale: "match" });
                app.frames(4, { waitMs: 30 });
            "#,
            Duration::from_secs(300),
        );
        shot(&mut harness, &format!("C02-down-{i}"));
    }

    // Zoom all the way in: the near bound is what keeps this out of the beams.
    harness.run(
        r#"
            for (let i = 0; i < 8; i++) {
                app.scroll(app.snapshot().find({ role: "card", label: "Stage" }), { dy: -200 });
                app.frames(1);
            }
            app.frames(6, { waitMs: 30 });
        "#,
        Duration::from_secs(300),
    );
    shot(&mut harness, "C03-zoomed-in");

    // Back to the opening pose for the shot that shows the default framing.
    harness.run(
        &format!(
            r#"
            // Toggling the stage off drops it, so bringing it back is a fresh
            // view — switching tabs within one room deliberately would not be,
            // and this shot must not keep the zoomed camera above.
            app.action("luma::ToggleVisualizer");
            app.frames(2);
            app.action("luma::ToggleVisualizer");
            until("the stage", (s) => s.find({{ role: "card", label: "Stage" }}) !== undefined);
            nav.agentOff();
            app.frames(12, {{ waitMs: 60 }});
            nav.step("the Play button", "button", "Play");
            app.frames(30, {{ waitMs: 55 }});
        "#
        ),
        Duration::from_secs(300),
    );
    shot(&mut harness, "D01-default-framing");

    // Frame cost, orbiting continuously — the gate's own measurement, reduced
    // here rather than dumped: two hundred frame records say less than four
    // quantiles do.
    let timings = harness.run(
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
        Duration::from_secs(300),
    );
    println!("orbit frame cost {timings}");
}
