//! The sign-in screen against a real renderer.
//!
//! The screen is raised by a stored session that no longer proves anyone, so
//! that is what the fixture writes. What the pixels are read for is that it is
//! a **state** and not a dialog: one centred column on the app's own ground,
//! with no card, band or box drawn around it.

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

/// Whether every pixel across one row is the same colour — what "nothing is
/// behind this screen" looks like from the outside. A card would put its edge,
/// its fill or its shadow somewhere along a row beside the column.
fn row_is_uniform(image: &image::RgbaImage, y: u32) -> bool {
    let first = image.get_pixel(0, y);
    (0..image.width()).all(|x| image.get_pixel(x, y) == first)
}

fn read_bounds(bounds: &Value) -> (f32, f32, f32, f32) {
    let read = |key: &str| bounds[key].as_f64().expect("a bounds number") as f32;
    (read("x"), read("y"), read("width"), read("height"))
}

#[test]
fn the_sign_in_screen_is_one_centred_column_on_bare_ground() {
    let dir = fixture_dir();
    let mut harness = harness(&dir);
    let result = harness.exec(
        &support::script(
            r#"
            const gate = until("the sign-in screen", (s) =>
                s.find({ role: "text", label: "Sign in to Luma" }) !== undefined);
            ({ shot: app.screenshot().path,
               title: gate.find({ role: "text", label: "Sign in to Luma" }).bounds,
               email: gate.find({ role: "input", label: "Email" }).bounds,
               primary: gate.find({ role: "button", label: "Continue" }).bounds,
               offline: gate.find({ role: "button", label: "Work offline" }).bounds })
        "#,
        ),
        GPU_LIVENESS_TIMEOUT,
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // Only the Email route is driven here: reaching the Code route means a
    // real `send_login_code`, and the Supabase URL this binary is built with is
    // production's — a pixel test must not mail a stranger a code.
    let full = preserve(
        out["shot"].as_str().expect("a full-window shot"),
        "signin.png",
    );

    let (tx, ty, tw, _) = read_bounds(&out["title"]);
    let (ex, _, ew, _) = read_bounds(&out["email"]);
    let (px_, py, pw, ph) = read_bounds(&out["primary"]);
    let (ox, oy, ow, oh) = read_bounds(&out["offline"]);

    for (name, x, w) in [
        ("title", tx, tw),
        ("field", ex, ew),
        ("primary", px_, pw),
        ("secondary", ox, ow),
    ] {
        assert!(
            ((x + w / 2.0) - WINDOW.0 / 2.0).abs() <= 1.5,
            "the {name} is centred in the window: x = {x}, width = {w}"
        );
    }

    // The capsule tier, as declared: one width, one height, for every capsule.
    for (name, w, h) in [("primary", pw, ph), ("secondary", ow, oh)] {
        assert!(
            (w - luma_ui::pill::WIDTH).abs() <= 1.0 && (h - luma_ui::pill::HEIGHT).abs() <= 1.0,
            "the {name} capsule is {w}x{h}, not the tier's {}x{}",
            luma_ui::pill::WIDTH,
            luma_ui::pill::HEIGHT
        );
    }
    assert!(
        (ew - luma_ui::pill::WIDTH).abs() <= 1.0,
        "the field is the column's width: {ew}"
    );
    assert!(
        ((oy - (py + ph)) - luma_ui::pill::GAP).abs() <= 1.5,
        "the two capsules are one gap apart: {} vs {}",
        oy - (py + ph),
        luma_ui::pill::GAP
    );

    // The whole column clears the drag band and its controls.
    assert!(
        ty >= luma_ui::dialog::TITLEBAR_CLEARANCE - 1.0,
        "the column clears the window's own chrome: y = {ty}"
    );

    // No card: the ground beside the column is unbroken all the way across,
    // both above the title and between the two capsules.
    let window = image::open(&full)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", full.display()))
        .to_rgba8();
    let scale = window.height() as f32 / WINDOW.1;
    for row in [
        (ty - 10.0) * scale,
        (py + ph + luma_ui::pill::GAP / 2.0) * scale,
    ] {
        let row = row as u32;
        assert!(
            row_is_uniform(&window, row),
            "the ground is unbroken at y = {row} — something is drawing a box"
        );
    }
}
