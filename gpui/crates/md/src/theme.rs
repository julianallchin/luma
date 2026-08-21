//! Adapted from zeron (MIT, © 2026 Wing) — crates/ui/src/theme.rs
//!
//! The chat surface's palette. **Not** Luma's brutalist ladder: this is the one
//! scoped style exception (`docs/specs/agent-chat-gpui.md` §0), and it lives
//! behind a crate boundary so that importing it from anywhere but `luma-md` /
//! `luma-chat` is a dependency error rather than a review catch.
//!
//! Motto, kept from the source: **numbers drive layout, colors are paint.**
//! Every layout constant in this crate is a plain number and none of them
//! depend on which color is painted.
//!
//! # One appearance
//!
//! zeron ships two designed appearances. Luma is dark-only — there is no light
//! mode to design against, and shipping the unreachable half would be a second
//! palette nobody paints. So [`Theme::dark`] is the only constructor and the
//! paint helpers below resolve directly.
//!
//! [`theme_generation`] survives that simplification on purpose: the markdown
//! renderer's cross-frame cache bakes a resolved [`Hsla`] into every `TextRun`,
//! so it needs *some* token saying "the palette moved". Keeping the counter is
//! what makes a future appearance switch a palette change rather than a hunt
//! through every cache key.

use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{hsla, Hsla, SharedString, WindowBackgroundAppearance};

use crate::syntax::TokenKind;

/// Monotonic id of the current palette.
///
/// Anything caching *resolved* colors is only valid for the palette that
/// produced them. Rather than thread the palette through every cache key, such
/// caches compare this counter and drop everything when it moves.
static THEME_GENERATION: AtomicU32 = AtomicU32::new(0);

/// The palette's current generation. Anything caching a *resolved* color
/// compares this and drops everything when it moves.
#[must_use]
pub fn theme_generation() -> u32 {
    THEME_GENERATION.load(Ordering::Relaxed)
}

/// Announce that the palette changed, invalidating every resolved-color cache.
///
/// Nothing calls this yet; it is the one function a second appearance would
/// call, and naming it here is what keeps that change local.
pub fn invalidate_palette() {
    THEME_GENERATION.fetch_add(1, Ordering::Relaxed);
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

// -- the glass tier ----------------------------------------------------------

/// Coverage of the glass surfaces over whatever is behind the window.
///
/// macOS is the only platform where the compositor guarantees a blur behind a
/// translucent window; everywhere else a merely *transparent* panel shows the
/// raw desktop through it, so the glass tier resolves opaque and every call
/// site below keeps painting without knowing which happened.
pub const GLASS_ALPHA: f32 = if cfg!(target_os = "macos") { 0.80 } else { 1.0 };

/// The panel's own ground. On the glass platform this is `grey(8)` at
/// [`GLASS_ALPHA`], so the plane behind the panel tints it; elsewhere it is
/// that grey, opaque.
#[must_use]
pub fn glass() -> Hsla {
    grey(8).opacity(GLASS_ALPHA)
}

/// Hover wash for a row sitting directly on [`glass`]. Softer than
/// [`Theme::element_hover`]: over a translucent ground a full-strength wash
/// paints out the tint that is the point of the tier.
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
pub fn card_glass_bg() -> Hsla {
    ink(0.05)
}

/// How the window must be composited for [`glass`] to mean anything.
///
/// The panel cannot apply this itself — background appearance is a *window*
/// property and the panel is one element inside somebody's window. This is the
/// value that window's owner passes to `Window::set_background_appearance`,
/// stated here so the alpha above and the compositing that makes it visible
/// are one decision in one place.
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

/// Paint-only colors for fenced code.
///
/// A separate struct from [`Theme`] because these are the only tokens with a
/// *lookup* — [`Self::color`] is the whole seam between the lexer's closed
/// vocabulary and the palette, and it is the reason no other file in the crate
/// needs to know either half.
///
/// Hues are zeron's: indigo / emerald / amber / pink at 72% of their nominal
/// saturation, which is what keeps six tones on a near-black ground from
/// reading as six highlighter pens.
#[derive(Debug, Clone)]
pub struct SyntaxPalette {
    pub comment: Hsla,
    pub keyword: Hsla,
    pub string: Hsla,
    pub number: Hsla,
    pub constant: Hsla,
    pub type_name: Hsla,
    pub function: Hsla,
    pub property: Hsla,
    /// Identifiers and operators — code's body copy, and the majority of it.
    pub plain: Hsla,
}

impl SyntaxPalette {
    /// What one token paints in.
    #[must_use]
    pub fn color(&self, kind: TokenKind) -> Hsla {
        match kind {
            TokenKind::Comment => self.comment,
            TokenKind::Keyword => self.keyword,
            TokenKind::String => self.string,
            TokenKind::Number => self.number,
            TokenKind::Constant => self.constant,
            TokenKind::Type => self.type_name,
            TokenKind::Function => self.function,
            TokenKind::Property => self.property,
            TokenKind::Plain => self.plain,
        }
    }

    /// The one appearance, over [`Theme::dark`]'s code surface.
    #[must_use]
    pub fn dark() -> Self {
        let tone = |color: Hsla| Hsla {
            s: color.s * 0.72,
            ..color
        };
        let indigo = tone(oklch(0.673, 0.182, 276.935));
        let emerald = tone(oklch(0.765, 0.177, 163.223));
        let amber = tone(oklch(0.828, 0.189, 84.429));
        let pink = tone(oklch(0.718, 0.202, 349.761));
        Self {
            comment: neutral(0.60),
            keyword: indigo,
            string: emerald,
            number: amber,
            constant: emerald,
            type_name: amber,
            function: pink,
            property: amber,
            plain: neutral(0.86),
        }
    }
}

/// The chat surface's tokens.
///
/// A struct rather than free functions for the *palette* — a token is a design
/// decision with a name, and a call site that reaches for `oklch(…)` directly
/// has minted a second palette. The context-free helpers above are the
/// exception, and only because they are called from element builders that have
/// no theme in scope.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The transcript's plane: the deepest surface in the panel.
    pub bg: Hsla,
    /// Shell around the transcript — header, composer gutter.
    pub surface: Hsla,
    /// Raised pills and chips that sit proud of the panel.
    pub surface_raised: Hsla,
    /// A floating card: menu, popover.
    pub surface_overlay: Hsla,
    /// Hover wash for interactive rows and buttons.
    pub element_hover: Hsla,
    /// Active / pressed wash.
    pub element_active: Hsla,
    /// Hairline border.
    pub border: Hsla,
    /// Stronger border for focused or raised edges.
    pub border_strong: Hsla,
    /// Primary text.
    pub text: Hsla,
    /// Timestamps and secondary labels.
    pub text_muted: Hsla,
    /// Placeholders and disabled copy.
    pub text_faint: Hsla,
    /// The composer plate.
    pub input_bg: Hsla,
    /// Accent — indigo. Bullets, quote rails, selection.
    pub accent: Hsla,
    /// Stronger accent for fills that carry a label.
    pub accent_strong: Hsla,
    /// Errors, and the stop button.
    pub danger: Hsla,
    /// Amber: a tool call still running.
    pub warning: Hsla,
    /// Emerald: a tool call that succeeded.
    pub success: Hsla,
    /// Pink: the working indicator.
    pub busy: Hsla,
    /// Inline-code and code-block text.
    pub code_text: Hsla,
    /// The wash behind an inline-code pill.
    pub code_wash: Hsla,
    /// Fenced code's tokens. Paint only: a block with no highlighter, or in a
    /// language the lexer does not know, paints entirely in [`Self::code_text`]
    /// at exactly the same size and metrics.
    pub syntax: SyntaxPalette,
    /// Body face. Luma's own, not zeron's Geist — the fonts are separately
    /// licensed and this port does not carry them.
    pub font_sans: SharedString,
    pub font_mono: SharedString,
}

impl Theme {
    /// The one appearance. Surfaces are zeron's sampled greys.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            bg: grey(6),
            surface: grey(13),
            surface_raised: neutral(0.235),
            surface_overlay: grey(0x16),
            element_hover: hsla(0.0, 0.0, 0.92, 0.11),
            element_active: hsla(0.0, 0.0, 0.92, 0.16),
            border: hsla(0.0, 0.0, 1.0, 0.08),
            border_strong: hsla(0.0, 0.0, 1.0, 0.14),
            text: neutral(0.922),
            text_muted: neutral(0.708),
            text_faint: neutral(0.556),
            input_bg: hsla(0.0, 0.0, 1.0, 0.03),
            accent: oklch(0.673, 0.182, 276.935),
            accent_strong: oklch(0.585, 0.233, 277.117),
            danger: oklch(0.704, 0.191, 22.216),
            warning: oklch(0.828, 0.189, 84.429),
            success: oklch(0.765, 0.177, 163.223),
            busy: oklch(0.718, 0.202, 349.761),
            code_text: oklch(0.811, 0.111, 293.571),
            code_wash: oklch(0.702, 0.183, 293.541).opacity(0.12),
            syntax: SyntaxPalette::dark(),
            font_sans: luma_font_sans(),
            font_mono: system_mono().into(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Luma's UI face. Named here rather than taken from `luma-ui` because this
/// crate deliberately does not depend on the brutalist surface — see the
/// module docs.
fn luma_font_sans() -> SharedString {
    "Inter".into()
}

fn system_mono() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
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
    /// resolved-color cache in the crate is silently permanent.
    #[test]
    fn invalidating_the_palette_moves_the_generation() {
        let before = theme_generation();
        invalidate_palette();
        assert_ne!(theme_generation(), before);
    }
}
