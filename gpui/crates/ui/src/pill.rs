//! The capsule tier: the controls a screen with no card on it is made of.
//!
//! # Where these live
//!
//! Their home is the sign-in screen (`luma-app`'s `signin`), which is not a
//! dialog and has no surface of its own — the app's ground edge to edge, one
//! centred column, nothing else. A screen like that cannot borrow the
//! [`crate::float`] tier: `float`'s buttons are sized to ride *inside* a card's
//! bands (a 32px box, a 22px cap), and a 32px button alone in the middle of an
//! empty window reads as a fragment that lost its card. So the capsule is its
//! own step: one width, one height, fully rounded, stated once here.
//!
//! There is exactly one of each — a filled [`primary`], an outlined
//! [`secondary`], a [`field`] that holds a real editor, and a [`link`] for the
//! quietest way out. A second emphasised capsule would mean two primary
//! actions, which is a design bug rather than a missing function.
//!
//! The emphasised fill is [`crate::float`]'s, not a second one: a white plate
//! with near-black text is a *statement about ink*, and it must not drift
//! between the card tier and this one.

use gpui::prelude::*;
use gpui::{div, px, Div, FontWeight, SharedString};

use crate::{float, glass, ladder, radius, Control, Enabled};

/// The column's width — every capsule on the screen is this wide, including
/// the field, so the stack reads as one object seen from the front rather than
/// as a pile of differently-sized parts.
pub const WIDTH: f32 = 380.0;

/// The capsule's height, on every page of the gate: a step above a
/// [`choice`], so a page's one action leads without standing a head taller
/// than what it sits among. [`radius::PILL`] is at least half of it, which
/// is what makes the shape a capsule rather than a rounded box.
pub const HEIGHT: f32 = 42.0;

/// Between two capsules in the stack.
pub const GAP: f32 = 16.0;

/// Between the title block and the first capsule under it. Wider than [`GAP`]:
/// the stack is one group, and the heading is not part of it.
pub const HEAD_GAP: f32 = 24.0;

/// The label size a capsule carries: a half step above the card tier's 13,
/// and the size a [`choice`] carries, so a page of choices and the capsule
/// under them are one scale.
const LABEL: f32 = 13.5;

/// The shared box. Geometry once, so the three capsules cannot drift apart in
/// anything but colour.
fn capsule() -> Div {
    div()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .rounded(px(radius::PILL))
        .text_size(px(LABEL))
}

/// The one emphasised capsule: a white plate with the ground's own colour
/// written on it. One per route, or the route has no primary action.
///
/// [`Enabled::No`] is a *recessed* plate rather than the white one at low
/// opacity, and the difference matters at this size: fading a 380x52 plate
/// fades its dark label with it, so the one word the capsule exists to say
/// goes first. A plate that steps down and a label that stays legible says
/// "not yet"; a ghost of a plate says nothing at all.
///
/// Extra children (a leading glyph) go on after the label, so the caller does
/// not need a second constructor to put a mark in front of a word.
pub fn primary(label: impl Into<SharedString>, enabled: Enabled) -> Control {
    let plate = capsule().justify_center().gap(px(8.0)).px(px(20.0));
    let plate = match enabled {
        Enabled::Yes => float::btn_primary_paint(plate.cursor_pointer()),
        Enabled::No => plate
            .bg(glass::ink(0.13))
            .font_weight(FontWeight::MEDIUM)
            .text_color(ladder::foreground_alpha(0.38)),
    };
    Control::new(plate.child(label.into()), enabled)
}

/// The alternative to [`primary`]: the same capsule with no fill, an edge, and
/// the screen's own foreground.
///
/// An outline rather than a dimmer plate because the pair sits on bare ground —
/// two fills a value apart would read as one control drawn twice.
pub fn secondary(label: impl Into<SharedString>) -> Div {
    capsule()
        .justify_center()
        .gap(px(8.0))
        .cursor_pointer()
        .px(px(20.0))
        .border_1()
        .border_color(glass::hairline(0.16))
        .text_color(ladder::foreground_alpha(0.92))
        .font_weight(FontWeight::MEDIUM)
        .hover(|style| style.bg(glass::ink(0.06)))
        .child(label.into())
}

/// One answer among several on a gate page: a small capsule that is either
/// taken or not. Taken is [`primary`]'s white plate, so a choice reads from
/// across the room; not taken is [`secondary`]'s outline. Sized to its
/// label, so a row of them wraps.
///
/// The caller adds the `.id()` and the click.
pub fn choice(label: impl Into<SharedString>, taken: bool) -> Div {
    let shape = div()
        .h(px(CHOICE_HEIGHT))
        .px(px(16.0))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .rounded(px(radius::PILL))
        .text_size(px(LABEL))
        .cursor_pointer();
    if taken {
        float::btn_primary_paint(shape)
    } else {
        shape
            .border_1()
            .border_color(glass::hairline(0.22))
            .text_color(ladder::foreground_alpha(0.92))
            .font_weight(FontWeight::MEDIUM)
            .hover(|style| style.bg(glass::ink(0.08)))
    }
    .child(label.into())
}

/// How tall a [`choice`] stands.
pub const CHOICE_HEIGHT: f32 = 36.0;

/// The capsule a [`crate::text_input::TextInput`] is dropped into: recessed
/// fill, one hairline, and the padding that puts the caret where a label would
/// have been.
///
/// It takes the field as a child rather than owning one, because an editor is
/// an entity the host keeps across frames and this tier holds no state.
pub fn field() -> Div {
    capsule()
        .px(px(22.0))
        .bg(glass::ink(0.04))
        .border_1()
        .border_color(glass::hairline(0.12))
}

/// The quietest way off a screen: small, dim, underlined — a word, not a
/// control. Below the capsules, never among them.
pub fn link(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .cursor_pointer()
        .text_size(px(12.5))
        .underline()
        .text_decoration_color(ladder::foreground_alpha(0.30))
        .text_color(ladder::foreground_alpha(0.50))
        .hover(|style| style.text_color(ladder::foreground_alpha(0.85)))
        .child(label.into())
}
