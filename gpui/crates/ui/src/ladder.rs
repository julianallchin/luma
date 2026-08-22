//! The grey ladder, once. Every GPUI surface — harness fixture and real app
//! screen alike — reads its color from here. If anything above this module
//! hardcodes an `0x…`, that's the bug.
//!
//! # The ground is the floor
//!
//! This ladder climbs. [`background`] is the darkest plane there is — the
//! window, the thread column, the workspace panel, every tab body — and every
//! other surface is placed by *how far it rises off it*: a card lifts a little
//! ([`card`]), a header band more ([`band`]), the sidebar further still because
//! it is chrome rather than content ([`chrome_plane`]), a floating menu most
//! ([`apex`]). There is nothing below the floor, so depth is never a darker
//! slice; a divider is a *lighter* hairline ([`trim`], [`border`]).
//!
//! That is the opposite of the web app's ladder in `src/App.css`, which puts
//! the titlebar deepest and separates surfaces with darker trim. The two
//! deliberately disagree — see CLAUDE.md, "UI design system", which carries a
//! table for each. Do not "fix" one into the other.
//!
//! Hue appears only for *meaning* ([`primary`], [`status_ok`], [`port`]).
//! Corners stay square and nothing animates; those halves of the contract are
//! unchanged.

use gpui::{hsla, rgb, rgba, Hsla, Rgba};

// -- planes -------------------------------------------------------------------

/// A plane's value, 0–255, and the currency [`seam`] does arithmetic in.
///
/// Planes are one channel rather than three because every one of them is
/// achromatic by rule, and because a seam's brightness is a *function* of the
/// planes it divides — which needs a number, not a colour. [`grey`] is the only
/// way one of these becomes paint, so a rung and the number it is cannot drift.
const GROUND: u8 = 0x18;
/// See [`GROUND`]. Ascending; each name is the role, not the value.
const CARD: u8 = 0x1c;
const STRIPE: u8 = 0x1f;
const TRIM: u8 = 0x21;
const BAND: u8 = 0x23;
const CONTROL: u8 = 0x24;
const CHROME_PLANE: u8 = 0x26;
const APEX: u8 = 0x2d;
const HOVER: u8 = 0x39;

/// A plane's value as paint. Achromatic by construction — a plane that wanted a
/// tint would be depth carried by hue, which this ladder does not do.
fn grey(value: u8) -> Rgba {
    rgb(u32::from_be_bytes([0, value, value, value]))
}

/// The ground: the window, the thread column, the workspace panel, every tab
/// body and every overlay screen's own fill. The darkest plane there is and the
/// one nothing sits under — see the module docs.
pub fn background() -> Rgba {
    grey(GROUND)
}

/// A body raised just off the ground: a graph node's card, the timeline's
/// waveform bed. The faintest lift the ladder has, which is all a body needs —
/// what separates it from the ground is that it is *above* it at all.
pub fn card() -> Rgba {
    grey(CARD)
}

/// The alternating list-row stripe, paired with [`background`] on the even
/// rows.
pub fn stripe() -> Rgba {
    grey(STRIPE)
}

/// The hairline between two sections of one surface. *Lighter* than the ground
/// it divides, which is what a divider has to be once the ladder climbs — a
/// darker slice would read as a hole rather than a line.
pub fn trim() -> Rgba {
    grey(TRIM)
}

/// A card's or a section's header band — the strip above its contents, on
/// [`card`] or straight on the ground.
pub fn band() -> Rgba {
    grey(BAND)
}

/// Control resting fill (buttons, inputs, select triggers).
pub fn control() -> Rgba {
    grey(CONTROL)
}

/// The sidebar: the one region raised above the content ground because it is
/// *chrome* and not content. The whole depth model in one step — the app's
/// subject matter is the floor, and the frame around it is what stands proud.
pub fn chrome_plane() -> Rgba {
    grey(CHROME_PLANE)
}

/// The ladder's apex: the brightest plane there is, and the one with nothing
/// above it — an input's own fill, and the ground a floating chrome surface
/// (menu, popover) lands on. Everything that sits on top of everything else
/// shares one rung.
///
/// The chat's composer is *not* this. It floats, so it keeps its coverage
/// ([`crate::glass::card_bg`] over the thread's ground) and lands where that
/// puts it — `#1f1f1f`, not this rung. A rung is what an opaque surface takes,
/// not a promise about every pixel above it.
pub fn apex() -> Rgba {
    grey(APEX)
}

/// Universal hover fill on an opaque surface. Achromatic where the reference's
/// own selected row is a hair warm (`#393838`); a plane that carried a tint
/// would be depth by hue, which this ladder does not do.
///
/// A row on a glass surface washes instead of taking this — see [`apex`] for
/// why that is the same distinction, not a second one.
pub fn hover() -> Rgba {
    grey(HOVER)
}

/// The definition line around every control.
///
/// The one downward-depth device left on this ladder, and deliberately: a
/// control's outline is a *component* decision the web app and this one still
/// share (`BUTTON_CLASS`), not a shell plane, and the reference measures no
/// control border to derive a lighter one from.
pub fn control_border() -> Rgba {
    rgb(0x080808)
}

/// The hairline between surfaces that aren't controls. One step brighter than
/// the brightest plane, so it reads over any of them.
pub fn border() -> Rgba {
    rgb(0x3f3f3f)
}

// -- seams --------------------------------------------------------------------

/// How far a rule has to clear the brighter of the two planes it divides before
/// it reads as a line rather than as one more value step.
///
/// Solved from the reference's own two seams, which are the only measured
/// points there are. `#181818` against itself is ruled `#2b2b2b`, and with no
/// step between the planes that fixes the lift alone: `43 − 24 = 19`. `#262626`
/// against `#181818` is ruled `#484848`, and what the lift does not account for
/// is left to the planes' own step — a seam between *different* planes is read
/// from the darker side too, so it has to clear that side by the step it
/// already sees, on top of this.
///
/// # The step is counted once, and lands one under
///
/// `72 − 38 − 19 = 15` against a step of `38 − 14 = 14`, so the exact fit is
/// fifteen-fourteenths of a step. [`rule`] counts the step **once** and
/// [`seam_plane`] therefore comes out `#474747`, one value under the reference's
/// `#484848`. That is a choice, not a rounding nobody noticed: two measured
/// points fitted with two free parameters is a lookup table wearing an
/// equation's clothes — it would predict nothing about a third seam — and
/// 1/255 of grey is beneath what the eyedropper that produced these numbers can
/// resolve. `seam_hint` is exact. The tests assert against the *measured*
/// greys with that one value of slack named, so neither the fit nor its cost
/// can drift unnoticed.
const SEAM_LIFT: u8 = 0x13;

/// The rule between two planes, *as a function of what it separates* — which is
/// the whole point: a seam is not a colour anyone picks. Move a plane and every
/// seam touching it moves too, so no seam can go stale the way a hand-copied
/// constant would.
///
/// The pair below are the shell's two, named for their roles; anything else
/// that divides two planes derives its own line the same way.
pub fn seam(a: Rgba, b: Rgba) -> Rgba {
    rule(value_of(a), value_of(b))
}

/// The plane a rung is, 0–255. Reads one channel because planes are achromatic
/// by rule, which the `planes_carry_no_hue` test holds.
fn value_of(plane: Rgba) -> u8 {
    (plane.r.clamp(0., 1.) * 255.).round() as u8
}

/// [`seam`] in the currency the rungs are stored in.
fn rule(a: u8, b: u8) -> Rgba {
    let (dark, bright) = (a.min(b), a.max(b));
    grey(
        bright
            .saturating_add(bright - dark)
            .saturating_add(SEAM_LIFT),
    )
}

/// The full-height rule between two regions of the shell that are *different
/// planes* — [`chrome_plane`] (the sidebar) and [`background`] (the thread
/// column). Brighter than [`seam_hint`] because it has to out-read both sides;
/// see [`seam`], which derives both from the planes they touch.
///
/// `#474747` — one value under the reference's `#484848`, deliberately, for the
/// reason the `SEAM_LIFT` constant gives.
pub fn seam_plane() -> Rgba {
    rule(CHROME_PLANE, GROUND)
}

/// The full-height rule between two regions that are the same plane — the
/// thread column and the workspace panel, both [`background`]. A hint that the
/// boundary is there, not a division; see [`seam`].
pub fn seam_hint() -> Rgba {
    rule(GROUND, GROUND)
}

/// The accent, used here for a waveform the analysis gave no per-bucket colors
/// — the only surface that needs a hue without meaning anything by it.
pub fn accent() -> Rgba {
    rgb(0x619ec9)
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

/// `--chart-3` — the playhead, and the third place color means something
/// rather than placing a surface. One hue, spent on "where the audio is".
pub fn playhead() -> Rgba {
    rgb(0xf1b467)
}

/// The three frequency bands of a rekordbox-style waveform: bass, mids, air.
/// Literal in `drawWaveform` (`src/features/track-editor/utils/timeline-drawing.ts`)
/// rather than a token, because they are the data's colors and not the panel's
/// — named here so this crate stays the one place a color is written down.
pub fn waveform_low() -> Rgba {
    rgb(0x0055e2)
}

/// See [`waveform_low`].
pub fn waveform_mid() -> Rgba {
    rgb(0xf2aa3c)
}

/// See [`waveform_low`].
pub fn waveform_high() -> Rgba {
    rgb(0xffffff)
}

/// The hue a pattern's clips carry on the timeline, keyed by pattern id.
///
/// A hash into a fixed palette rather than a stored color, so the same pattern
/// reads the same in every venue and in both hosts without anything having to
/// remember it. Mirrors `getPatternColor` in
/// `src/features/track-editor/utils/timeline-constants.ts` exactly, including
/// the wrapping `i32` accumulator — pattern ids are ASCII UUIDs, so iterating
/// bytes here and UTF-16 code units there is the same walk.
pub fn pattern(pattern_id: &str) -> Rgba {
    const PALETTE: [u32; 8] = [
        0x8b5cf6, 0xec4899, 0xf59e0b, 0x10b981, 0x3b82f6, 0xef4444, 0x06b6d4, 0xf97316,
    ];
    let mut hash: i32 = 0;
    for byte in pattern_id.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(byte));
    }
    rgb(PALETTE[hash.unsigned_abs() as usize % PALETTE.len()])
}

/// `--destructive` — failure prose, and the one red the app writes words in.
pub fn danger() -> Rgba {
    rgb(0xe34671)
}

/// `bg-red-600` — the fill a destructive control takes under the pointer
/// (`window-controls.tsx`), and the only red that is a *surface* rather than
/// prose. Darker than [`danger`] because [`destructive_foreground`] sits on it.
pub fn destructive_hover() -> Rgba {
    rgb(0xdc2626)
}

/// `--destructive-foreground` — what a [`destructive_hover`] fill is legible
/// under. True white, and the only place the panel spends it on text.
pub fn destructive_foreground() -> Rgba {
    rgb(0xffffff)
}

/// The dimming a control takes when it will not accept input
/// (`disabled:opacity-50` in `BUTTON_CLASS`). Opacity rather than a grey of
/// its own, so one value dims a control's fill, border and label together
/// whatever they are.
pub const DISABLED_OPACITY: f32 = 0.5;

/// `--foreground`.
pub fn foreground() -> Rgba {
    rgb(0xe4e4e4)
}

/// Placeholders and de-emphasised text. Brighter than the web app's
/// `--muted-foreground`: the reference measures its muted ink at this value,
/// and a ladder whose ground fell to [`background`] has the room to spend it.
pub fn muted_foreground() -> Rgba {
    rgb(0xa3a3a3)
}

/// `text-foreground/90` — the resting label color on controls.
pub fn foreground_90() -> Rgba {
    rgba(0xe4e4e4e6)
}

/// `text-gray-400` — the label above a node's param control, and the one text
/// value on this screen that is neither [`foreground`] nor
/// [`muted_foreground`]. Literal in `standard-node.tsx` rather than a token on
/// the web side; named here so it stays a value with an owner.
pub fn param_label() -> Rgba {
    rgb(0x9ca3af)
}

/// `rgba(226, 232, 240, 0.85)` — the min/max axis labels the view node's 2D
/// context writes into its plot (`view-channel-node.tsx`). A canvas literal on
/// the web side, so it has no token to mirror.
pub fn plot_axis() -> Rgba {
    rgba(0xe2e8f0d9)
}

/// `text-slate-400` — the view node's "waiting for signal data…" line.
pub fn plot_empty() -> Rgba {
    rgb(0x94a3b8)
}

/// One trace in a view node's plot, off the same 12-step wheel the web canvas
/// strokes with: `hsl(index · 30, 82%, 62%)` (`CHROMA_LINE_COLORS` in
/// `view-channel-node.tsx`). The formula rather than a baked table, because
/// the formula is what the source says and a table would be a second place to
/// keep it right. Wraps, so a signal with more series than hues repeats them —
/// as the web `index % length` does.
pub fn plot_trace(index: usize) -> Rgba {
    hsla((index % 12) as f32 * 30. / 360., 0.82, 0.62, 1.).into()
}

/// `bg-white/5` — the faintest lift there is over whatever surface is
/// underneath, and the only token that does not name a plane of its own: the
/// fill *and* border of a legend chip under a view node's plot, and a titlebar
/// button's hover. Spelled by its web value rather than by one of its two jobs,
/// as [`foreground_90`] is, so neither caller has to borrow the other's name.
pub fn white_5() -> Rgba {
    rgba(0xffffff0d)
}

/// `text-slate-200` — the series name on a legend chip. Lighter than
/// [`legend_value`], which is the reading beside it.
pub fn legend_label() -> Rgba {
    rgb(0xe2e8f0)
}

/// `text-slate-400` — the mono reading on a legend chip. The same slate as
/// [`plot_empty`]: the view node spends one recessive grey on everything that
/// is not a trace.
pub fn legend_value() -> Rgba {
    plot_empty()
}

/// `--foreground` at an arbitrary alpha, for the places the web side stacks a
/// `/90` text color with an `opacity-50` icon.
pub fn foreground_alpha(alpha: f32) -> Hsla {
    let mut color: Hsla = foreground().into();
    color.a = alpha;
    color
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two seams the reference actually measures, named here as the
    /// *measured* greys and not as whatever [`rule`] happens to return — a test
    /// that restates the implementation is green forever and holds nothing.
    ///
    /// One value of slack, for the reason [`SEAM_LIFT`] gives. Widen this and
    /// the rule has stopped being a fit to the reference at all.
    #[test]
    fn the_seam_rule_lands_on_the_measured_seams() {
        for (planes, measured) in [((0x26, 0x18), 0x48_u8), ((0x18, 0x18), 0x2b)] {
            let derived = value_of(rule(planes.0, planes.1));
            assert!(
                derived.abs_diff(measured) <= 1,
                "seam({:#04x}, {:#04x}) is {derived:#04x}, reference {measured:#04x}",
                planes.0,
                planes.1
            );
        }
    }

    /// Where the slack is spent, pinned. Counting the planes' step once puts
    /// [`seam_plane`] one under the reference and leaves [`seam_hint`] exact;
    /// if that ever stops being true it is a decision, and it should have to be
    /// made here rather than absorbed.
    #[test]
    fn only_the_plane_seam_is_short_and_only_by_one() {
        assert_eq!(value_of(seam_plane()), 0x48 - 1);
        assert_eq!(value_of(seam_hint()), 0x2b);
    }

    /// A seam is a function of what it separates, in both directions: the rule
    /// between two planes cannot depend on which one the caller names first.
    #[test]
    fn a_seam_does_not_care_which_side_is_named_first() {
        assert_eq!(
            seam(chrome_plane(), background()),
            seam(background(), chrome_plane())
        );
        assert_eq!(seam(chrome_plane(), background()), seam_plane());
    }

    /// The model in one assertion: the ground is the floor and every plane is
    /// placed by how far it rises off it. A rung that sank below the ground
    /// would be depth pointing the wrong way.
    #[test]
    fn the_ladder_climbs_from_the_ground() {
        let rungs = [
            GROUND,
            CARD,
            STRIPE,
            TRIM,
            BAND,
            CONTROL,
            CHROME_PLANE,
            APEX,
            HOVER,
        ];
        assert!(rungs.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// Planes are achromatic: hue on this ladder means *meaning*, never depth.
    #[test]
    fn planes_carry_no_hue() {
        let plane = grey(CHROME_PLANE);
        assert_eq!(plane.r, plane.g);
        assert_eq!(plane.g, plane.b);
    }
}
