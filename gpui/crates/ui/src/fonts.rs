//! Inter, embedded.
//!
//! Inter is Luma's UI font and is *not* a macOS system font, so without this
//! the text system silently falls back to a different face. The screenshot
//! harness cares because every typography comparison becomes noise; the app
//! cares because it would ship the wrong typeface. Both call [`install`], and
//! the two TTFs are the same files `harness/fonts.css` `@font-face`s on the
//! web side (rsms/inter v4.1) — one set of outlines for all three renderers.

use std::borrow::Cow;

use gpui::{App, Font, FontFallbacks, SharedString};

/// The family name to pass to `.font_family(…)`.
pub const FAMILY: &str = "Inter";

/// The face behind every numeric readout (a slider's value, a plot's axis
/// labels): the first *nameable* family in the `ui-monospace, SFMono-Regular,
/// Menlo, monospace` stack the web side sets. The two ahead of it are not
/// families a text system can be asked for — SF Mono ships as the reserved
/// `.SF NS Mono` and matches nothing under its marketing name — and a family
/// that fails to match falls back to the UI face silently, which renders a
/// proportional number where a tabular one belongs.
pub const MONO: &str = "Menlo";

/// Prefer the platform's color emoji face before generic symbol fallback.
pub fn font(family: impl Into<SharedString>) -> Font {
    static FALLBACKS: std::sync::LazyLock<FontFallbacks> = std::sync::LazyLock::new(|| {
        let family = if cfg!(target_os = "macos") {
            "Apple Color Emoji"
        } else if cfg!(target_os = "windows") {
            "Segoe UI Emoji"
        } else {
            "Noto Color Emoji"
        };
        FontFallbacks::from_fonts(vec![family.to_owned()])
    });
    Font {
        fallbacks: Some(FALLBACKS.clone()),
        ..gpui::font(family)
    }
}

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
