//! The grey ladder, once. Every GPUI surface — harness fixture and real app
//! screen alike — reads its color from here, so the port has the same single
//! source of truth the web side has in `src/App.css`. If anything above this
//! module hardcodes an `0x…`, that's the bug.
//!
//! Six greys carry the entire hierarchy and hue appears only for *meaning*
//! ([`primary`]). Depth between two adjacent surfaces is a value step or a
//! slice of [`trim`] — never a shadow, a gradient, or a rounded corner.

use gpui::{hsla, rgb, rgba, Hsla, Rgba};

/// `--titlebar-background` — the deepest plane, top bar only.
pub fn titlebar_background() -> Rgba {
    rgb(0x0e0e0e)
}

/// `--gutter` — heavier gap / empty-area contrast, one notch deeper than
/// [`trim`]. Also the tone [`crate::glass::panel`] takes to the chrome tier.
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

/// `--accent` / `--chart-4` — one hue under two names on the web side. Used
/// here for a waveform the analysis gave no per-bucket colors, which is the
/// only surface that needs a hue without meaning anything by it.
pub fn accent() -> Rgba {
    rgb(0x81a1c1)
}

/// `--muted` — the recessed ground a painted surface sits on. Deeper than
/// [`background`] and shallower than [`input`]; the timeline's waveform bed and
/// its lane fills are the only readers.
pub fn muted() -> Rgba {
    rgb(0x262626)
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

/// `--muted-foreground` — placeholders and de-emphasised text.
pub fn muted_foreground() -> Rgba {
    rgb(0x777777)
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
