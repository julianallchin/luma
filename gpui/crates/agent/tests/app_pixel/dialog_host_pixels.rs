//! Pixel and geometry proof for the assembled production dialog host.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;
use support::{Clip, Fixture};

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

fn capture_dir() -> PathBuf {
    let directory = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-dialog-host-review".into()),
    );
    fs::create_dir_all(&directory).expect("could not create dialog capture directory");
    directory
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = capture_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("could not preserve {}: {error}", destination.display()));
    println!("production dialog capture {}", destination.display());
    destination
}

fn pixels(path: impl AsRef<Path>) -> image::RgbaImage {
    image::open(path.as_ref())
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.as_ref().display()))
        .to_rgba8()
}

/// The shared diff at the shared noise floor — see `support::image`.
fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage) -> f32 {
    support::image::differing_fraction(left, right, support::image::CHANNEL_NOISE)
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

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
}

#[test]
fn production_root_layers_a_compact_frosted_dialog_below_live_window_controls() {
    let mut harness = Fixture::new(
        "dialog-host-pixels",
        8,
        vec![Clip::new("pattern-strobe", "Strobe", 1.0, 4.0)],
    )
    .window(640.0, 480.0)
    .open(Mode::Pixel);

    let out = run(
        &mut harness,
        &support::script(
            r#"
            nav.venue("Test Venue");
            until("track browser", () =>
                app.snapshot().find({ role: "input", label: "Search tracks…" }));
            const base = app.screenshot().path;

            app.action("luma::OpenPatterns");
            until("pattern dialog", () =>
                app.snapshot().find({ role: "card", label: "Pattern dialog" }));
            app.frames(2);
            const shot = app.snapshot();
            const card = shot.find({ role: "card", label: "Pattern dialog" });
            const close = shot.find({ role: "button", label: "close" });
            const minimize = shot.find({ role: "button", label: "minimize" });
            const maximize = shot.find({ role: "button", label: "maximize" });
            ({
                base,
                overlay: app.screenshot().path,
                cardShot: app.screenshot({ node: card }).path,
                card: card.bounds,
                close: close.bounds,
                minimize: minimize.bounds,
                maximize: maximize.bounds,
            })
            "#,
        ),
    );

    let card = &out["card"];
    let x = number(card, "x");
    let y = number(card, "y");
    let width = number(card, "width");
    let height = number(card, "height");
    assert!(x >= 15.0, "dialog escaped the left gutter: {card:#}");
    assert!(y >= 37.0, "dialog overlapped the titlebar: {card:#}");
    assert!(
        x + width <= 625.0,
        "dialog escaped the right gutter: {card:#}"
    );
    assert!(
        y + height <= 465.0,
        "dialog escaped the bottom gutter: {card:#}"
    );
    assert!(
        width > 500.0 && height > 350.0,
        "compact clamp collapsed the route: {card:#}"
    );

    for label in ["close", "minimize", "maximize"] {
        let bounds = &out[label];
        assert!(
            number(bounds, "width") > 0.0 && number(bounds, "height") > 0.0,
            "{label} control disappeared under the modal: {bounds:#}"
        );
        assert!(
            number(bounds, "y") + number(bounds, "height") <= y,
            "{label} control is not layered in the reserved titlebar: {bounds:#}, card={card:#}"
        );
    }

    let base_path = preserve(out["base"].as_str().unwrap(), "dialog-host-base.png");
    let overlay_path = preserve(out["overlay"].as_str().unwrap(), "dialog-host-patterns.png");
    let card_path = preserve(
        out["cardShot"].as_str().unwrap(),
        "dialog-host-pattern-card.png",
    );
    let changed = differing_fraction(&pixels(base_path), &pixels(overlay_path));
    assert!(
        changed > 0.20,
        "the production modal plane did not visibly transform the shell: {changed:.3}"
    );
    assert!(
        luma_range(&pixels(card_path)) > 24,
        "the frosted card capture is a flat placeholder rather than production route content"
    );
}
