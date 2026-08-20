//! The grey ladder, once. Every GPUI surface — harness fixture and real app
//! screen alike — reads its color from here, so the port has the same single
//! source of truth the web side has in `src/App.css`. If anything above this
//! module hardcodes an `0x…`, that's the bug.
//!
//! Six greys carry the entire hierarchy and hue appears only for *meaning*
//! ([`primary`]). Depth between two adjacent surfaces is a value step or a
//! slice of [`trim`] — never a shadow, a gradient, or a rounded corner.

use gpui::{rgb, rgba, Hsla, Rgba};

/// `--titlebar-background` — the deepest plane, top bar only.
pub fn titlebar_background() -> Rgba {
    rgb(0x0e0e0e)
}

/// `--gutter` — heavier gap / empty-area contrast, one notch deeper than
/// [`trim`]. The welcome screen's ground.
pub fn gutter() -> Rgba {
    rgb(0x191919)
}

/// `--trim` — the fine gap between sections. A separator, never a fill.
pub fn trim() -> Rgba {
    rgb(0x212121)
}

/// `--background` / `--card` — app body and card surfaces.
pub fn background() -> Rgba {
    rgb(0x272727)
}

/// `--stripe` — the alternating list-row stripe, paired with [`background`]
/// (`--card`) on the even rows.
pub fn stripe() -> Rgba {
    rgb(0x2b2b2b)
}

/// `--control` — control resting fill (buttons, inputs, select triggers).
pub fn control() -> Rgba {
    rgb(0x2e2e2e)
}

/// `--control-border` — the definition line around every control.
pub fn control_border() -> Rgba {
    rgb(0x080808)
}

/// `--hover` — universal hover fill.
pub fn hover() -> Rgba {
    rgb(0x3b3b3b)
}

/// `--border` — the hairline between surfaces that aren't controls.
pub fn border() -> Rgba {
    rgb(0x3f3f3f)
}

/// `--input` — the recessed core of an input-shaped control (checkbox fill).
pub fn input() -> Rgba {
    rgb(0x1a1a1a)
}

/// `--primary` — the one accent, used only for meaning (checked, selected).
pub fn primary() -> Rgba {
    rgb(0x88c0d0)
}

/// `--foreground`.
pub fn foreground() -> Rgba {
    rgb(0xe4e4e4)
}

/// `--muted-foreground` — placeholders and de-emphasised text.
pub fn muted_foreground() -> Rgba {
    rgb(0x777777)
}

/// `text-foreground/90` — the resting label color on controls.
pub fn foreground_90() -> Rgba {
    rgba(0xe4e4e4e6)
}

/// `--foreground` at an arbitrary alpha, for the places the web side stacks a
/// `/90` text color with an `opacity-50` icon.
pub fn foreground_alpha(alpha: f32) -> Hsla {
    let mut color: Hsla = foreground().into();
    color.a = alpha;
    color
}
