//! Pixel mode: the same headless app, with real glyphs and a real GPU.
//!
//! The spec for this harness assumed pixel mode would need a display server —
//! Xvfb and a software rasterizer. At the pinned gpui rev it needs neither.
//! `HeadlessAppContext` is `TestPlatform` — the same deterministic dispatcher
//! headless mode uses, the same absence of a window server — with two things
//! plugged in: a real `PlatformTextSystem`, so text is measured with its true
//! metrics instead of the noop system's stand-ins, and a
//! `PlatformHeadlessRenderer`, which is what makes `render_to_image` possible.
//!
//! So pixel mode is not a second architecture. It is the same backend with
//! two better parts, and the only call it adds is [`screenshot`].
//!
//! It sits behind the `pixel` feature because obtaining those two parts means
//! linking the platform crate and creating a GPU device, and a `cargo test`
//! that quietly did that would not be a headless test any more.
//!
//! Only macOS has a headless renderer at the pinned rev; elsewhere
//! `current_headless_renderer` returns `None` and `app.screenshot()` reports
//! that there is no renderer rather than pretending.

use std::sync::{Arc, OnceLock};

use gpui::{App, AppContext as _, Bounds, HeadlessAppContext, Pixels, Render, Size, Window};
use serde_json::{json, Value};

use crate::error::HarnessError;
use crate::pump::{Backend, Host};

/// The platform's text system, made once.
///
/// `current_platform` is not safe to call twice in a process — the second call
/// aborts — so the one thing we want out of it is taken on first use and kept.
/// `Arc<dyn PlatformTextSystem>` is `Send + Sync`, so the `Rc<dyn Platform>`
/// it came from can be dropped immediately and never crosses a thread.
fn text_system() -> Arc<dyn gpui::PlatformTextSystem> {
    static TEXT_SYSTEM: OnceLock<Arc<dyn gpui::PlatformTextSystem>> = OnceLock::new();
    TEXT_SYSTEM
        // `headless: true` — we want the platform's text system, not its event
        // loop, and asking for the windowing half would want the main thread.
        .get_or_init(|| gpui_platform::current_platform(true).text_system())
        .clone()
}

pub(crate) fn open<V: Render>(
    size: Size<Pixels>,
    build: impl FnOnce(&mut Window, &mut App) -> V + 'static,
) -> Host {
    let mut cx = HeadlessAppContext::with_platform(
        text_system(),
        Arc::new(gpui_component_assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_component::init(cx);
        luma_ui::fonts::install(cx);
    });
    let handle = cx
        .open_window(size, |window, cx| {
            let view = build(window, cx);
            cx.new(|_| view)
        })
        .expect("failed to open the headless window");

    // One directory per process, so a `reset` cannot overwrite a shot a script
    // is still holding the path to.
    let shots = std::env::temp_dir().join(format!("gpui-agent-{}", std::process::id()));
    std::fs::create_dir_all(&shots).ok();

    Host::Pixel {
        cx,
        window: handle.into(),
        shots,
    }
}

/// Capture the window, optionally cropped to one node's box, and write a PNG.
///
/// A path rather than the bytes: a screenshot is hundreds of kilobytes, and
/// pushing that through the interpreter as base64 would spend the model's
/// whole context on a picture of one button.
pub(crate) fn screenshot(
    backend: &mut Backend,
    crop: Option<Bounds<Pixels>>,
) -> Result<Value, HarnessError> {
    let (image, scale) =
        backend.in_window(|window, _| (window.render_to_image(), window.scale_factor()));
    let image =
        image.map_err(|error| HarnessError::BadCall(format!("screenshot failed: {error}")))?;

    // Node bounds are logical; the captured frame is physical.
    let image = match crop {
        None => image,
        Some(bounds) => {
            let at = |value: Pixels| (f32::from(value) * scale).round().max(0.) as u32;
            image::imageops::crop_imm(
                &image,
                at(bounds.origin.x).min(image.width()),
                at(bounds.origin.y).min(image.height()),
                at(bounds.size.width),
                at(bounds.size.height),
            )
            .to_image()
        }
    };

    let Host::Pixel { shots, .. } = backend.host() else {
        unreachable!("only the pixel host reaches here");
    };
    let path = shots.join(format!(
        "{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    image
        .save(&path)
        .map_err(|error| HarnessError::BadCall(format!("could not write the shot: {error}")))?;
    Ok(json!({
        "path": path.display().to_string(),
        "width": image.width(),
        "height": image.height(),
    }))
}
