//! The Perform screen's fader: a track its value fills, with a bright edge
//! where the value is. The value box every other number is set with is
//! [`crate::float::scrub`], and the fader drags by the same rules.

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::arg::{drag_fraction, OwnedDrag};
use crate::drag::DragGhost;
use crate::node::{Instrument, Role};
use crate::{glass, ladder, radius};

/// A fader drag in flight, and *which* fader is in it. gpui routes a drag by
/// its payload's type, so every fader hears every fader's drag; the id is how
/// a listener knows the move is its own.
#[derive(Clone)]
struct SliderDrag {
    id: SharedString,
}

impl OwnedDrag for SliderDrag {
    fn owner(&self) -> &SharedString {
        &self.id
    }
}

/// The fader of the Perform screen, live: [`fader_track`] that a drag moves.
/// 0–1, and the same drag rules as [`crate::float::scrub`]: absolute, and
/// only once the pointer moves.
///
/// It has no size of its own: it fills the box its caller gives it, so a
/// tall cell gets a tall fader and a wide one a wide fader.
pub fn luma_fader(
    id: impl Into<SharedString>,
    value: f32,
    axis: Axis,
    tint: Hsla,
    on_change: impl Fn(f32, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let moved = id.clone();
    let vertical = axis == Axis::Vertical;
    fader_track(value, axis, tint)
        // The value, published for a driver, as the slider's readout is.
        .child(div().agent_node(Role::Text, format!("{id} = {:.2}", value.clamp(0., 1.))))
        .id(ElementId::Name(id.clone()))
        .when(vertical, |el| el.cursor_ns_resize())
        .when(!vertical, |el| el.cursor_ew_resize())
        .on_drag(SliderDrag { id }, |_, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| DragGhost)
        })
        .on_drag_move(drag_fraction(
            moved,
            move |at, _: &SliderDrag, window, cx| {
                on_change(if vertical { 1. - at.y } else { at.x }, window, cx)
            },
        ))
}

/// A fader as it looks, without the drag: a recessed track that the value
/// fills, from the bottom (`Axis::Vertical`) or from the left, in `tint`,
/// with a bright edge where the value is. For a fader shown where a drag
/// means something else, such as a layout being arranged.
pub fn fader_track(value: f32, axis: Axis, tint: Hsla) -> Div {
    let fraction = value.clamp(0., 1.);
    let vertical = axis == Axis::Vertical;
    let fill = div()
        .absolute()
        .bottom_0()
        .left_0()
        .bg(tint.opacity(0.45))
        .when(vertical, |fill| fill.w_full().h(relative(fraction)))
        .when(!vertical, |fill| fill.h_full().w(relative(fraction)));
    // The edge rides the fill's end, inside the track at both ends.
    let edge = div()
        .absolute()
        .bg(ladder::foreground_alpha(0.9))
        .when(vertical, |edge| {
            edge.left_0()
                .w_full()
                .h(px(FADER_EDGE))
                .bottom(relative(fraction))
                .mb(px(-FADER_EDGE * fraction))
        })
        .when(!vertical, |edge| {
            edge.top_0()
                .h_full()
                .w(px(FADER_EDGE))
                .left(relative(fraction))
                .ml(px(-FADER_EDGE * fraction))
        })
        .when(fraction <= 0., |edge| edge.invisible());
    div()
        .relative()
        .overflow_hidden()
        .rounded(px(radius::CONTROL))
        .bg(glass::scrim(0.28))
        .child(fill)
        .child(edge)
}

/// How thick the bright edge at a fader's value is.
const FADER_EDGE: f32 = 2.;
