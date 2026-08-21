//! Inter, embedded.
//!
//! Inter is Luma's UI font and is *not* a macOS system font, so without this
//! the text system silently falls back to a different face. The screenshot
//! harness cares because every typography comparison becomes noise; the app
//! cares because it would ship the wrong typeface. Both call [`install`], and
//! the two TTFs are the same files `harness/fonts.css` `@font-face`s on the
//! web side (rsms/inter v4.1) — one set of outlines for all three renderers.

use std::borrow::Cow;

use gpui::App;

/// The family name to pass to `.font_family(…)`.
pub const FAMILY: &str = "Inter";

/// The family Tailwind's `font-mono` resolves to on WebKit/macOS — the face
/// behind every numeric readout (a slider's value, a plot's axis labels).
/// Not embedded: it is a system face on the platform this app ships to, and
/// the web side inherits the same one.
pub const MONO: &str = "SF Mono";

/// Register Inter with the app's text system. Call once, at startup, before
/// opening a window.
///
/// # Panics
///
/// If the embedded TTFs fail to parse, which would mean the binary itself is
/// corrupt — there is no runtime recovery from that.
pub fn install(cx: &App) {
    cx.text_system()
        .add_fonts(vec![
            Cow::Borrowed(include_bytes!("../../../../harness/fonts/Inter-Regular.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../../../../harness/fonts/Inter-Bold.ttf").as_slice()),
        ])
        .expect("failed to load embedded Inter");
}
