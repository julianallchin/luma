//! The chrome tier's paint: translucent surfaces, washes and hairlines.
//!
//! Adapted from zeron (MIT, © 2026 Wing) — `crates/ui/src/theme.rs`.
//!
//! Luma paints two surfaces, and which one a component belongs to is decided by
//! *what it is*, not by which crate it lives in:
//!
//! - [`crate::ladder`] — **instrument surfaces.** Tab contents, controls,
//!   tables, the graph canvas, the timeline. Square, no motion, the six greys.
//! - [`glass`] — **chrome surfaces.** Titlebar, sidebar, tab strip, panel
//!   seams, the thread column, overlays. Translucent, [`crate::motion`]'s
//!   curves, comet's radii.
//!
//! It lived behind the `luma-md` / `luma-chat` crate boundary while the chat
//! was the only comet-language surface in the app. The shell itself is chrome
//! now, so the boundary is a named tier rather than a dependency edge — see
//! `docs/specs/comet-shell.md` §5.
//!
//! Motto, kept from the source: **numbers drive layout, colors are paint.**
//! Every layout constant above this module is a plain number and none of them
//! depend on which color is painted.
//!
//! # One appearance
//!
//! zeron ships two designed appearances. Luma is dark-only — there is no light
//! mode to design against, and shipping the unreachable half would be a second
//! palette nobody paints. [`generation`] survives that simplification on
//! purpose: the markdown renderer's cross-frame cache bakes a resolved [`Hsla`]
//! into every `TextRun`, so it needs *some* token saying "the palette moved".

use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{hsla, Hsla, WindowBackgroundAppearance};

/// Monotonic id of the current palette.
static GENERATION: AtomicU32 = AtomicU32::new(0);

/// The palette's current generation. Anything caching a *resolved* color
/// compares this and drops everything when it moves.
#[must_use]
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed)
}

/// Announce that the palette changed, invalidating every resolved-color cache.
///
/// Nothing calls this yet; it is the one function a second appearance would
/// call, and naming it here is what keeps that change local.
pub fn invalidate() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Translucent **fill** ink for interactive states and chip plates.
///
/// Fills rest on transparent *white* at zero alpha, never on transparent
/// black: an opaque wash mid-fade flashes dark over these near-black planes.
#[must_use]
pub fn ink(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 1.0, alpha)
}

/// Translucent **hairline** ink for borders, dividers and rings. Separate from
/// [`ink`] because edges and fills scale in opposite directions when the field
/// brightens — the distinction is what makes a light mode a palette change.
#[must_use]
pub fn hairline(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 1.0, alpha)
}

/// Interactive-state wash: a softened [`ink`] that stops short of pure white,
/// so a hover plate reads as tinted glass rather than paint.
#[must_use]
pub fn wash(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 0.92, alpha)
}

/// Alpha of the standard modal backdrop.
pub const SCRIM_ALPHA: f32 = 0.60;

/// Coverage of the glass surfaces over whatever is behind the window.
///
/// macOS is the only platform where the compositor guarantees a blur behind a
/// translucent window; everywhere else a merely *transparent* panel shows the
/// raw desktop through it, so the glass tier resolves opaque and every call
/// site below keeps painting without knowing which happened.
pub const GLASS_ALPHA: f32 = if cfg!(target_os = "macos") { 0.80 } else { 1.0 };

/// The chrome's own ground. On the glass platform this is `grey(8)` at
/// [`GLASS_ALPHA`], so the plane behind the window tints it; elsewhere it is
/// that grey, opaque.
#[must_use]
pub fn glass() -> Hsla {
    grey(8).opacity(GLASS_ALPHA)
}

/// Hover wash for a row sitting directly on [`glass`]. Softer than an opaque
/// element hover: over a translucent ground a full-strength wash paints out the
/// tint that is the point of the tier.
#[must_use]
pub fn glass_hover() -> Hsla {
    wash(0.11)
}

/// A card raised off the glass — code block, tool chip, header strip.
///
/// A white *wash* and not a grey plate, which is what "raised" has to mean on
/// this tier: an opaque grey would punch a slab through the translucency, and a
/// translucent grey lands on the ground's own tone and disappears. The wash
/// lifts whatever is behind it instead, so the card reads as raised at every
/// coverage [`GLASS_ALPHA`] can take.
///
/// One value, used by every raised plate on the surface — a second card fill
/// would be a second answer to the same question.
#[must_use]
pub fn card_bg() -> Hsla {
    ink(0.05)
}

/// How the window must be composited for [`glass`] to mean anything.
///
/// An element cannot apply this itself — background appearance is a *window*
/// property. This is the value that window's owner passes to
/// `Window::set_background_appearance`, stated here so the alpha above and the
/// compositing that makes it visible are one decision in one place.
///
/// It must be re-applied after every theme swap: gpui's macOS backend tears
/// the `NSVisualEffectView` out of the hierarchy whenever the value is
/// anything but `Blurred`.
#[must_use]
pub fn window_background_appearance() -> WindowBackgroundAppearance {
    if GLASS_ALPHA < 1.0 {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// Modal backdrop. Black: a scrim's job is to darken what is behind it.
#[must_use]
pub fn scrim(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, alpha)
}

/// An exact achromatic tone from an 8-bit channel value (`grey(13)` ≡
/// `#0d0d0d`) — the surfaces are sampled from screenshots, not generated.
#[must_use]
pub fn grey(value: u8) -> Hsla {
    hsla(0.0, 0.0, f32::from(value) / 255.0, 1.0)
}

/// A neutral (chroma 0) oklch tone. Chroma 0 means `r == g == b` exactly, so
/// this skips the hue math, which would otherwise leave float-noise saturation.
#[must_use]
pub fn neutral(lightness: f32) -> Hsla {
    let [v, _, _] = oklch_to_srgb(lightness, 0.0, 0.0);
    hsla(0.0, 0.0, v, 1.0)
}

/// An oklch color in CSS notation (L 0..1, C, H in degrees).
#[must_use]
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Hsla {
    let [r, g, b] = oklch_to_srgb(l, c, h_deg);
    let (h, s, l) = rgb_to_hsl(r, g, b);
    hsla(h, s, l, 1.0)
}

/// oklch → sRGB, each component 0..1 and gamut-clipped.
///
/// Reference: Björn Ottosson's OKLab definition — the matrices CSS Color 4 uses.
fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> [f32; 3] {
    let h = h_deg.to_radians();
    let a = c * h.cos();
    let b = c * h.sin();

    // OKLab → LMS (cube roots undone)
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);

    // LMS → linear sRGB
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_93 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_4 * s3;
    let b = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;

    [gamma_encode(r), gamma_encode(g), gamma_encode(b)]
}

fn gamma_encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB (0..1 components) → HSL, all components 0..1 (gpui's convention).
fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let delta = max - min;
    if delta < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    (h, s, l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_matches_its_hex() {
        assert_eq!(grey(13).l, 13.0 / 255.0);
    }

    #[test]
    fn oklch_neutrals_are_achromatic() {
        assert_eq!(neutral(0.5).s, 0.0);
    }

    /// The cache-invalidation token has to actually move, or every
    /// resolved-color cache downstream is silently permanent.
    #[test]
    fn invalidating_the_palette_moves_the_generation() {
        let before = generation();
        invalidate();
        assert_ne!(generation(), before);
    }
}
