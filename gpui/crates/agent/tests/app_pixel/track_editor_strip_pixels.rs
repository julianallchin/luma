//! The args strip under a real renderer: the ghosted empty state and the
//! populated one, shot from the same session.
//!
//! The property pinned is the strip's whole reason to exist stated in pixels:
//! selecting a clip repaints the strip's *interior* — the shot pair must
//! differ, and must differ inside the same box, because the box itself (and
//! everything above it) is not allowed to move. The headless suite asserts
//! the geometry through the node protocol; this asserts the renderer agrees,
//! and leaves a PNG of each state behind for eyes.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test app_pixel track_editor_strip
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::path::PathBuf;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use image::RgbaImage;
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// The headless strip fixture's shape, on a renderer.
fn harness() -> Harness {
    Fixture::new(
        "track-editor-strip-pixels",
        TRACK_SECONDS,
        vec![
            Clip::new("pat-glow", "Glow", 2.0, 5.0).lit(),
            Clip::new("pat-glow", "Glow", 8.0, 11.0).lit(),
        ],
    )
    .open(Mode::Pixel)
}

const SCRIPT: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    nav.expand();
    nav.stageOff();
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    app.frames(8, { waitMs: 30 });

    const strip = () => app.snapshot().find({ role: "card", label: "Args strip" });
    const emptyBounds = strip().bounds;
    const empty = app.screenshot({ node: strip() });
    const emptyWindow = app.screenshot();

    app.click(app.snapshot().find({ role: "card", label: "Glow" }));
    until("the strip populates", (s) =>
        s.findAll({ role: "input" }).some((n) => n.label.startsWith("intensity = ")));
    app.frames(8, { waitMs: 30 });
    const populatedBounds = strip().bounds;
    const populated = app.screenshot({ node: strip() });
    const populatedWindow = app.screenshot();

    // Open the blend menu: the float card must appear (its rows register as
    // buttons) and, from the bottom-docked strip, land inside the window.
    app.click(app.snapshot().find({ role: "select", label: "replace" }));
    until("the blend menu", (s) => s.find({ role: "button", label: "multiply" }) !== undefined);
    app.frames(6, { waitMs: 30 });
    const menuRow = app.snapshot().find({ role: "button", label: "multiply" });
    const menuWindow = app.screenshot();

    ({ emptyBounds, populatedBounds, empty, populated, emptyWindow, populatedWindow,
       menuRow: menuRow.bounds, menuWindowShot: menuWindow })
"#;

#[test]
fn the_strip_repaints_in_place_when_a_clip_is_selected() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // The box did not move — the node protocol's no-reflow contract, agreed
    // to by the frame the renderer actually drew.
    assert_eq!(out["emptyBounds"], out["populatedBounds"]);

    let (empty_path, empty) = keep(&out["empty"], "strip-empty");
    let (populated_path, populated) = keep(&out["populated"], "strip-populated");
    keep(&out["emptyWindow"], "window-empty");
    keep(&out["populatedWindow"], "window-populated");
    let (menu_path, _) = keep(&out["menuWindowShot"], "window-menu-open");
    // The open blend menu's rows are on screen and wholly inside the 1200×800
    // window — the float's snap doing its job from the bottom-docked strip,
    // where an unsnapped menu would hang below the bottom edge.
    let row = &out["menuRow"];
    let top = row["y"].as_f64().unwrap();
    let bottom = top + row["height"].as_f64().unwrap();
    assert!(
        top > 0. && bottom <= 800.,
        "the blend menu row sits outside the window: {row:#} ({})",
        menu_path.display(),
    );
    eprintln!(
        "args strip shots:\n  empty: {}\n  populated: {}",
        empty_path.display(),
        populated_path.display()
    );

    assert_eq!(
        (empty.width(), empty.height()),
        (populated.width(), populated.height()),
        "the two strip shots cover different boxes"
    );

    // Populating repaints the interior: ghost slabs give way to live
    // controls, so a healthy pair differs across a solid fraction of the
    // strip. Byte-equal shots would mean the selection never reached the
    // strip at all.
    let differing = empty
        .pixels()
        .zip(populated.pixels())
        .filter(|(a, b)| a != b)
        .count();
    let total = (empty.width() * empty.height()) as usize;
    assert!(
        differing * 50 > total,
        "only {differing} of {total} pixels changed between empty and \
         populated — the strip did not populate\n  {} vs {}",
        empty_path.display(),
        populated_path.display(),
    );
}

/// Copy the shot somewhere stable and decode it, so a failing assertion names
/// a path that still exists — the harness deletes its own directory.
fn keep(shot: &Value, name: &str) -> (PathBuf, RgbaImage) {
    let source = shot["path"].as_str().expect("a shot has a path");
    let directory = std::env::temp_dir().join("luma-args-strip");
    std::fs::create_dir_all(&directory).expect("failed to create the shot directory");
    let kept = directory.join(format!("{name}.png"));
    std::fs::copy(source, &kept).expect("failed to keep the shot");
    let image = image::open(&kept)
        .expect("the harness wrote a shot that is not an image")
        .to_rgba8();
    (kept, image)
}
