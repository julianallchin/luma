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
fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
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
