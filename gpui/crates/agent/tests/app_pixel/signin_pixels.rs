//! The sign-in screen against a real renderer.
//!
//! The screen is raised by a stored session that no longer proves anyone, so
//! that is what the fixture writes. What the pixels are read for is that it is
//! a **state** and not a dialog: the card is centred on the app's own ground
//! with nothing painted behind it, clear of the titlebar band, and carrying
//! real content rather than one flat plate.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;
use support::session::{self, Stored};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{px, size, AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;

const WINDOW: (f32, f32) = (900.0, 650.0);

fn fixture_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-signin-pixels-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("failed to create the sign-in pixel directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start the fixture runtime")
        .block_on(session::seed(&dir, Stored::Unproven));
    dir
}

fn harness(dir: &Path) -> Harness {
    let root: gpui_agent::RootFactory =
        Arc::new(move |window: &mut Window, cx: &mut App| -> AnyView {
            luma_app::init(cx);
            let library = luma_app::Library::open().expect("failed to open the pixel library");
            let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
            cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
                .into()
        });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            window_size: size(px(WINDOW.0), px(WINDOW.1)),
            call_timeout: GPU_LIVENESS_TIMEOUT,
            runtime: support::runtime(dir),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the sign-in pixel harness")
}

fn capture_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-signin-review".into()),
    );
    fs::create_dir_all(&dir).expect("failed to create the sign-in capture directory");
    dir
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = capture_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("failed to preserve {}: {error}", destination.display()));
    println!("sign-in capture {}", destination.display());
    destination
}

fn luma_range(image: &image::RgbaImage) -> u8 {
    let mut low = u8::MAX;
    let mut high = u8::MIN;
    for pixel in image.pixels() {
        let luma = ((u16::from(pixel[0]) * 30
            + u16::from(pixel[1]) * 59
            + u16::from(pixel[2]) * 11)
            / 100) as u8;
        low = low.min(luma);
        high = high.max(luma);
    }
    high - low
}

/// Whether every pixel across one row is the same colour — what "nothing is
/// behind this screen" looks like from the outside. A dimmed shell would put a
/// sidebar seam, a band or a tab chip somewhere along it.
fn row_is_uniform(image: &image::RgbaImage, y: u32) -> bool {
    let first = image.get_pixel(0, y);
    (0..image.width()).all(|x| image.get_pixel(x, y) == first)
}

#[test]
fn the_sign_in_screen_centres_its_card_on_the_app_ground() {
    let dir = fixture_dir();
    let mut harness = harness(&dir);
    let result = harness.exec(
        &support::script(
            r#"
            const gate = until("the sign-in screen", (s) =>
                s.find({ role: "text", label: "Sign in to Luma" }) !== undefined);
            const card = gate.find({ role: "card", label: "Sign-in card" });
            ({ full: app.screenshot().path,
               card: app.screenshot({ node: card }).path,
               bounds: card.bounds })
        "#,
        ),
        GPU_LIVENESS_TIMEOUT,
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let full = preserve(
        out["full"].as_str().expect("a full-window shot"),
        "signin.png",
    );
    let card = preserve(
        out["card"].as_str().expect("a card shot"),
        "signin-card.png",
    );

    let bounds = &out["bounds"];
    let read = |key: &str| bounds[key].as_f64().expect("a bounds number") as f32;
    let (x, y, w, h) = (read("x"), read("y"), read("width"), read("height"));
    assert!(
        ((x + w / 2.0) - WINDOW.0 / 2.0).abs() <= 1.0,
        "the card is centred in the window: x = {x}, width = {w}"
    );
    assert!(
        y >= luma_ui::dialog::TITLEBAR_CLEARANCE - 1.0,
        "the card clears the drag band and its controls: y = {y}"
    );
    assert!(
        x >= 0.0 && x + w <= WINDOW.0 && y + h <= WINDOW.1,
        "the card stays inside the viewport: {x},{y} {w}x{h}"
    );

    // Not a flat placeholder: header band, body copy and footer legend give the
    // card a real value spread even in a monochrome tier.
    let image = image::open(&card)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", card.display()))
        .to_rgba8();
    assert!(
        luma_range(&image) > 24,
        "the card paints content, not one grey plate"
    );

    // A state, not a dialog: the ground beside the card is the app's own,
    // unbroken — no scrim over a shell, because there is no shell.
    let window = image::open(&full)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", full.display()))
        .to_rgba8();
    let scale = window.height() as f32 / WINDOW.1;
    let row = ((y - 8.0) * scale) as u32;
    assert!(
        row_is_uniform(&window, row),
        "the ground above the card is unbroken at y = {row}"
    );
}
