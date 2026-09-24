//! Production pixel and motion proof for the workspace tab chrome.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(300));
    assert_eq!(
        result.error, None,
        "script failed:\n{code}\n{}",
        result.stdout
    );
    result.result
}

fn review_dir() -> PathBuf {
    let directory = PathBuf::from("/tmp/luma-tabs-review");
    fs::create_dir_all(&directory).expect("could not create tab review directory");
    directory
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = review_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("could not preserve {}: {error}", destination.display()));
    println!("tab chrome capture {}", destination.display());
    destination
}

fn pixels(path: impl AsRef<Path>) -> image::RgbaImage {
    image::open(path.as_ref())
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.as_ref().display()))
        .to_rgba8()
}

fn luma_range(image: &image::RgbaImage) -> u8 {
    let mut low = u8::MAX;
    let mut high = u8::MIN;
    for pixel in image.pixels() {
        let luma = ((u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])) / 3) as u8;
        low = low.min(luma);
        high = high.max(luma);
    }
    high.saturating_sub(low)
}

/// Mean adjacent-pixel luma delta in the text-free right half of the menu.
/// The same window-space field is sampled before and during the popover, so a
/// real backdrop blur must materially suppress the timeline grid/waveform's
/// high-frequency edges in both the entrance and resting frames.
/// Mean absolute luma difference between two frames over the menu's box.
///
/// Both frames are sampled at the *same* window-space region, so this says
/// only "what is on screen here changed" — which is the one thing a popover
/// owes its backdrop and the one thing that does not depend on where the strip
/// put it.
fn menu_field_change(before: &image::RgbaImage, after: &image::RgbaImage, bounds: &Value) -> f64 {
    assert_eq!(
        before.dimensions(),
        after.dimensions(),
        "frames must be the same size to be differenced"
    );
    let scale = f64::from(before.width()) / 1280.0;
    let x0 = ((number(bounds, "x") + 12.0) * scale).round() as u32;
    let x1 = ((number(bounds, "x") + number(bounds, "width") - 12.0) * scale).round() as u32;
    let y0 = ((number(bounds, "y") + 10.0) * scale).round() as u32;
    let y1 = ((number(bounds, "y") + number(bounds, "height") - 10.0) * scale).round() as u32;
    let luma = |image: &image::RgbaImage, x: u32, y: u32| {
        let pixel = image.get_pixel(x, y);
        (f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2])) / 3.0
    };
    let mut total = 0.0;
    let mut samples = 0_u64;
    for y in y0..y1 {
        for x in x0..x1 {
            total += (luma(after, x, y) - luma(before, x, y)).abs();
            samples += 1;
        }
    }
    total / samples.max(1) as f64
}

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
}

fn opacity(value: &Value) -> f64 {
    value["label"]
        .as_str()
        .and_then(|label| label.rsplit_once(' '))
        .and_then(|(_, opacity)| opacity.parse().ok())
        .unwrap_or_else(|| panic!("closing node did not report opacity: {value:#}"))
}

#[test]
fn empty_workspace_action_rows_are_visible() {
    let mut harness = Fixture::new(
        "empty-workspace-pixels",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .window(1280.0, 800.0)
    .open(Mode::Pixel);
    let out = run(
        &mut harness,
        &support::script(
            r#"
        nav.venue("Test Venue");
        app.action("luma::NewTab");
        until("empty workspace", s => s.find({ role: "card", label: "Empty panel" }) !== undefined);
        app.frames(4);
        ({ shot: app.screenshot().path,
           rows: ["Track editor"].map(label =>
             app.snapshot().find({ role: "button", label }).bounds) })
    "#,
        ),
    );
    let path = preserve(out["shot"].as_str().unwrap(), "empty-workspace.png");
    assert!(luma_range(&pixels(path)) > 30);
    for bounds in out["rows"].as_array().unwrap() {
        assert!(number(bounds, "width") > 200.0, "{out:#}");
        assert!(number(bounds, "height") >= 50.0, "{out:#}");
    }
}
