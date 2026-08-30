//! Pixel proof for every production venue-dialog route.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;
use support::session;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{px, size, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;

const LAST_VENUE: &str = "last-venue";

async fn seed(dir: &Path, remembered: bool) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open pixel fixture app database");
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', ?, 'Pixel Venue')")
        .bind(session::PRINCIPAL)
        .execute(&db.0)
        .await
        .expect("failed to seed pixel venue");
    session::signed_in(dir).await;
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
            call_timeout: GPU_LIVENESS_TIMEOUT,
            runtime: support::runtime(dir),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start venue pixel harness")
}

fn run(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), GPU_LIVENESS_TIMEOUT);
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

/// The shared diff at the shared noise floor — see `support::image`.
fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage) -> f32 {
    support::image::differing_fraction(left, right, support::image::CHANNEL_NOISE)
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

// ---------------------------------------------------------------------------
// Frame cost
// ---------------------------------------------------------------------------

/// The venue dialog with motion left **on**, for measuring rather than
/// asserting. [`support::runtime`] snaps every animation so the gates above
/// read final geometry without racing a slide; a cost measurement needs the
/// slide it skips.
fn paced_harness(dir: &Path) -> Harness {
    let root: gpui_agent::RootFactory = Arc::new(move |window: &mut Window, cx: &mut App| {
        luma_app::init(cx);
        let library = luma_app::Library::open().expect("failed to open pixel library");
        let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
        cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
            .into()
    });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            window_size: size(px(1280.0), px(800.0)),
            call_timeout: GPU_LIVENESS_TIMEOUT,
            runtime: luma_ui::runtime::Runtime {
                config_dir: Some(dir.to_path_buf()),
                reduced_motion: false,
                motion_scale: 1.0,
                ..luma_ui::runtime::Runtime::default()
            },
            ..Config::default()
        },
        root,
    )
    .expect("failed to start paced venue harness")
}

fn percentile(sample: &mut Vec<f64>, fraction: f64) -> f64 {
    sample.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if sample.is_empty() {
        return f64::NAN;
    }
    sample[(((sample.len() - 1) as f64) * fraction).round() as usize]
}

fn summarize(frames: &[Value], from: u64, to: u64, label: &str) -> f64 {
    let mut draw: Vec<f64> = Vec::new();
    let mut parked: Vec<f64> = Vec::new();
    for frame in frames {
        let number = frame["frame"].as_u64().unwrap();
        if number > from && number <= to {
            draw.push(frame["drawMs"].as_f64().unwrap());
            parked.push(frame["parkedMs"].as_f64().unwrap());
        }
    }
    let count = draw.len();
    let mean = draw.iter().sum::<f64>() / count.max(1) as f64;
    let p50 = percentile(&mut draw.clone(), 0.50);
    let p95 = percentile(&mut draw, 0.95);
    println!(
        "{label:<22} n={count:<4} drawMs mean={mean:6.2} p50={p50:6.2} p95={p95:6.2}  \
         parkedMs p50={:5.2}",
        percentile(&mut parked, 0.50)
    );
    p50
}

/// Per-frame CPU cost of the production venue dialog: the shell behind it, the
/// settled card over that shell, and the browse→create morph.
///
/// `#[ignore]` because it is an instrument, not a gate. The shell row is the
/// baseline; the gap to the settled row is what the card and its frosted
/// backdrop cost every frame they are up, and the gap to the morphing row is
/// what the animation adds on top.
#[test]
#[ignore = "measurement, not a gate"]
fn venue_dialog_frame_cost() {
    let dir = fixture_dir("frame-cost", true);
    let mut harness = paced_harness(&dir);
    let out = run(
        &mut harness,
        r#"
        until("the restored venue shell", (s) =>
            s.find({ role: "button", label: "Pixel Venue" }) !== undefined);
        const shellFrom = app.frames(1).frame;
        app.frames(24, { waitMs: 16 });
        const shellTo = app.frames(1).frame;

        // Re-snapshot: the handle from `until` is 24 frames stale by now.
        app.click(app.snapshot().find({ role: "button", label: "Pixel Venue" }));
        until("the browse route", (s) =>
            s.find({ role: "input", label: "Search venues…" }) !== undefined);
        app.frames(12, { waitMs: 16 });
        const settledFrom = app.frames(1).frame;
        app.frames(24, { waitMs: 16 });
        const settledTo = app.frames(1).frame;

        app.click(app.snapshot().find({ role: "button", label: "Create venue" }));
        app.frames(24, { waitMs: 16 });
        const morphTo = app.frames(1).frame;
        ({ shellFrom, shellTo, settledFrom, settledTo, morphTo,
           frames: app.timings().frames })
        "#,
    );
    let frames = out["frames"].as_array().unwrap();
    let number = |key: &str| out[key].as_u64().unwrap();
    println!("\n--- venue dialog frame cost (pixel, motion on, 1280x800) ---");
    let shell = summarize(
        frames,
        number("shellFrom"),
        number("shellTo"),
        "shell, no dialog",
    );
    let settled = summarize(
        frames,
        number("settledFrom"),
        number("settledTo"),
        "settled dialog",
    );
    let morphing = summarize(
        frames,
        number("settledTo"),
        number("morphTo"),
        "morphing dialog",
    );
    println!(
        "dialog costs {:.2} ms/frame over the shell",
        settled - shell
    );
    println!(
        "morph adds   {:.2} ms/frame over settled\n",
        morphing - settled
    );

    // `drawMs` is the scene build and stops at the renderer's door — see
    // `app.timings()`. The frosted backdrop is a GPU pass, so the only way to
    // see it from here is wall clock across a free-running pump.
    println!("--- wall clock, free-running pump (includes the GPU) ---");
    let create = pump_wall(&mut harness, 60, "create route up");
    run(
        &mut harness,
        r#"app.key("escape"); app.frames(20, { waitMs: 16 });"#,
    );
    let none = pump_wall(&mut harness, 60, "no dialog");
    println!("dialog costs {:.2} ms/frame of wall\n", create - none);
}

/// Wall-clock milliseconds per frame across a free-running pump, which is the
/// only number here that has the GPU in it.
fn pump_wall(harness: &mut Harness, frames: usize, label: &str) -> f64 {
    // Once to warm whatever the first frame allocates, then the measured run.
    run(harness, &format!("app.frames({frames})"));
    let start = Instant::now();
    run(harness, &format!("app.frames({frames})"));
    let per = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    println!("{label:<22} wall {per:6.2} ms/frame over {frames} frames");
    per
}

/// Seed enough venues that the browse route is a real list rather than a line.
///
/// The one-venue fixture the gates use is the wrong shape for a cost
/// measurement: [`morph::card`] rebuilds every layer's content every frame, so
/// what a morph actually costs is proportional to the content it is carrying,
/// and a card holding one row carries almost none.
async fn seed_many(dir: &Path, venues: usize) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open venue cost fixture database");
    for index in 0..venues {
        sqlx::query("INSERT INTO venues (id, uid, name, description) VALUES (?, ?, ?, ?)")
            .bind(format!("venue-{index:04}"))
            .bind(session::PRINCIPAL)
            .bind(format!("Pixel Venue {index:04}"))
            .bind(format!(
                "A seeded room for frame-cost measurement, number {index}"
            ))
            .execute(&db.0)
            .await
            .expect("failed to seed venue");
    }
    session::signed_in(dir).await;
    let state = luma_lib::database::local::state::init_state_db_at(dir)
        .await
        .expect("failed to open venue cost fixture state database");
    luma_lib::database::local::auth::set_session_item(&state.0, LAST_VENUE, "venue-0000")
        .await
        .expect("failed to seed venue preference");
    db.0.close().await;
    state.0.close().await;
}

fn heavy_fixture_dir(venues: usize) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("luma-gpui-venue-cost-heavy-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("failed to create venue cost fixture directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start venue cost fixture runtime")
        .block_on(seed_many(&dir, venues));
    dir
}

/// The same measurement over a card carrying a long list, which is the shape
/// every content-rich dialog in the app actually has.
#[test]
#[ignore = "measurement, not a gate"]
fn venue_dialog_frame_cost_with_a_long_list() {
    const VENUES: usize = 200;
    let dir = heavy_fixture_dir(VENUES);
    let mut harness = paced_harness(&dir);
    let out = run(
        &mut harness,
        r#"
        until("the restored venue shell", (s) =>
            s.find({ role: "button", label: "Pixel Venue 0000" }) !== undefined);
        const shellFrom = app.frames(1).frame;
        app.frames(24, { waitMs: 16 });
        const shellTo = app.frames(1).frame;

        app.click(app.snapshot().find({ role: "button", label: "Pixel Venue 0000" }));
        until("the browse route", (s) =>
            s.find({ role: "input", label: "Search venues…" }) !== undefined);
        app.frames(12, { waitMs: 16 });
        const settledFrom = app.frames(1).frame;
        app.frames(24, { waitMs: 16 });
        const settledTo = app.frames(1).frame;

        // Only the flight itself: a wider window averages in the create route,
        // which is a handful of controls and reads as the morph getting cheaper.
        app.click(app.snapshot().find({ role: "button", label: "Create venue" }));
        app.frames(12, { waitMs: 16 });
        const flightTo = app.frames(1).frame;
        app.frames(24, { waitMs: 16 });
        const afterTo = app.frames(1).frame;
        ({ shellFrom, shellTo, settledFrom, settledTo, flightTo, afterTo,
           frames: app.timings().frames })
        "#,
    );
    let frames = out["frames"].as_array().unwrap();
    let number = |key: &str| out[key].as_u64().unwrap();
    println!("\n--- venue dialog frame cost, {VENUES} venues (pixel, motion on, 1280x800) ---");
    let shell = summarize(
        frames,
        number("shellFrom"),
        number("shellTo"),
        "shell, no dialog",
    );
    let browse = summarize(
        frames,
        number("settledFrom"),
        number("settledTo"),
        "browse route (list)",
    );
    let flight = summarize(
        frames,
        number("settledTo"),
        number("flightTo"),
        "browse->create flight",
    );
    let create = summarize(
        frames,
        number("flightTo"),
        number("afterTo"),
        "create route (settled)",
    );
    println!(
        "the list costs  {:.2} ms/frame over the shell",
        browse - shell
    );
    println!(
        "the flight costs {:.2} ms/frame over the list it is leaving",
        flight - browse
    );
    println!("…and lands on a route costing {create:.2} ms/frame\n");
}
