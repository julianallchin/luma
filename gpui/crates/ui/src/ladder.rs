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

/// The status hues, and the only place a color means something rather than
/// placing a surface. Tailwind's `emerald-500` / `amber-500` / `rose-500`,
/// which is what the web app reaches for on a coverage dot or a progress ring.
pub fn status_ok() -> Rgba {
    rgb(0x10b981)
}

/// See [`status_ok`].
pub fn status_warn() -> Rgba {
    rgb(0xf59e0b)
}

/// See [`status_ok`].
pub fn status_bad() -> Rgba {
    rgb(0xf43f5e)
}

/// The hue a graph port and its wire carry, keyed by the wire spelling of
/// `PortType`. One hue per signal kind is the graph editor's whole legend —
/// the second place after the status dots where color means something rather
/// than placing a surface.
///
/// Keyed by string rather than by the enum because that enum lives in Luma's
/// core, which this crate deliberately does not depend on. The caller matches
/// exhaustively on `PortType` to produce the key, so a new variant is a
/// compile error there and lands here as [`default_port`] until it is named.
/// Mirrors `PORT_TYPE_COLORS` in `src/shared/lib/react-flow/types.ts`.
pub fn port(port_type: &str) -> Rgba {
    match port_type {
        "Intensity" => rgb(0xf59e0b),
        "Audio" => rgb(0x3b82f6),
        "BeatGrid" => rgb(0x10b981),
        "Series" => rgb(0x8b5cf6),
        "Color" => rgb(0xec4899),
        "Signal" => rgb(0x22d3ee),
        "Selection" => rgb(0xc084fc),
        "Events" => rgb(0xef4444),
        "Stops" => rgb(0xf472b6),
        _ => default_port(),
    }
}

/// The hue of a port whose type [`port`] does not know — `gray-500`, as on the
/// web side.
pub fn default_port() -> Rgba {
    rgb(0x6b7280)
}

/// `text-destructive` — failure prose, not a surface. Softer than
/// [`status_bad`] because it is a whole line of text rather than a 6px dot.
pub fn danger() -> Rgba {
    rgb(0xf87171)
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
