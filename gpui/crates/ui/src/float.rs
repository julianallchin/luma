//! Controls for the things that float — dialogs, menus, pickers.
//!
//! # Why these are not the slab controls
//!
//! `luma_button` and its siblings are the *instrument* tier: square, on
//! [`crate::ladder`]'s opaque greys, 9px uppercase silkscreen. That is the
//! right language for a panel of controls bolted to a machine, and the wrong
//! one for a picker floating over a blurred backdrop — an opaque slab on
//! translucent glass paints out the tint that is the point of the surface, and
//! square corners on a detached card leave it reading as a hole in the window
//! rather than an object above it.
//!
//! So the split is the same one [`crate::glass`] already draws, and it is
//! decided by *what a control sits on*, not by which crate it lives in: on a
//! plane, take the slab; on glass, take these. Neither tier gets a second
//! style of its own — there is one row, one key cap, one button pair here, the
//! way there is one `luma_button` there.
//!
//! [`picker_chip`] is the one control here that lives on planes too, and it is
//! not an exception to that rule but an application of it: a trigger's menu
//! always floats, so the *pair* sits on glass even when the trigger's own
//! ground is opaque. See its note.
//!
//! Radii come from [`crate::radius`], every fill, edge and ring from
//! [`crate::glass`], and every text colour from [`crate::ladder`]'s foreground
//! family — ink is what a *surface* is made of, not what a word is written in,
//! and prose reads the same whatever plane it lands on. Nothing in this module
//! writes a colour or a corner down.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Div, FontWeight, SharedString};
use gpui_component::{Icon, IconName};

use crate::{glass, ladder, motion, radius, select};

// ---------------------------------------------------------------------------
// Bands
// ---------------------------------------------------------------------------

/// A palette's header or footer strip: the recessed [`glass::band`] shade with
/// a hairline on the edge that faces the body.
///
/// A band is a *pane* welded to the card — see [`crate::radius`]'s mixed-corner
/// rule. Its outer corners are the card's, its inner edge is a seam, and the
/// caller rounds the two free corners because only the caller knows which end
/// of the card it is at.
pub fn band() -> Div {
    div().bg(glass::band()).flex().flex_row().items_center()
}

/// The seam a band presents to the card body it is welded to.
const BAND_SEAM: f32 = 0.06;

/// The one row of chrome a [`header_band`] is tall.
pub const HEADER_HEIGHT: f32 = 46.0;

/// [`band`] at the top of a card: outer corners rounded, bottom edge a seam.
///
/// A fixed height, because a header holds one row of chrome — a search field,
/// a breadcrumb — and a palette whose header grew with its content would make
/// the list below it jump as the query changed.
pub fn header_band() -> Div {
    band()
        .flex_none()
        .h(px(HEADER_HEIGHT))
        .rounded_t(px(radius::MODAL))
        .border_b_1()
        .border_color(glass::hairline(BAND_SEAM))
        .pl(px(12.0))
        .pr(px(10.0))
        .gap(px(10.0))
}

/// [`band`] at the bottom of a card: outer corners rounded, top edge a seam.
///
/// Padded rather than fixed-height — a footer is a legend, and it should be
/// exactly as tall as the key caps it carries.
pub fn footer_band() -> Div {
    band()
        .flex_none()
        .rounded_b(px(radius::MODAL))
        .border_t_1()
        .border_color(glass::hairline(BAND_SEAM))
        .px(px(12.0))
        .py(px(FOOTER_PAD_Y))
        .gap(px(12.0))
}

const FOOTER_PAD_Y: f32 = 8.0;

/// What a [`footer_band`] holding one row of [`key_cap`]s comes out at: pad,
/// cap, pad, seam.
///
/// A *consequence* of the recipe above rather than a setting — a footer is
/// padded, not fixed. It is named because a fixed-size card sizing a child
/// against its own bands needs the number, and one derived here cannot drift
/// from the band the way a 46 written at the call site did.
pub const FOOTER_HEIGHT: f32 = FOOTER_PAD_Y * 2.0 + KEY_CAP_HEIGHT + 1.0;

/// Hairline divider between sections of a floating card.
pub fn divider() -> Div {
    div().h(px(1.0)).bg(glass::hairline(0.07))
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// What a row in a picker is currently *doing*.
///
/// Three states rather than two booleans because "selected" and "the keyboard
/// is here" are different facts that must never look the same: selection is
/// what pressing Enter would keep, the cursor is where pressing Enter would
/// move it to. A picker that paints both with one wash shows the user two
/// selected rows and no way to tell which one Enter takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// Neither selected nor under the keyboard — hover is the only lift.
    Rest,
    /// The keyboard cursor is on this row, but it is not the selected value.
    Cursor,
    /// This is the selected value, cursor or not.
    Selected,
}

impl RowState {
    /// Resolve the two facts a picker actually tracks.
    ///
    /// Selection wins: a row that is both is already the brightest thing in
    /// the list, and adding the cursor plate on top would only dull the ring
    /// that distinguishes it.
    #[must_use]
    pub fn of(selected: bool, cursor: bool) -> Self {
        match (selected, cursor) {
            (true, _) => Self::Selected,
            (false, true) => Self::Cursor,
            (false, false) => Self::Rest,
        }
    }
}

/// Paint `row` for `state`. The selection recipe lives here once, so every row
/// shape below wears the same three states.
///
/// Rest vs Cursor is the only state pair separated by *value alone* — the
/// cursor plate ([`glass::card_cursor_bg`]) sits a step above the card with no
/// ring or glyph to fall back on, where Selected always has its ring. Retuning
/// the ladder or a surface's lightness can collapse that step before anything
/// else breaks; whoever touches those re-checks this pair.
fn row_state_paint(row: Div, state: RowState, fade_key: impl Into<SharedString>) -> Div {
    match state {
        // Fill and ring, from the one selection recipe. Nothing paints behind
        // a glass chip — see [`glass::card_selected_shadows`].
        RowState::Selected => row
            .bg(glass::card_selected_bg())
            .shadow(glass::card_selected_shadows()),
        // A plate, no ring: the cursor marks a position, not a state.
        RowState::Cursor => row.bg(glass::card_cursor_bg()),
        RowState::Rest => {
            let fade_key = fade_key.into();
            let mut row = row.bg(motion::hover_blend(
                &fade_key,
                glass::wash(0.0),
                glass::glass_hover(),
            ));
            // Imperative form: `.on_hover` needs the `Stateful` the caller's
            // own `.id(…)` produces, which does not exist yet here.
            row.interactivity()
                .on_hover(motion::hover_listener(fade_key));
            row
        }
    }
}

/// One row of a picker's **list** — the thing the query filters and the arrow
/// keys walk.
///
/// Height comes from its padding, so a row grows with the content it carries
/// (a title over a subtitle). Contrast [`nav_row`].
///
/// `fade_key` drives the hover blend and must be unique app-wide and stable
/// across frames — the row's element id is the natural choice. The caller adds
/// `.id()`, the click listener and the children; hover listeners need element
/// state, which only the caller's id can provide.
pub fn menu_row(state: RowState, fade_key: impl Into<SharedString>) -> Div {
    row_state_paint(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(radius::ROW))
            .text_size(px(13.0))
            .cursor_pointer(),
        state,
        fade_key,
    )
}

/// One row of a picker's **rail** — the fixed column of scopes beside the list
/// (playlists, sources, devices).
///
/// A fixed 28px and a hair smaller than [`menu_row`], because a rail is a set
/// of destinations rather than the content itself: it should read as chrome
/// next to the list, and a rail whose rows changed height as their labels
/// wrapped would make the column jitter while the list beside it scrolled.
pub fn nav_row(state: RowState, fade_key: impl Into<SharedString>) -> Div {
    row_state_paint(
        div()
            .h(px(28.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .rounded(px(radius::ROW))
            .text_size(px(12.5))
            .cursor_pointer(),
        state,
        fade_key,
    )
}

/// The rail column itself: a fixed-width scroller divided from the list by its
/// own leading hairline, so the two panes share one boundary.
pub fn rail() -> Div {
    div()
        .w(px(196.0))
        .flex_none()
        .border_l_1()
        .border_color(glass::hairline(BAND_SEAM))
        .px(px(8.0))
        .py(px(8.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
}

/// A quiet word on glass — the name of a control, or (padded, as
/// [`section_heading`]) the heading over a group of rows.
///
/// One step under body text and muted with it, so it reads as chrome. The
/// instrument tier answers this with [`crate::silkscreen`]'s 9px uppercase,
/// which is a *panel* legend and looks stencilled onto glass.
pub fn label(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_size(px(11.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ladder::foreground_alpha(0.55))
        .child(text.into())
}

/// A quiet heading over a group of [`nav_row`]s.
pub fn section_heading(text: impl Into<SharedString>) -> Div {
    label(text).px(px(8.0)).pb(px(4.0))
}

/// The scroll viewport for a picker's list, with its vertical gutters on a
/// **wrapper** outside the scroller.
///
/// This is not a style preference. Vertical padding placed *inside* a scroll
/// container is eaten twice: the wheel's maximum offset consumes the bottom
/// pad, and `scroll_to_item` pins a row's edge flush to the viewport, so the
/// last row of a keyboard-driven list ends up touching the footer band. Only a
/// gutter that is not part of the scrolled content survives both. Horizontal
/// padding has no such problem and stays inside.
///
/// The caller puts its `.id()`, `.overflow_y_scroll()` and rows on the element
/// this returns as a child — `viewport()` is the wrapper.
pub fn viewport() -> Div {
    div().flex_1().min_h_0().py(px(6.0))
}

/// The row list inside a [`viewport`]: horizontal gutter and the app-wide list
/// rhythm. The caller adds `.id()`, `.overflow_y_scroll()` and `.track_scroll()`.
pub fn list() -> Div {
    div().size_full().px(px(8.0)).flex().flex_col().gap(px(2.0))
}

/// The line a picker shows where its rows would be, when there are none.
pub fn empty_row(message: impl Into<SharedString>) -> Div {
    div()
        .px(px(14.0))
        .py(px(16.0))
        .text_size(px(12.5))
        .text_color(ladder::foreground_alpha(0.45))
        .child(message.into())
}

/// The float tier's "this one is chosen" mark, and the hole it leaves when it
/// is not.
///
/// A fixed-width hole rather than nothing, so a label does not shift when the
/// mark arrives — and one function rather than a glyph at each call site, so
/// the single-select menus ([`crate::luma_select_item`]) and the multi-select
/// lists that tick several rows cannot end up marking chosen-ness two
/// different ways. This is what a checkbox is on glass; the instrument tier's
/// square [`crate::luma_checkbox`] belongs on a plane.
pub fn check(checked: bool) -> AnyElement {
    if checked {
        Icon::new(IconName::Check)
            .size(px(CHECK))
            .into_any_element()
    } else {
        div().size(px(CHECK)).into_any_element()
    }
}

/// The mark's box — the glyph and the hole share it or rows jump.
const CHECK: f32 = 12.0;

// ---------------------------------------------------------------------------
// Key caps
// ---------------------------------------------------------------------------

/// The height of a [`key_cap`], and so of the chips that must read as one of
/// them ([`btn_primary_chip`], `add_tracks`'s submit).
pub const KEY_CAP_HEIGHT: f32 = 22.0;

/// A footer key-cap — the `⌘K` / `esc` chip in a palette's legend.
///
/// Holds arbitrary children so a cap can carry a glyph, a word, or two glyphs
/// split by [`cap_split`].
pub fn key_cap() -> Div {
    div()
        .h(px(KEY_CAP_HEIGHT))
        .px(px(6.0))
        .rounded(px(radius::CAP))
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(4.0))
        .bg(glass::ink(0.05))
        .text_size(px(11.0))
        .font_family(crate::fonts::MONO)
        .text_color(ladder::foreground_alpha(0.70))
}

/// Make a [`key_cap`] a target: the `esc` chip that closes a card, the `←`
/// that walks a route back.
///
/// A cap is a *legend* by default — it names a key the keyboard already owns
/// and is not itself clickable. This is the opt-in for the ones that are, and
/// it exists so the lit shade is written here once instead of at every card
/// that happens to put a button in its band.
///
/// Takes the cap rather than building one, so a caller can gate it behind the
/// `.when(interactive, …)` a non-interactive dialog needs without having to
/// choose a different constructor.
pub fn key_cap_pressable(cap: Div) -> Div {
    cap.cursor_pointer()
        .hover(|style| style.bg(glass::ink(CAP_PRESSED)))
}

/// The lift a pressable [`key_cap`] takes under the pointer — one step above
/// its rest fill, so it reads as the same chip lit rather than a new control.
const CAP_PRESSED: f32 = 0.09;

/// The hairline between two glyphs sharing one [`key_cap`] (`[ ↑ | ↓ ]`).
pub fn cap_split() -> Div {
    div().w(px(1.0)).h(px(11.0)).bg(glass::hairline(0.10))
}

/// The tiny verb after a [`key_cap`] ("Navigate", "Open").
pub fn key_hint_label(label: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(10.5))
        .text_color(ladder::foreground_alpha(0.45))
        .child(label.into())
}

/// The glyph size inside a key cap — small enough that the cap reads as a key
/// rather than as a button with an icon in it.
const CAP_GLYPH: f32 = 12.5;

fn cap_glyph(icon: IconName) -> Icon {
    Icon::new(icon)
        .size(px(CAP_GLYPH))
        .text_color(ladder::foreground_alpha(0.70))
}

/// One entry in a footer legend: a key cap holding a glyph, then the verb that
/// key performs.
pub fn key_hint(icon: IconName, label: impl Into<SharedString>) -> Div {
    hint_row()
        .child(key_cap().child(cap_glyph(icon)))
        .child(key_hint_label(label))
}

/// [`key_hint`] for a key with no glyph in the set — the cap carries the word
/// itself ("tab", "esc").
pub fn key_hint_text(cap: impl Into<SharedString>, label: impl Into<SharedString>) -> Div {
    hint_row()
        .child(key_cap().child(cap.into()))
        .child(key_hint_label(label))
}

/// [`key_hint`] for a pair of keys that share one verb — `[↑|↓] Navigate`.
/// Two caps would claim two behaviours; one split cap says "either of these".
pub fn key_hint_pair(first: IconName, second: IconName, label: impl Into<SharedString>) -> Div {
    hint_row()
        .child(
            key_cap()
                .child(cap_glyph(first))
                .child(cap_split())
                .child(cap_glyph(second)),
        )
        .child(key_hint_label(label))
}

fn hint_row() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap(px(5.0))
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// The quiet button on a floating surface: text only, hover lifts a wash.
///
/// `fade_key` as in [`menu_row`]. The caller adds `.id()` and the click.
pub fn btn(label: impl Into<SharedString>, fade_key: impl Into<SharedString>) -> Div {
    let fade_key = fade_key.into();
    let mut btn = btn_shape()
        .text_color(motion::hover_blend(
            &fade_key,
            ladder::foreground_alpha(0.72),
            ladder::foreground_alpha(1.0),
        ))
        .bg(motion::hover_blend(
            &fade_key,
            glass::wash(0.0),
            glass::ink(0.06),
        ))
        .child(label.into());
    btn.interactivity()
        .on_hover(motion::hover_listener(fade_key));
    btn
}

/// The one emphasised button: a near-white fill with near-black text — the
/// only place on this tier where ink becomes a *surface* rather than a state.
///
/// One per card, or the card has no primary action.
pub fn btn_primary(label: impl Into<SharedString>) -> Div {
    btn_primary_paint(btn_shape()).child(label.into())
}

/// [`btn_primary`] shrunk to key-cap proportions, for the submit affordance
/// that rides *inside* a [`header_band`] beside the [`key_cap`]s.
///
/// A band is 46px tall and a 32px button in it leaves no air; more to the
/// point, a full-size button in a strip of key caps reads as a different
/// component from its neighbours. At cap height and [`radius::CAP`] it reads
/// as the one lit key in the row — which is exactly what it is.
///
/// The caller supplies the children (typically the `⌘` glyph and a word), so
/// the chip can carry its own busy label without a second function.
pub fn btn_primary_chip() -> Div {
    btn_primary_paint(
        div()
            .h(px(KEY_CAP_HEIGHT))
            .px(px(8.0))
            .rounded(px(radius::CAP))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .text_size(px(12.0))
            .cursor_pointer(),
    )
}

/// The emphasised fill, once, so the button and its chip cannot drift apart in
/// colour the way they legitimately differ in size.
pub(crate) fn btn_primary_paint(shape: Div) -> Div {
    shape
        .bg(glass::ink(0.92))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ladder::background())
        .hover(|style| style.bg(gpui::white()))
}

/// Dim a control that is present but cannot act — a submit with nothing to
/// submit, a chip mid-flight. Opacity rather than a second fill, so one value
/// dims the plate and its label together.
pub const INERT_OPACITY: f32 = 0.6;

/// The shared box both buttons compose against — this tier's answer to the
/// instrument tier's `slab`, so the pair cannot drift apart in geometry.
fn btn_shape() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .flex_shrink_0()
        .h(px(32.0))
        .px(px(12.0))
        .rounded(px(radius::ROW))
        .text_size(px(13.0))
        .cursor_pointer()
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

/// The one dropdown trigger: comet's picker chip (`shell/composer.rs`'s
/// toolbar chips — model, space, device), carrying the current value and a
/// chevron.
///
/// It lives on *this* tier and not with the slabs because a trigger and the
/// menu it opens are one object: the menu is a [`popover_card`], so a square
/// opaque trigger under a rounded translucent card reads as two components
/// stapled together. That is what makes this the canonical dropdown even
/// where the trigger itself sits on a plane — the tier is decided by the
/// menu, which always floats.
///
/// Comet's chips are 32px because they sit in a 46px composer band; ours are
/// [`crate::CONTROL_HEIGHT`] so a chip in a row of controls reads as one
/// of them rather than as a taller foreign thing — the same argument
/// [`btn_primary_chip`] makes about a button in a row of key caps.
///
/// `options` are every value the chip can show. They size it, through the one
/// ghost-stack the slabs use, so picking a different value never resizes the
/// trigger and never moves its neighbours.
pub fn picker_chip(value: &str, options: &[&str]) -> Div {
    select::ghost_stack(
        chip_plate().relative(),
        select::sentence_case(value),
        options.iter().map(|o| select::sentence_case(o)).collect(),
        PICKER_CHIP_PAD,
        ladder::foreground_alpha(0.45),
    )
}

/// A chip that acts instead of picking — the "Pick fixtures" affordance beside
/// the strip's expression field.
///
/// The same plate as [`picker_chip`] minus its chevron and ghost stack,
/// because a chip button and a chip trigger sitting in one row are the same
/// object with and without a menu; the moment they are drawn from two recipes
/// the row has two heights, two radii and two hovers. The caller supplies the
/// `.id()`, the click and the children.
pub fn chip() -> Div {
    chip_plate().justify_center().px(px(PICKER_CHIP_PAD))
}

/// The plate both chips wear. Padding is left off because [`picker_chip`]'s
/// ghost stack applies its own inset to two overlapping layers, and a plate
/// that already carried it would inset them twice.
fn chip_plate() -> Div {
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap(px(6.0))
        .h(px(crate::CONTROL_HEIGHT))
        .rounded(px(radius::ROW))
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ladder::foreground_alpha(0.9))
        .bg(glass::ink(0.06))
        .hover(|style| {
            style
                .bg(glass::glass_hover())
                .text_color(ladder::foreground())
        })
        .cursor_pointer()
}

/// Comet's `px(10)` on its picker chips — and, through [`field`], on every
/// other control in the row, so a value in a chip and a value in a field are
/// inset by the same amount and their text lines up.
const PICKER_CHIP_PAD: f32 = 10.0;

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

/// The one box a person types into: comet's composer pill at control scale —
/// a recessed [`glass::ink`] wash inside a [`glass::hairline`], rounded to
/// [`radius::ROW`] like the chip beside it.
///
/// A field is on this tier for the same reason [`picker_chip`] is, arrived at
/// from the other side: the chip's menu floats, and a field's completion menu
/// floats too, so both halves of a strip of controls hang glass off
/// themselves. Two languages in one row is the thing this replaces.
///
/// It is *recessed* where the chip is *raised* — a fill one step fainter, plus
/// an edge the chip does not have — and that is the whole vocabulary
/// distinguishing them: you press a chip, you type into a hole. Neither
/// carries a focus ring; on this tier the caret is the focus indicator.
///
/// The caller supplies the width, the type (a value field is mono, prose is
/// not) and the child. Height and inset are not negotiable: they are what make
/// a row of mixed controls read as one row. Same box drawn empty is the
/// strip's ghost, so populating a cell cannot move it.
pub fn field() -> Div {
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .h(px(crate::CONTROL_HEIGHT))
        .px(px(PICKER_CHIP_PAD))
        .overflow_hidden()
        .rounded(px(radius::ROW))
        .border_1()
        .border_color(glass::hairline(0.08))
        .bg(glass::ink(0.03))
        .text_size(px(12.0))
        .text_color(ladder::foreground())
}

// ---------------------------------------------------------------------------
// Anchored menus
// ---------------------------------------------------------------------------

/// The surface a floating *menu* is drawn on — smaller and one corner-step
/// tighter than a dialog card, because it hangs off a control rather than
/// standing on its own.
pub fn popover_card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(4.0))
        .rounded(px(radius::CARD))
        .border_1()
        .border_color(glass::hairline(0.10))
        .bg(glass::overlay())
        .text_size(px(13.0))
        .overflow_hidden()
}

/// Hang `content` off the trigger it is a child of — below it where there is
/// room, above it where there is not.
///
/// `deferred` lifts it onto a floating layer above everything painted so far,
/// and `anchored` positions it against the trigger. The zero-size wrapper pins
/// the origin to the trigger's *bottom* edge: without it the layer's static
/// position is subject to the trigger's own flex alignment, and an
/// `items_center` trigger would centre the whole menu.
///
/// **Which way it opens is not a parameter** — gpui's default fit mode
/// switches the anchor corner when the menu would not fit, so the window edge
/// decides and no caller can get it wrong. That is what a bottom-docked
/// consumer like the args strip needs: a menu that opened downward and then
/// *slid* up to fit (the snap fit mode) lands wherever the window bottom is,
/// covering everything between, while a switched anchor stays attached to the
/// control it belongs to.
///
/// `trigger` is how tall the control is. gpui switches the anchor about a
/// *single* point — here the trigger's bottom edge — so a flipped menu would
/// end at that same edge and paint over the very control it belongs to. The
/// height buys the way out: reserved as padding **below** the card, it is
/// invisible in the downward case and is exactly the distance the card has to
/// clear in the upward one. Hence padding rather than [`gpui::Anchored::offset`],
/// which is applied before the switch and therefore only ever points one way.
///
/// Only the card occludes — hitboxes are paint-order only in gpui, so a click
/// on a menu row would otherwise also fire whatever sits underneath the layer,
/// and reserved air that swallowed clicks would be a dead band under every
/// open menu.
pub fn anchored_below(
    id: impl Into<SharedString>,
    trigger: f32,
    content: AnyElement,
) -> AnyElement {
    hang(id, Side::Below, trigger, content)
}

/// Hang `content` off the *top* of the trigger it is a child of — the mirror
/// of [`anchored_below`], for a control that has nothing under it.
///
/// The whole rationale of [`anchored_below`] applies, reflected: the wrapper
/// pins the origin to the trigger's top edge, the anchor corner is the card's
/// bottom one, and the trigger's height is reserved as padding **above** the
/// card so that the downward flip near the window's *top* edge clears the
/// control instead of painting over it.
///
/// It also mirrors horizontally, hanging from the trigger's top-**right** and
/// growing left. That is not a second knob wearing a default: a control with
/// no room below it is one docked to the bottom of the window, those strips
/// run to the window's right edge, and a left-aligned card there overflows —
/// at which point gpui snaps it back inside and the card is no longer pinned
/// to anything. Aligning to the right edge is what keeps the pin.
pub fn anchored_above(
    id: impl Into<SharedString>,
    trigger: f32,
    content: AnyElement,
) -> AnyElement {
    hang(id, Side::Above, trigger, content)
}

/// Which edge of the trigger a menu hangs from. The only thing that differs
/// between [`anchored_below`] and [`anchored_above`] — everything else about
/// hanging a menu (the floating layer, the reserved air, the occlusion) is one
/// implementation below.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Below,
    Above,
}

fn hang(id: impl Into<SharedString>, side: Side, trigger: f32, content: AnyElement) -> AnyElement {
    let id = id.into();
    let reserved = px(trigger + MENU_GAP);
    let gap = px(MENU_GAP);
    let mut origin = div().absolute().size_0();
    let (anchor, card) = match side {
        Side::Below => {
            origin = origin.bottom_0().left_0();
            (gpui::Anchor::TopLeft, div().pt(gap).pb(reserved))
        }
        Side::Above => {
            origin = origin.top_0().right_0();
            (gpui::Anchor::BottomRight, div().pb(gap).pt(reserved))
        }
    };
    origin
        .child(
            gpui::deferred(gpui::anchored().anchor(anchor).child(motion::menu_in(
                id,
                card.child(div().occlude().child(content)),
            )))
            .priority(1),
        )
        .into_any_element()
}

/// Air between a trigger and the menu it opens — enough that the menu reads as
/// a separate object rather than the control growing out of itself.
const MENU_GAP: f32 = 6.0;

/// Hang `content` at a window-space point — the painted-canvas sibling of
/// [`anchored_below`], which pins to a parent element a canvas does not have
/// (a right-click hands you a `Point<Pixels>`, not a trigger). Same floating
/// layer, same anchor switching, same occlusion; only the origin differs, so a
/// menu opened from a canvas is the same object as one opened from a control —
/// including opening upward from a click near the window's bottom edge.
pub fn anchored_at(
    id: impl Into<SharedString>,
    at: gpui::Point<gpui::Pixels>,
    content: AnyElement,
) -> AnyElement {
    gpui::deferred(
        gpui::anchored()
            .position(at)
            .anchor(gpui::Anchor::TopLeft)
            .child(motion::menu_in(id.into(), div().occlude().child(content))),
    )
    .priority(1)
    .into_any_element()
}

// ---------------------------------------------------------------------------
// The picker loop
// ---------------------------------------------------------------------------

/// The query → rows → cursor loop every searchable list shares: filter rows
/// by a query, keep a wrapping keyboard cursor over what survived, and put the
/// cursor back on the first row whenever the rows or the query change.
///
/// State only. Rendering stays with the caller ([`menu_row`] and friends), and
/// so does scrolling — [`Picker::step`] hands back the new cursor position for
/// a `scroll_to_item`. Written here once because the loop had already been
/// written twice (the chat-history dialog and the group-expression editor's
/// suggestion menu) and a third copy — the graph editor's add-node palette —
/// was on its way.
///
/// The match rule is fixed at construction: what "matches the query" means is
/// the one fact that differs between pickers, and it belongs to the picker's
/// owner, not to every place that forwards a keystroke.
pub struct Picker<T> {
    rows: Vec<T>,
    matches: Box<dyn Fn(&T, &str) -> bool>,
    /// Most rows ever shown. A menu hanging off a text caret wants a cap
    /// (the expression editor shows ten); a dialog list wants them all.
    limit: usize,
    query: String,
    /// Indices into `rows`, in row order — the filter selects, it never
    /// clones.
    shown: Vec<usize>,
    cursor: usize,
}

impl<T> Picker<T> {
    pub fn new(matches: impl Fn(&T, &str) -> bool + 'static) -> Self {
        Self {
            rows: Vec::new(),
            matches: Box::new(matches),
            limit: usize::MAX,
            query: String::new(),
            shown: Vec::new(),
            cursor: 0,
        }
    }

    /// Cap how many rows [`Picker::shown`] yields, however many match.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self.refilter();
        self
    }

    /// Replace the rows. Refilters against the current query; the cursor goes
    /// back to the top, because it pointed into a list that no longer exists.
    pub fn set_rows(&mut self, rows: Vec<T>) {
        self.rows = rows;
        self.refilter();
    }

    /// Change the query. A no-op when the string is unchanged — callers may
    /// re-derive the query every frame, and an unchanged query must not eat
    /// the cursor position the arrow keys just set.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        if query == self.query {
            return;
        }
        self.query = query;
        self.refilter();
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows that survive the query, in row order, up to the limit.
    pub fn shown(&self) -> impl ExactSizeIterator<Item = &T> + '_ {
        self.shown.iter().map(|&at| &self.rows[at])
    }

    /// No row survives the query (or there are none).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// Where the keyboard cursor is, as a position in [`Picker::shown`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The row under the cursor.
    #[must_use]
    pub fn current(&self) -> Option<&T> {
        self.shown.get(self.cursor).map(|&at| &self.rows[at])
    }

    /// Walk the cursor by `delta`, wrapping at both ends. Returns the new
    /// cursor position for a `scroll_to_item`, or `None` when there is
    /// nothing to walk — the caller then neither scrolls nor redraws.
    pub fn step(&mut self, delta: isize) -> Option<usize> {
        let count = self.shown.len() as isize;
        if count == 0 {
            return None;
        }
        self.cursor = (self.cursor as isize + delta).rem_euclid(count) as usize;
        Some(self.cursor)
    }

    /// Put the cursor back on the first row without touching the query — for
    /// the moments a caller re-opens a menu whose query happens not to have
    /// changed.
    pub fn rewind(&mut self) {
        self.cursor = 0;
    }

    /// A whitespace-only query shows everything: matching is for words, and
    /// the trimmed query is what the match rule is handed.
    fn refilter(&mut self) {
        let query = self.query.trim();
        self.shown = if query.is_empty() {
            (0..self.rows.len()).take(self.limit).collect()
        } else {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| (self.matches)(row, query))
                .map(|(at, _)| at)
                .take(self.limit)
                .collect()
        };
        self.cursor = 0;
    }
}

// ---------------------------------------------------------------------------
// Async slots
// ---------------------------------------------------------------------------

/// Placeholder rows shown while a picker's list loads.
///
/// One shared pulse clock drives every skeleton in the window, so rows in
/// different dialogs stay phase-locked and an unmounted list schedules no
/// frames at all — see [`motion::pulse_delta`].
pub fn skeleton_rows(count: usize, view: gpui::EntityId, cx: &mut gpui::App) -> Div {
    let delta = motion::pulse_delta(&motion::PULSE, view, cx);
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .py(px(4.0))
        .children((0..count).map(move |index| {
            let phase = motion::staggered_phase(delta, index, SKELETON_STAGGER);
            div()
                .h(px(28.0))
                .rounded(px(radius::CONTROL))
                .bg(glass::ink(0.04))
                .opacity(SKELETON_DIM + SKELETON_LIFT * motion::pulse_wave(phase))
        }))
}

/// Per-row lag of the skeleton pulse, as a fraction of the period — enough for
/// the wave to read as travelling down the list rather than blinking at once.
const SKELETON_STAGGER: f32 = 0.08;
/// A skeleton row never goes fully dark: it is a placeholder, not a flash.
const SKELETON_DIM: f32 = 0.35;
/// …and never reaches full strength either, so it can't be mistaken for content.
const SKELETON_LIFT: f32 = 0.40;

/// Inline failure inside a picker: what went wrong, in the danger tone, where
/// the list would have been. The caller appends its own retry affordance.
pub fn error_row(message: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(8.0))
        .text_size(px(12.0))
        .text_color(ladder::danger())
        .child(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this module exists to not have: selection and the keyboard
    /// cursor resolving to the same paint.
    #[test]
    fn selection_outranks_the_cursor_and_never_shares_its_plate() {
        assert_eq!(RowState::of(false, false), RowState::Rest);
        assert_eq!(RowState::of(false, true), RowState::Cursor);
        assert_eq!(RowState::of(true, false), RowState::Selected);
        assert_eq!(
            RowState::of(true, true),
            RowState::Selected,
            "a row that is both is selected, not doubly painted"
        );
        assert_ne!(glass::card_cursor_bg(), glass::card_selected_bg());
    }

    /// A skeleton pulses between two states that are both visibly placeholder.
    #[test]
    fn a_skeleton_never_reaches_content_strength_or_darkness() {
        assert!(SKELETON_DIM > 0.0);
        assert!(SKELETON_DIM + SKELETON_LIFT < 1.0);
    }

    fn picker() -> Picker<&'static str> {
        let mut picker = Picker::new(|row: &&str, query: &str| {
            row.to_lowercase().contains(&query.to_lowercase())
        });
        picker.set_rows(vec!["alpha", "beta", "gamma", "beta_two"]);
        picker
    }

    /// The loop's contract: filter in row order, cursor to the top on every
    /// rows or query change, wrap at both ends, and treat a whitespace query
    /// as no query.
    #[test]
    fn the_picker_filters_steps_and_wraps() {
        let mut p = picker();
        assert_eq!(p.shown().count(), 4);
        assert_eq!(p.step(-1), Some(3), "stepping up from the top wraps");
        assert_eq!(p.current(), Some(&"beta_two"));

        p.set_query("BETA");
        assert_eq!(p.shown().copied().collect::<Vec<_>>(), ["beta", "beta_two"]);
        assert_eq!(p.cursor(), 0, "a new query rewinds the cursor");
        assert_eq!(p.step(1), Some(1));
        p.set_query("BETA");
        assert_eq!(p.cursor(), 1, "an unchanged query keeps the cursor");
        p.set_query("   ");
        assert_eq!(p.shown().count(), 4, "whitespace is no query");

        p.set_query("zzz");
        assert!(p.is_empty());
        assert_eq!(p.step(1), None, "nothing to walk, nothing to scroll");
        assert_eq!(p.current(), None);
    }

    /// The cap bounds what is shown, not what is held.
    #[test]
    fn the_picker_limit_caps_shown_rows() {
        let mut p =
            Picker::new(|row: &u32, query: &str| row.to_string().contains(query)).with_limit(2);
        p.set_rows((0..30).collect());
        assert_eq!(p.shown().count(), 2);
        p.set_query("1");
        assert_eq!(p.shown().copied().collect::<Vec<_>>(), [1, 10]);
    }

    /// Every radius this module paints is on the ladder — the check that
    /// catches a corner invented at a call site.
    #[test]
    fn every_corner_comes_from_the_vocabulary() {
        for corner in [radius::CAP, radius::CONTROL, radius::ROW, radius::MODAL] {
            assert!(radius::LADDER.contains(&corner), "{corner} is off-ladder");
        }
    }
}
