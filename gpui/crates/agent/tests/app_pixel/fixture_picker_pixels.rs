//! The fixture picker under a real renderer: the room, lit by the selection.
//!
//! ```sh
//! CARGO_TARGET_DIR="$(git rev-parse --show-toplevel)/gpui/target-pixel" \
//!   cargo test -p gpui-agent --features pixel --test app_pixel fixture_picker
//! ```
//!
//! Two shots of one dialog: nothing ticked (the whole rig lit, which is what
//! `all` means) and one group ticked. The assertion is that those two frames
//! *differ* — a picture that did not change when a group was ticked would mean
//! the highlight never reached the renderer, which is the one failure a
//! headless test cannot see.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use image::RgbaImage;
use serde_json::Value;
use support::{Clip, Fixture};

/// Where the shots are kept. Outside the harness's own directory, which it
/// deletes, so a failing assertion names a path that still exists.
fn directory() -> PathBuf {
    std::env::var("LUMA_PICKER_SHOTS").map_or_else(
        |_| std::env::temp_dir().join("luma-fixture-picker"),
        PathBuf::from,
    )
}

fn harness() -> Harness {
    Fixture::new(
        "fixture-picker-pixels",
        20,
        vec![Clip::new("pat-glow", "Glow", 2.0, 5.0).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Pixel)
}

const SCRIPT: &str = r#"
    function checkbox(label) {
        return app.snapshot().find({ role: "checkbox", label });
    }
    function lit() {
        // The preview only exists once a frame has come back; before that the
        // card shows a line of text where the picture goes.
        return app.snapshot().find({ role: "card", label: "Selection preview" }) !== undefined;
    }

    nav.venue("Test Venue");
    app.frames(8);
    nav.track("Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    nav.expand();
    nav.stageOff();

    app.click(app.snapshot().find({ role: "card", label: "Glow" }));
    until("the strip", (s) =>
        s.findAll({ role: "input" }).some((n) => n.label.startsWith("expression = ")));
    app.click(app.snapshot().find({ role: "button", label: "Pick fixtures" }));
    until("the picker", (s) => s.find({ role: "checkbox", label: "left_movers" }) !== undefined);
    until("the whole rig lit", lit);
    app.frames(10, { waitMs: 40 });
    const whole = app.screenshot();

    app.click(checkbox("left_movers"));
    app.frames(6, { waitMs: 40 });
    until("the half-rig frame", lit);
    app.frames(10, { waitMs: 40 });
    const half = app.screenshot();

    ({ whole, half })
"#;

#[test]
#[ignore = "capture: needs a GPU and writes PNGs"]
fn ticking_a_group_changes_the_picture() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let (whole_path, whole) = keep(&out["whole"], "picker-dialog");
    let (half_path, half) = keep(&out["half"], "picker-dialog-one-group");
    eprintln!(
        "fixture picker shots:\n  {}\n  {}",
        whole_path.display(),
        half_path.display()
    );

    assert_eq!(
        (whole.width(), whole.height()),
        (half.width(), half.height()),
        "the two shots cover different windows"
    );
    // Half the rig going dark is a large, structural change; a threshold well
    // under it still fails a picture that never re-rendered.
    let differing = whole
        .pixels()
        .zip(half.pixels())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differing > 5_000,
        "only {differing} pixels changed when a group was ticked — the \
         highlight never reached the renderer\n  {} vs {}",
        whole_path.display(),
        half_path.display(),
    );
}

fn keep(shot: &Value, name: &str) -> (PathBuf, RgbaImage) {
    let source = shot["path"].as_str().expect("a shot has a path");
    let directory = directory();
    std::fs::create_dir_all(&directory).expect("failed to create the shot directory");
    let kept = directory.join(format!("{name}.png"));
    std::fs::copy(Path::new(source), &kept).expect("failed to keep the shot");
    let image = image::open(&kept)
        .expect("the harness wrote a shot that is not an image")
        .to_rgba8();
    (kept, image)
}
