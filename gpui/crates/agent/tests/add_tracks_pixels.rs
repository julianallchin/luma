//! Production pixel proof for the assembled add-track dialog routes.

#![cfg(all(feature = "app", feature = "pixel"))]

mod support;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::{json, Value};
use support::Fixture;

fn run(harness: &mut gpui_agent::Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(180));
    assert_eq!(
        result.error, None,
        "pixel script failed:\n{}",
        result.stdout
    );
    result.result
}

fn capture_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-add-tracks-review".into()),
    );
    fs::create_dir_all(&dir).expect("failed to create add-track capture directory");
    dir
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = capture_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("failed to preserve {}: {error}", destination.display()));
    println!("add-track capture {}", destination.display());
    destination
}

fn pixels(path: impl AsRef<Path>) -> image::RgbaImage {
    image::open(path.as_ref())
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.as_ref().display()))
        .to_rgba8()
}

fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage) -> f32 {
    assert_eq!(left.dimensions(), right.dimensions());
    let changed = left
        .pixels()
        .zip(right.pixels())
        .filter(|(left, right)| (0..3).any(|channel| left[channel].abs_diff(right[channel]) >= 3))
        .count();
    changed as f32 / (left.width() * left.height()) as f32
}

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
}

fn width(bounds: &Value) -> f64 {
    number(bounds, "width")
}

fn height(bounds: &Value) -> f64 {
    number(bounds, "height")
}

fn source_fixture() -> luma_app::SourceAdapterFixture {
    let row = json!({
        "id": "pixel-source",
        "uuid": "pixel-source-uuid",
        "filePath": "/fixture/pixel-source.wav",
        "filename": "pixel-source.wav",
        "title": "Pixel Source Track",
        "artist": "Pixel Artist",
        "album": "Pixel Album",
        "bpm": 126.0,
        "durationSeconds": 180.0,
        "fileSize": 1024,
        "sampleRate": 44100
    });
    luma_app::SourceAdapterFixture {
        library: json!({"trackCount": 1}),
        playlists: json!([{
            "id": "pixel-crate",
            "name": "Pixel crate",
            "parentId": null,
            "trackCount": 1
        }]),
        tracks: json!([row.clone()]),
        playlist_tracks: HashMap::from([("pixel-crate".into(), json!([row]))]),
        searches: HashMap::new(),
    }
}

#[test]
fn production_routes_center_anchor_and_morph_without_clipping() {
    std::env::set_var("LUMA_MOTION_SCALE", "3");
    std::env::set_var("LUMA_MOTION", "off");
    let mut empty = Fixture::new("add-tracks-pixels-empty", 1, vec![])
        .without_track()
        .window(1000.0, 720.0)
        .open(Mode::Pixel);
    let empty_out = run(
        &mut empty,
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        const shot = until("the empty all-Luma route", (s) =>
            s.find({ role: "button", label: "Import tracks" })
                && s.findAll({ role: "row" }).length === 0 ? s : undefined);
        const card = shot.find({ role: "card", label: "Add tracks dialog" });
        const importButton = shot.find({ role: "button", label: "Import tracks" });
        ({ full: app.screenshot().path,
           cardShot: app.screenshot({ node: card }).path,
           card: card.bounds,
           importButton: importButton.bounds })
        "#,
    );
    drop(empty);

    let empty_card = &empty_out["card"];
    let empty_button = &empty_out["importButton"];
    let card_center_x = number(empty_card, "x") + width(empty_card) / 2.0;
    let button_center_x = number(empty_button, "x") + width(empty_button) / 2.0;
    let button_center_y = number(empty_button, "y") + height(empty_button) / 2.0;
    // The empty body begins after the 48px toolbar and the search field's
    // 46px occupied band (12px top margin + 34px field).
    let body_top = number(empty_card, "y") + 94.0;
    let body_center_y = (body_top + number(empty_card, "y") + height(empty_card)) / 2.0;
    assert!(
        (card_center_x - button_center_x).abs() <= 2.0,
        "empty import is not horizontally centered: {empty_out:#}"
    );
    assert!(
        (button_center_y - body_center_y).abs() <= 2.0,
        "empty import is not centered in the body below its toolbar: {empty_out:#}"
    );
    preserve(empty_out["full"].as_str().unwrap(), "empty-browser.png");
    preserve(
        empty_out["cardShot"].as_str().unwrap(),
        "empty-browser-card.png",
    );

    std::env::set_var("LUMA_MOTION", "on");
    let mut routes = Fixture::new("add-tracks-pixels-routes", 8, vec![])
        .with_source_fixture(source_fixture())
        .with_source_fixture_delay(Duration::from_millis(180))
        .with_motion()
        .window(1000.0, 720.0)
        .open(Mode::Pixel);
    let out = run(
        &mut routes,
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        let shot = until("the populated all-Luma route", (s) =>
            s.find({ role: "row", label: "Aurora" })
                && s.find({ role: "button", label: "Import tracks" }) ? s : undefined);
        let card = shot.find({ role: "card", label: "Add tracks dialog" });
        const browser = app.screenshot().path;
        const browserBounds = card.bounds;
        const browserImportBounds = shot.find({ role: "button", label: "Import tracks" }).bounds;

        app.click(shot.find({ role: "button", label: "Import tracks" }));
        shot = until("the committed source picker", (s) =>
            s.find({ role: "button", label: "Rekordbox" })?.enabled === true ? s : undefined);
        card = shot.find({ role: "card", label: "Add tracks dialog" });
        const picker = app.screenshot().path;
        const pickerBounds = card.bounds;

        app.click(shot.find({ role: "button", label: "Rekordbox" }));
        app.frames(1, { waitMs: 180 });
        shot = app.snapshot();
        card = shot.find({ role: "card", label: "Add tracks dialog" });
        const midpoint = app.screenshot().path;
        const midpointBounds = card.bounds;

        shot = until("the committed source library", (s) =>
            s.find({ role: "row", label: "Pixel Source Track" })
                && s.find({ role: "input", label: "Search source…" })?.focused === true ? s : undefined);
        card = shot.find({ role: "card", label: "Add tracks dialog" });
        const library = app.screenshot().path;
        const libraryBounds = card.bounds;
        const sourceImportBounds = shot.find({ role: "button", label: "Import selected" }).bounds;
        ({ browser, browserBounds, browserImportBounds,
           picker, pickerBounds,
           midpoint, midpointBounds,
           library, libraryBounds, sourceImportBounds })
        "#,
    );

    for (label, bounds, expected_width, expected_height) in [
        ("browser", &out["browserBounds"], 760.0, 600.0),
        ("picker", &out["pickerBounds"], 440.0, 280.0),
        ("library", &out["libraryBounds"], 900.0, 620.0),
    ] {
        assert!(
            (width(bounds) - expected_width).abs() <= 2.0
                && (height(bounds) - expected_height).abs() <= 2.0,
            "{label} did not own its exact route size: {bounds:#}"
        );
        assert!(
            number(bounds, "x") >= 15.0
                && number(bounds, "y") >= 37.0
                && number(bounds, "x") + width(bounds) <= 985.0
                && number(bounds, "y") + height(bounds) <= 705.0,
            "{label} escaped the usable viewport: {bounds:#}"
        );
    }

    let mid = &out["midpointBounds"];
    assert!(
        width(mid) > 440.0 && width(mid) < 900.0 && height(mid) > 280.0 && height(mid) < 620.0,
        "the production route snapped instead of morphing: {mid:#}"
    );
    for (label, card, button) in [
        (
            "browser",
            &out["browserBounds"],
            &out["browserImportBounds"],
        ),
        ("source", &out["libraryBounds"], &out["sourceImportBounds"]),
    ] {
        let gap = number(card, "y") + height(card) - number(button, "y") - height(button);
        assert!(
            (8.0..=22.0).contains(&gap),
            "{label} import action is not anchored in the bottom footer: gap={gap}, {button:#}"
        );
    }

    let browser = preserve(out["browser"].as_str().unwrap(), "track-browser.png");
    let picker = preserve(out["picker"].as_str().unwrap(), "source-picker.png");
    let midpoint = preserve(
        out["midpoint"].as_str().unwrap(),
        "source-morph-midpoint.png",
    );
    let library = preserve(out["library"].as_str().unwrap(), "source-library.png");
    assert!(
        differing_fraction(&pixels(&browser), &pixels(&picker)) > 0.08,
        "browser and picker are not visibly distinct production routes"
    );
    assert!(
        differing_fraction(&pixels(&midpoint), &pixels(&library)) > 0.04,
        "morph midpoint is not visibly distinct from its committed route"
    );
}
