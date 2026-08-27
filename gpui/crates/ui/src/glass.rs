//! The chrome tier's paint: translucent surfaces, washes and hairlines.
//!
//! Adapted from zeron (MIT, © 2026 Wing) — `crates/ui/src/theme.rs`.
//!
//! Luma paints two surfaces, and which one a component belongs to is decided by
//! *what it is*, not by which crate it lives in:
//!
//! - [`crate::ladder`] — **planes and instrument surfaces.** The shell's three
//!   structural planes, tab contents, controls, tables, the graph canvas, the
//!   timeline. Square, no motion, the ladder's greys.
//! - [`glass`] — **what floats.** Menus, popovers, overlay grounds, the washes
//!   and inks interactive states take on a translucent surface.
//!   [`crate::motion`]'s curves, comet's radii.
//!
//! It lived behind the `luma-md` / `luma-chat` crate boundary while the chat
//! was the only comet-language surface in the app. The shell itself is chrome
//! now, so the boundary is a named tier rather than a dependency edge — see
//! `docs/specs/comet-shell.md` §5.
//!
//! # One ladder, two coverages
//!
//! This tier does **not** mint tones of its own. Every surface here is a rung
//! of [`crate::ladder`] taken to a coverage — [`panel`] is the ladder's ground,
//! [`glass`] the chrome plane above it, [`overlay`] the apex above that — so
//! the two tiers read as one climb (`18 → 26 → 2d`) and there is exactly one
//! place a grey is written down. The tier decides *how much desktop shows
//! through*, never *what colour a surface is*.
//!
//! # What a plane may spend on the blur
//!
//! A translucent surface's brightness is set partly by whatever is behind the
//! window, so a coverage costs a plane some of its rung. Whether that is worth
//! paying is decided by **what the plane carries**, not by where it sits:
//!
//! - the window's one root fill is [`glass`], and the sidebar lifts off it with
//!   a [`tone_column`] wash rather than a plane of its own — chrome, so the
//!   blur is the point;
//! - the workspace panel is [`crate::ladder::background`], **opaque** — it
//!   carries instrument surfaces, and a waveform read through a blurred
//!   desktop is a waveform you cannot read;
//! - the thread column is [`panel_opaque`], the same ground at full coverage —
//!   it scrolls, and a transcript that scrolls needs its ends dissolved.
//!
//! So the rule is: **a structural plane is opaque** — because it carries an
//! instrument, or because it carries scrolling content whose edges have to be
//! faded. Only what genuinely *floats* takes a coverage.
//!
//! The second half of that is the less obvious one. A fade band is an overlay
//! painted **on** its plane, so it can only disappear into a colour it can
//! name; a translucent plane's on-screen tone is its rung composited over
//! whatever is behind the *window*, which no band can match at any tint. The
//! thread column read as a dark strip under its own header for exactly that
//! reason, at every tint tried. Opaque, the band and the plane are one token,
//! and
//! they agree by construction rather than by calibration.
//!
//! This tier therefore covers those grounds, what floats over them, and the
//! washes and inks a translucent surface's own states have to be.
//!
//! [`ink`], [`wash`] and [`hairline`] are the exception, and deliberately so:
//! they are alpha over neutral white, which is what a translucent surface's
//! own states have to be — an opaque grey plate on a translucent plane paints
//! out the tint that is the point of the tier.
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

use crate::ladder;

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

// ---------------------------------------------------------------------------
// The alpha families
// ---------------------------------------------------------------------------
//
// Four families, and every translucent value on this tier belongs to exactly
// one of them. They exist as separate functions rather than one `alpha(a)`
// because they do not move together when the field behind them changes: on a
// bright backdrop an edge needs *more* ink and a plate needs *less*, and a
// scrim must stay black while a fill flips to soft-black. That divergence is
// precisely what a light mode would have to express, so the families are the
// seam a second appearance is added at — one `match` per family, no call site
// touched. Luma is dark-only today (see "One appearance" above), so each is
// currently a single arm.
//
// Alphas are quoted at every call site in dark-mode terms; a family, not a
// call site, is where a second appearance would derive its own value.

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

/// Recessed **shade**: always black, at every appearance. The family that
/// *subtracts* light — a modal backdrop ([`scrim`]) and the recessed strip a
/// palette's header and footer sit on ([`band`]) are the same gesture at two
/// strengths, and neither has a tone to flip. A "light scrim" of white would
/// wash a modal out instead of seating it.
///
/// This is the one family with no ladder rung behind it, and deliberately so:
/// the ladder climbs from its ground and has nothing below the floor, so
/// *recessed* cannot be a rung. It has to be paint that removes light.
#[must_use]
pub fn scrim(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, alpha)
}

/// Alpha of the standard modal backdrop.
pub const SCRIM_ALPHA: f32 = 0.60;

/// Alpha of the modal backdrop behind a dialog.
///
/// Heavier than the 0.35 this was when the plane was *also* blurred: dimming is
/// now the only thing separating the dialog from the shell, so it has to do
/// that work alone. Lighter than [`SCRIM_ALPHA`], because the shell underneath
/// is meant to stay legible — a lighting desk's stage should still read as a
/// stage while you pick a track to put on it, and a blackout would say the
/// dialog had replaced the app rather than risen above it.
pub const DIALOG_SCRIM_ALPHA: f32 = 0.45;

/// Coverage of a chrome surface over whatever is behind the window.
///
/// macOS is the only platform where the compositor guarantees a blur behind a
/// translucent window; everywhere else a merely *transparent* panel shows the
/// raw desktop through it, so the glass tier resolves opaque and every call
/// site below keeps painting without knowing which happened.
pub const GLASS_ALPHA: f32 = if cfg!(target_os = "macos") { 0.80 } else { 1.0 };

/// Coverage of the content ground. Lower than [`GLASS_ALPHA`] on purpose: the
/// ground is the plane the eye reads *through* the least, so it can spend more
/// of its coverage on the blur behind it — and where the shell has already
/// painted it opaque, the coverage costs nothing at all.
pub const PANEL_ALPHA: f32 = if cfg!(target_os = "macos") { 0.50 } else { 1.0 };

/// Coverage of a floating chrome surface — menu, popover, picker. **Opaque**,
/// because these are the surfaces whose whole job is carrying *text*, and text
/// may not ghost under text: a menu row over a label ("subtract" over a
/// strip's "REPLACE") reads as two broken strings, not as depth.
///
/// The four percent this used to keep — meant as a token membership in the
/// glass family — was enough to do exactly that: measured over the timeline,
/// a clip's own label and its lit blocks both read through the usage card.
/// Four percent of a bright element on a near-black plane is not a texture,
/// it is a second image. A surface that hangs over *content* rather than over
/// the window's edge has no desktop behind it to be glass about, so there is
/// nothing for the coverage to buy. Decorative glass ([`PANEL_ALPHA`],
/// [`DIALOG_ALPHA`] — whose card sits on its own scrim) keeps its coverage.
pub const OVERLAY_ALPHA: f32 = 1.0;

/// Coverage of a large frosted picker.
///
/// Raised from the 0.34 this was when the modal plane behind it was ALSO
/// blurred. With the plane reduced to a plain tint the card became the only
/// thing separating a dialog from the shell, and at a third coverage it read
/// as see-through rather than as glass — the content behind it stayed legible
/// enough to compete with the content on it. Still well short of [`OVERLAY_ALPHA`]:
/// a menu has to put rows on a *known* background, while a dialog is big
/// enough that some of the blurred backdrop showing through is the point.
pub const DIALOG_ALPHA: f32 = if cfg!(target_os = "macos") { 0.62 } else { 1.0 };

/// The chrome plane: what a surface takes to read as *frame* rather than
/// content. [`ladder::chrome_plane`] at [`GLASS_ALPHA`] — the sidebar's own
/// rung, so a chrome surface anywhere in the app lands where the sidebar is.
///
/// The shell paints its sidebar opaque from the ladder (see the module docs);
/// this is the same plane for anything that can afford the desktop through it.
#[must_use]
pub fn glass() -> Hsla {
    tinted(ladder::chrome_plane(), GLASS_ALPHA)
}

/// The content ground with the desktop showing through it: [`ladder::background`]
/// at [`PANEL_ALPHA`].
///
/// No plane in the shell takes this — every structural plane is opaque (see the
/// module docs), so this is the coverage a *floating* surface reaches for when
/// it wants the ground's rung rather than the apex. Anything painted **on** a
/// ground wants [`panel_opaque`] instead.
#[must_use]
pub fn panel() -> Hsla {
    tinted(ladder::background(), PANEL_ALPHA)
}

/// The content ground's **colour**, at full coverage.
///
/// [`panel`] is that colour at [`PANEL_ALPHA`], and the coverage is the part
/// that does not travel: it is what the plane spends on the blur *behind the
/// window*. Anything painted **on** the ground — a fade band, a dissolve, a
/// mask — has the ground behind it rather than the desktop, so reusing
/// [`panel`] there paints a second half-coverage of the same tone over a
/// surface that already has it: too dark to match the ground, and too
/// transparent to hide what it is covering.
///
/// So this is what a structural plane and everything painted on it both ask
/// for — the thread column and the fade bands at its ends are the same token,
/// which is what makes them match. [`panel`] is the same colour spending part
/// of itself on the blur, for a surface that floats. Same colour, one
/// definition, two coverages — two functions rather than an alpha argument
/// nobody would know how to pick.
#[must_use]
pub fn panel_opaque() -> Hsla {
    tinted(ladder::background(), 1.0)
}

/// A floating chrome surface: menu, popover, the plane an overlay screen sits
/// on. [`ladder::apex`] at [`OVERLAY_ALPHA`] — the ladder's brightest plane,
/// so a thing that floats over the chrome still reads above it.
#[must_use]
pub fn overlay() -> Hsla {
    tinted(ladder::apex(), OVERLAY_ALPHA)
}

/// Translucent material for large modal cards. [`ladder::apex`] at
/// [`DIALOG_ALPHA`] — the same rung [`overlay`] takes, because a dialog floats
/// for the same reason a menu does; only the coverage differs.
#[must_use]
pub fn dialog() -> Hsla {
    tinted(ladder::apex(), DIALOG_ALPHA)
}

/// Alpha of the recessed strip a palette's header and footer sit on.
///
/// Measured subtler values vanish against the dim scrim: the band has to read
/// as recessed through both the backdrop blur and the card tint above it.
pub const BAND_ALPHA: f32 = 0.16;

/// Recessed translucent header/footer band used by palette-shaped dialogs —
/// the [`scrim`] family at [`BAND_ALPHA`]. A strip that is *below* the card's
/// own surface cannot be a rung (see [`scrim`]); it is the card's material
/// with light taken back out of it.
#[must_use]
pub fn band() -> Hsla {
    scrim(BAND_ALPHA)
}

/// A ladder tone taken to the glass tier at `alpha`. The single conversion
/// between the two tiers: the ladder owns every *value*, this module owns how
/// much of the desktop each surface lets through.
fn tinted(tone: gpui::Rgba, alpha: f32) -> Hsla {
    Hsla::from(tone).opacity(alpha)
}

/// The chrome band's three wash strengths, named so the tab chips, the corner
/// toggles and the close hotspot cannot drift apart one literal at a time.
/// Hover on a control at rest — the quietest lift there is.
pub const WASH_SUBTLE: f32 = 0.06;
/// What an *active* chrome control rests at: the selected tab chip, a toggle
/// whose panel is up, the armed close hotspot.
pub const WASH_REST: f32 = 0.10;
/// Hover over an already-active control — one step above its rest, so the
/// pointer still reads on the brightest chip in the band.
pub const WASH_EMPHASIS: f32 = 0.14;

/// Alpha of the one wash that marks a row as *lifted* — hovered or selected.
///
/// Hover and selection share this fill on purpose. Two different fills make a
/// hovered row and a selected row compete for the same reading, and the moment
/// the pointer rests on the selected row they have to resolve into one anyway.
/// What distinguishes selection is the ring ([`card_selected_shadows`]), which
/// is an *edge* — a different channel, so the two compose instead of fighting.
pub const SELECTION_ALPHA: f32 = 0.11;

/// Hover wash for a row sitting directly on [`glass`]. Softer than an opaque
/// element hover: over a translucent ground a full-strength wash paints out the
/// tint that is the point of the tier.
///
/// Identical to [`card_selected_bg`] by construction — see [`SELECTION_ALPHA`].
#[must_use]
pub fn glass_hover() -> Hsla {
    wash(SELECTION_ALPHA)
}

/// Selected fill for a row or chip inside a floating card — menu rows, the
/// picker rail, segmented chips. The same wash a hover takes
/// ([`SELECTION_ALPHA`]); the ring is what says "selected".
#[must_use]
pub fn card_selected_bg() -> Hsla {
    wash(SELECTION_ALPHA)
}

/// The selected chip's outline, as an **inset** shadow.
///
/// gpui paints inset shadows on top of the background and only at the edges —
/// a border with no layout cost, so selection never reflows a row. A *drop*
/// shadow is a filled rect painted BEHIND the element, and behind a
/// translucent fill it shows straight through as an opaque plate with a greyed
/// rim. Nothing may paint behind a glass chip: the card already carries
/// whatever elevation the stack needs, and selection inside it only owes the
/// edge.
///
/// One layer, zero blur, one pixel of spread. A blurred ring on a near-black
/// field reads as smudge rather than outline at this size.
#[must_use]
pub fn card_selected_shadows() -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: hairline(0.09),
        offset: gpui::point(gpui::px(0.0), gpui::px(0.0)),
        blur_radius: gpui::px(0.0),
        spread_radius: gpui::px(1.0),
        inset: true,
    }]
}

/// The keyboard cursor's plate — the row the arrow keys are on, which is *not*
/// the same thing as the row that is selected.
///
/// A step below [`card_selected_bg`] and from the [`ink`] family rather than
/// the wash: the cursor is a position, not a state, so it reads as a lighter
/// plate with no ring. Two rows looking selected at once is the bug this
/// distinction exists to prevent.
#[must_use]
pub fn card_cursor_bg() -> Hsla {
    ink(0.05)
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

/// Coverage of the sidebar's tone column over the shell root.
///
/// Barely there on purpose: the sidebar is *not* a plane of its own on a
/// blurred window — see [`tone_column`].
pub const TONE_COLUMN_ALPHA: f32 = 0.05;

/// The sidebar's tone: a wash over the shell root, not a fill of its own.
///
/// # The blur is a window property, not an element's
///
/// A blurred sidebar is three things together, and painting any of them alone
/// gets nothing:
///
/// 1. the window composites blurred ([`window_background_appearance`]),
/// 2. the shell **root** paints [`glass`] — one translucent fill across the
///    whole window, which is the only surface the desktop shows through,
/// 3. the sidebar paints *this* over that root, plus a `border_r` of
///    [`hairline`], and **no background of its own**.
///
/// Step 3 is the counter-intuitive one. An opaque sidebar rung would be a
/// slab sitting on the blur rather than a region of it; a *translucent* rung
/// would land on the root's own tone and vanish. Only a wash lifts what is
/// already behind it, which is what "one blurred pane, subtly divided" has to
/// mean. The column is a tone, and the border is the division.
///
/// The gotcha in step 1: gpui's macOS backend tears the `NSVisualEffectView`
/// out of the hierarchy whenever the appearance is set to anything but
/// `Blurred`, so a theme swap must **re-push** it — see
/// [`window_background_appearance`].
#[must_use]
pub fn tone_column() -> Hsla {
    wash(TONE_COLUMN_ALPHA)
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

    /// The tier borrows the ladder's *value* and changes only its coverage —
    /// a glass surface that had drifted to a tone of its own is the bug this
    /// asserts against.
    ///
    /// Every surface function on this tier is listed. A new one that is not
    /// here has escaped the contract, so the count is asserted too.
    #[test]
    fn glass_surfaces_are_ladder_rungs_at_a_coverage() {
        let surfaces = [
            ("glass", glass(), ladder::chrome_plane(), GLASS_ALPHA),
            ("panel", panel(), ladder::background(), PANEL_ALPHA),
            ("overlay", overlay(), ladder::apex(), OVERLAY_ALPHA),
            ("dialog", dialog(), ladder::apex(), DIALOG_ALPHA),
        ];
        for (name, surface, rung, alpha) in surfaces {
            assert_eq!(surface.l, Hsla::from(rung).l, "{name} minted its own tone");
            assert_eq!(surface.s, 0.0, "{name} picked up a tint");
            assert_eq!(surface.a, alpha, "{name} is not at its declared coverage");
        }
        assert_eq!(surfaces.len(), 4, "a new surface must join the guard");
    }

    /// A dialog floats for the same reason a menu does, so it lands on the
    /// same rung; only how much desktop it lets through differs.
    #[test]
    fn the_two_floating_surfaces_share_a_rung() {
        assert_eq!(dialog().l, overlay().l);
        assert_ne!(dialog().a, overlay().a);
    }

    /// Recessed paint has no rung to borrow (there is nothing below the
    /// ground), so it must come from the one family that subtracts light.
    #[test]
    fn recessed_paint_is_the_scrim_family() {
        assert_eq!(band(), scrim(BAND_ALPHA));
        for shade in [band(), scrim(SCRIM_ALPHA), scrim(DIALOG_SCRIM_ALPHA)] {
            assert_eq!(shade.l, 0.0, "a shade that is not black is not recessed");
            assert_eq!(shade.s, 0.0);
        }
    }

    /// Hover and selection are the same lift; only the ring separates them.
    /// A second fill here is the bug — see [`SELECTION_ALPHA`].
    #[test]
    fn hover_and_selection_share_one_fill_and_differ_only_by_the_ring() {
        assert_eq!(glass_hover(), card_selected_bg());
        assert_ne!(card_cursor_bg(), card_selected_bg());

        let ring = card_selected_shadows();
        assert_eq!(ring.len(), 1, "selection is one edge, not a stack");
        assert!(ring[0].inset, "a drop shadow paints behind a glass chip");
        assert_eq!(ring[0].blur_radius, gpui::px(0.0));
        assert_eq!(ring[0].spread_radius, gpui::px(1.0));
        assert_eq!(ring[0].color, hairline(0.09));
    }

    /// The four families are the seam a light mode is added at, so each has to
    /// stay recognisably itself: fills and edges lift, washes stop short of
    /// white, shades are black.
    #[test]
    fn the_alpha_families_stay_distinct() {
        assert_eq!(ink(0.5).l, 1.0);
        assert_eq!(hairline(0.5).l, 1.0);
        assert!(wash(0.5).l < 1.0, "a wash that reached white is an ink");
        assert_eq!(scrim(0.5).l, 0.0);
        for family in [ink(0.42), hairline(0.42), wash(0.42), scrim(0.42)] {
            assert_eq!(family.a, 0.42, "a family must not rescale its alpha");
            assert_eq!(family.s, 0.0);
        }
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
