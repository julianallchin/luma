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

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

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

    let (whole_path, whole) =
        support::image::keep_in("fixture-picker", &out["whole"], "picker-dialog");
    let (half_path, half) =
        support::image::keep_in("fixture-picker", &out["half"], "picker-dialog-one-group");
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
