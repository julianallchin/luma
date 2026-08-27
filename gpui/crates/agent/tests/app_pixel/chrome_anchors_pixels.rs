//! What the window's corners actually look like in each panel state.
//!
//! The geometry gate lives in `chrome_anchors`; this is the half a number
//! cannot answer — that the right anchor reads as *pressed* while its panel is
//! up, that the left one keeps its quiet rest ink, and that neither collides
//! with the traffic lights or the cluster beside it. The band is 38px tall, so
//! each shot is cropped to it: a full 1280×800 frame diffed for a 24px control
//! is mostly waveform.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::fs;
use std::path::PathBuf;

use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;
use support::{Clip, Fixture};

/// The head band, full width — see [`luma_app::chrome::HEIGHT`].
const BAND_HEIGHT: u32 = 38;

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

fn shots_dir() -> PathBuf {
    let directory = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-chrome-anchors".into()),
    );
    fs::create_dir_all(&directory).expect("could not create the capture directory");
    directory
}

/// Screenshot, crop to the band, and keep it under `name`.
fn band(harness: &mut Harness, name: &str) -> image::RgbaImage {
    let value = run(harness, "app.screenshot()");
    let source = value["path"].as_str().expect("a screenshot has a path");
    let full = image::open(source)
        .unwrap_or_else(|error| panic!("could not read {source}: {error}"))
        .to_rgba8();
    // The renderer works in device pixels; the band is stated in points.
    let scale = full.height() / 800;
    let cropped =
        image::imageops::crop_imm(&full, 0, 0, full.width(), BAND_HEIGHT * scale).to_image();
    let destination = shots_dir().join(format!("{name}.png"));
    cropped
        .save(&destination)
        .unwrap_or_else(|error| panic!("could not write {}: {error}", destination.display()));
    println!("chrome capture {}", destination.display());
    cropped
}

/// Mean absolute luma over a point-space box, in device pixels.
fn brightness(shot: &image::RgbaImage, x: u32, width: u32) -> f32 {
    let scale = shot.height() / BAND_HEIGHT;
    let (x, width) = (x * scale, width * scale);
    let mut total = 0.0;
    let mut count = 0.0;
    for pixel in shot
        .enumerate_pixels()
        .filter(|(px, _, _)| *px >= x && *px < x + width)
        .map(|(_, _, pixel)| pixel)
    {
        total +=
            0.299 * f32::from(pixel[0]) + 0.587 * f32::from(pixel[1]) + 0.114 * f32::from(pixel[2]);
        count += 1.0;
    }
    total / count
}

/// The empty panel, whole, for eyes rather than numbers — three stacked
/// choices centred in the panel. Not a gate: what is being judged is whether
/// it reads as an offer, which no assertion answers.
#[test]
#[ignore = "capture, not a gate"]
fn capture_the_empty_panel() {
    let mut harness = Fixture::new(
        "chrome-empty-panel-shot",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .window(1280.0, 800.0)
    .open(Mode::Pixel);

    run(
        &mut harness,
        &format!(
            r#"
            {nav}
            nav.venue("Test Venue");
            app.frames(10, {{ waitMs: 40 }});
        "#,
            nav = support::NAV
        ),
    );
    let value = run(&mut harness, "app.screenshot()");
    let source = value["path"].as_str().expect("a screenshot has a path");
    let destination = shots_dir().join("empty-panel.png");
    fs::copy(source, &destination).expect("could not keep the empty-panel shot");
    println!("empty panel {}", destination.display());
}

#[test]
fn the_right_anchor_reads_as_pressed_only_while_its_panel_is_up() {
    let mut harness = Fixture::new(
        "chrome-anchors-pixels",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .window(1280.0, 800.0)
    .open(Mode::Pixel);

    run(
        &mut harness,
        &format!(
            r#"
            {nav}
            nav.trackEditor("Test Venue", "Aurora");
            until("the timeline", (s) => s.find({{ role: "card", label: "Waveform" }}) !== undefined);
            app.frames(8, {{ waitMs: 40 }});
        "#,
            nav = support::NAV
        ),
    );
    let both_open = band(&mut harness, "both-open");

    run(
        &mut harness,
        r#"app.action("luma::ToggleWorkspace"); app.frames(8, { waitMs: 40 });"#,
    );
    let panel_closed = band(&mut harness, "panel-closed");

    run(
        &mut harness,
        r#"app.action("luma::ToggleWorkspace"); app.action("luma::ToggleSidebar"); app.frames(8, { waitMs: 40 });"#,
    );
    let sidebar_closed = band(&mut harness, "sidebar-closed");

    // The toggle's own 24px box, at the two anchors (see `chrome_anchors` for
    // where those numbers come from).
    let right_lit = brightness(&both_open, 1244, 24);
    let right_dark = brightness(&panel_closed, 1244, 24);
    assert!(
        right_lit > right_dark + 1.0,
        "the right anchor did not lift while its panel was open: {right_lit} vs {right_dark}"
    );

    // The left anchor is the same ink in both — it is the sidebar's state that
    // changes, and the *sidebar* toggle is what carries that reading.
    let left_open = brightness(&both_open, 74, 24);
    let left_shut = brightness(&sidebar_closed, 74, 24);
    assert!(
        left_open > left_shut + 1.0,
        "the left anchor did not lift while the sidebar was open: {left_open} vs {left_shut}"
    );

    // Nothing paints into the lights' corner in any state — the band yields it.
    for (name, shot) in [
        ("both-open", &both_open),
        ("panel-closed", &panel_closed),
        ("sidebar-closed", &sidebar_closed),
    ] {
        let gap = brightness(shot, 98, 10);
        assert!(
            gap < 70.0,
            "{name} painted a control into the gap right of the toggle: {gap}"
        );
    }
}
