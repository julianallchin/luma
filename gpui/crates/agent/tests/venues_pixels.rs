//! Pixel proof for every production venue-dialog route.

#![cfg(all(feature = "app", feature = "pixel"))]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{px, size, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use serde_json::Value;

const LAST_VENUE: &str = "last-venue";

async fn seed(dir: &Path, remembered: bool) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open pixel fixture app database");
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', NULL, 'Pixel Venue')")
        .execute(&db.0)
        .await
        .expect("failed to seed pixel venue");
    let state = luma_lib::database::local::state::init_state_db_at(dir)
        .await
        .expect("failed to open pixel fixture state database");
    if remembered {
        luma_lib::database::local::auth::set_session_item(&state.0, LAST_VENUE, "venue")
            .await
            .expect("failed to seed pixel preference");
    }
    db.0.close().await;
    state.0.close().await;
}

fn fixture_dir(name: &str, remembered: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "luma-gpui-venue-pixels-{name}-{}",
        std::process::id()
    ));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("failed to create pixel fixture directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start pixel fixture runtime")
        .block_on(seed(&dir, remembered));
    dir
}

fn harness(dir: &Path, fixture: luma_app::NavigationFixture) -> Harness {
    std::env::set_var("LUMA_CONFIG_DIR", dir);
    std::env::set_var("LUMA_MOTION", "off");
    let root: gpui_agent::RootFactory = Arc::new(move |window: &mut Window, cx: &mut App| {
        luma_app::init(cx);
        let mut library = luma_app::Library::open().expect("failed to open pixel library");
        library.set_navigation_fixture(fixture.clone());
        let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
        cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
            .into()
    });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            window_size: size(px(900.0), px(650.0)),
            call_timeout: Duration::from_secs(30),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start venue pixel harness")
}

fn run(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(60));
    assert_eq!(
        result.error, None,
        "pixel script failed:\n{}",
        result.stdout
    );
    result.result
}

fn capture_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-venue-review".into()),
    );
    fs::create_dir_all(&dir).expect("failed to create venue capture directory");
    dir
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = capture_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("failed to preserve {}: {error}", destination.display()));
    println!("venue capture {}", destination.display());
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

fn luma_range(image: &image::RgbaImage) -> u8 {
    let (mut low, mut high) = (u8::MAX, u8::MIN);
    for pixel in image.pixels() {
        let luma = ((u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])) / 3) as u8;
        low = low.min(luma);
        high = high.max(luma);
    }
    high.saturating_sub(low)
}

/// Horizontal high-frequency energy within a screen rectangle. The titlebar
/// strips sit outside the dialog card, so their change measures the live
/// backdrop/scrim treatment rather than route-content replacement.
fn edge_energy(image: &image::RgbaImage, x0: u32, x1: u32, y0: u32, y1: u32) -> f32 {
    let mut total = 0u64;
    let mut count = 0u64;
    for y in y0..y1.min(image.height()) {
        for x in (x0 + 1)..x1.min(image.width()) {
            let left = image.get_pixel(x - 1, y);
            let here = image.get_pixel(x, y);
            total += (0..3)
                .map(|channel| u64::from(left[channel].abs_diff(here[channel])))
                .sum::<u64>();
            count += 3;
        }
    }
    total as f32 / count.max(1) as f32
}

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
}

fn assert_card_geometry(label: &str, card: &Value) {
    let x = number(card, "x");
    let y = number(card, "y");
    let width = number(card, "width");
    let height = number(card, "height");
    assert!(x >= 15.0, "{label} escaped left gutter: {card:#}");
    assert!(y >= 37.0, "{label} overlapped titlebar: {card:#}");
    assert!(x + width <= 885.0, "{label} escaped right gutter: {card:#}");
    assert!(
        y + height <= 635.0,
        "{label} escaped bottom gutter: {card:#}"
    );
    assert!(
        width > 600.0 && height > 400.0,
        "{label} collapsed: {card:#}"
    );
}

#[test]
fn production_venue_routes_are_frosted_distinct_and_viewport_safe() {
    let restored_dir = fixture_dir("routes", true);
    let mut routes = harness(
        &restored_dir,
        luma_app::NavigationFixture {
            catalogue_responses: vec![(Duration::from_millis(300), None), (Duration::ZERO, None)],
            ..Default::default()
        },
    );
    let shots = run(
        &mut routes,
        r#"
        let shot = app.snapshot();
        const loadingCard = shot.find({ role: "card", label: "Venue dialog" });
        if (!shot.find({ role: "text", label: "Loading venues…" })) {
            throw new Error("first production paint skipped Loading");
        }
        const loading = app.screenshot().path;
        const loadingCardShot = app.screenshot({ node: loadingCard }).path;

        const baseState = until("the restored venue shell", (s) =>
            s.find({ role: "button", label: "Pixel Venue" }) !== undefined);
        const base = app.screenshot().path;
        app.click(baseState.find({ role: "button", label: "Pixel Venue" }));
        shot = until("the browse route", (s) =>
            s.find({ role: "input", label: "Search venues…" })?.focused === true
                && s.find({ role: "card", label: "Pixel Venue" }) !== undefined);
        const browseCard = shot.find({ role: "card", label: "Venue dialog" });
        const browse = app.screenshot().path;
        const browseCardShot = app.screenshot({ node: browseCard }).path;

        app.click(shot.find({ role: "button", label: "Create venue" }));
        shot = until("the create route", (s) =>
            s.find({ role: "input", label: "Venue name" })?.focused === true);
        const createCard = shot.find({ role: "card", label: "Venue dialog" });
        ({ loading,
           loadingCardShot,
           loadingBounds: loadingCard.bounds,
           base,
           browse,
           browseCardShot,
           browseBounds: browseCard.bounds,
           create: app.screenshot().path,
           createCardShot: app.screenshot({ node: createCard }).path,
           createBounds: createCard.bounds })
        "#,
    );

    let error_dir = fixture_dir("error", false);
    let long_error = format!(
        "Pixel catalogue failure while reading venue metadata.\n{}\n{}",
        "UNBROKEN_ERROR_TOKEN_".repeat(180),
        (0..80)
            .map(|line| format!("diagnostic line {line}: the venue catalogue remains unavailable"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut error = harness(
        &error_dir,
        luma_app::NavigationFixture {
            catalogue_responses: vec![(Duration::from_millis(250), Some(long_error))],
            ..Default::default()
        },
    );
    let error_shots = run(
        &mut error,
        r#"
        const initial = app.snapshot();
        if (!initial.find({ role: "text", label: "Loading venues…" })) {
            throw new Error("error route skipped Loading");
        }
        const errorLoading = app.screenshot().path;
        const shot = until("the pixel error route", (s) =>
            s.find((n) => n.role === "text" && n.label.includes("Pixel catalogue failure")) !== undefined);
        const card = shot.find({ role: "card", label: "Venue dialog" });
        const errorViewport = shot.find({ role: "card", label: "Venue error viewport" });
        const errorText = shot.find((n) => n.role === "text" && n.label.includes("Pixel catalogue failure"));
        ({ errorLoading,
           error: app.screenshot().path,
           errorCardShot: app.screenshot({ node: card }).path,
           errorBounds: card.bounds,
           errorViewportBounds: errorViewport.bounds,
           errorTextBounds: errorText.bounds,
           errorLabelLength: errorText.label.length })
        "#,
    );

    for (label, bounds) in [
        ("loading", &shots["loadingBounds"]),
        ("browse", &shots["browseBounds"]),
        ("create", &shots["createBounds"]),
        ("error", &error_shots["errorBounds"]),
    ] {
        assert_card_geometry(label, bounds);
    }
    let error_card = &error_shots["errorBounds"];
    let error_viewport = &error_shots["errorViewportBounds"];
    let error_text = &error_shots["errorTextBounds"];
    let viewport_x = number(error_viewport, "x");
    let viewport_y = number(error_viewport, "y");
    let viewport_width = number(error_viewport, "width");
    let viewport_height = number(error_viewport, "height");
    let text_x = number(error_text, "x");
    let text_y = number(error_text, "y");
    let text_width = number(error_text, "width");
    let text_height = number(error_text, "height");
    let card_x = number(error_card, "x");
    let card_y = number(error_card, "y");
    let card_width = number(error_card, "width");
    let card_height = number(error_card, "height");
    assert!(
        viewport_x >= card_x + 32.0
            && viewport_x + viewport_width <= card_x + card_width - 32.0,
        "error viewport escaped the dialog's horizontal content gutters: viewport={error_viewport:#}, card={error_card:#}"
    );
    assert!(
        viewport_y >= card_y && viewport_y + viewport_height <= card_y + card_height,
        "error viewport escaped the dialog vertically: viewport={error_viewport:#}, card={error_card:#}"
    );
    assert!(
        text_x >= viewport_x
            && text_x + text_width <= viewport_x + viewport_width
            && text_y >= viewport_y
            && text_y + text_height <= viewport_y + viewport_height,
        "ellipsized error text escaped its clip viewport: text={error_text:#}, viewport={error_viewport:#}"
    );
    assert!(
        error_shots["errorLabelLength"].as_u64().unwrap() > 4_000,
        "the semantic node did not retain the complete adversarial error"
    );
    println!(
        "venue error geometry: card=({card_x:.0},{card_y:.0}) {card_width:.0}x{card_height:.0}; viewport=({viewport_x:.0},{viewport_y:.0}) {viewport_width:.0}x{viewport_height:.0}; text=({text_x:.0},{text_y:.0}) {text_width:.0}x{text_height:.0}"
    );

    let loading = preserve(shots["loading"].as_str().unwrap(), "venue-loading.png");
    let base = preserve(shots["base"].as_str().unwrap(), "venue-shell-base.png");
    let browse = preserve(shots["browse"].as_str().unwrap(), "venue-browse.png");
    let create = preserve(shots["create"].as_str().unwrap(), "venue-create.png");
    let error_loading = preserve(
        error_shots["errorLoading"].as_str().unwrap(),
        "venue-error-loading.png",
    );
    let error = preserve(error_shots["error"].as_str().unwrap(), "venue-error.png");
    let card_paths = [
        preserve(
            shots["loadingCardShot"].as_str().unwrap(),
            "venue-loading-card.png",
        ),
        preserve(
            shots["browseCardShot"].as_str().unwrap(),
            "venue-browse-card.png",
        ),
        preserve(
            shots["createCardShot"].as_str().unwrap(),
            "venue-create-card.png",
        ),
        preserve(
            error_shots["errorCardShot"].as_str().unwrap(),
            "venue-error-card.png",
        ),
    ];

    let loading_pixels = pixels(&loading);
    let base_pixels = pixels(&base);
    let browse_pixels = pixels(&browse);
    let create_pixels = pixels(&create);
    let error_loading_pixels = pixels(&error_loading);
    let error_pixels = pixels(&error);
    let shell_edge_before = edge_energy(&base_pixels, 300, 850, 2, 34);
    let shell_edge_after = edge_energy(&browse_pixels, 300, 850, 2, 34);
    let sidebar_edge_before = edge_energy(&base_pixels, 8, 250, 2, 34);
    let sidebar_edge_after = edge_energy(&browse_pixels, 8, 250, 2, 34);
    let shell_change = differing_fraction(&base_pixels, &browse_pixels);
    let browse_create_change = differing_fraction(&browse_pixels, &create_pixels);
    let error_change = differing_fraction(&error_loading_pixels, &error_pixels);
    println!(
        "venue pixels: shell changed={shell_change:.3}, browse/create={browse_create_change:.3}, error transition={error_change:.3}; titlebar edge shell {shell_edge_before:.3}->{shell_edge_after:.3}, sidebar {sidebar_edge_before:.3}->{sidebar_edge_after:.3}"
    );
    assert!(
        shell_change > 0.20,
        "venue modal did not visibly scrim/blur the production shell: {shell_change:.3}"
    );
    assert!(
        browse_create_change > 0.02,
        "browse and create rendered as the same route: {browse_create_change:.3}"
    );
    assert!(
        error_change > 0.005,
        "loading and error rendered as the same route: {error_change:.3}"
    );
    assert!(
        shell_edge_after < shell_edge_before,
        "shell backdrop did not reduce titlebar edge energy: {shell_edge_before:.3}->{shell_edge_after:.3}"
    );
    assert!(
        sidebar_edge_after < sidebar_edge_before,
        "sidebar backdrop did not reduce titlebar edge energy: {sidebar_edge_before:.3}->{sidebar_edge_after:.3}"
    );
    for path in card_paths {
        assert!(
            luma_range(&pixels(path)) > 24,
            "venue route card is a flat placeholder"
        );
    }
    assert!(
        differing_fraction(&loading_pixels, &error_loading_pixels) < 0.02,
        "loading paint changed substantially between route outcomes"
    );
}
