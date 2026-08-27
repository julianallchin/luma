//! The pattern-arg widget kit: the vocabulary a pattern-args inspector (the
//! track editor's bottom channel strip) and the graph editor's node params
//! both compose from.
//!
//! # The strip is the layout contract
//!
//! Every widget here is designed for one consuming shape: a **horizontal**
//! channel strip of fixed height, cells composing left-to-right. So each
//! widget is a cell — a 9px silkscreen label beside a [`CONTROL_HEIGHT`]
//! control — with a footprint that does not depend on its value, and a ghosted
//! rendering ([`arg_cell_ghost`]) for the strip's empty state that occupies
//! exactly the same box, so selection appearing never shifts the layout.
//!
//! # Values in, typed change events out
//!
//! No widget persists anything. Stateless widgets ([`select::luma_arg_select`],
//! [`palette::luma_palette_row`], [`gradient::luma_gradient_bar`],
//! [`color::luma_hsv_picker`]) are free
//! functions in the crate's usual shape: the caller passes the value and a
//! closure hears a typed event. Widgets that buffer *drafts* — text being
//! typed is not a value yet — are entities, for the same reason
//! [`crate::text_input::TextInput`] is: [`number::DraftedNumber`] and
//! [`expression::GroupExpressionEditor`] own the draft and emit only committed
//! values, so a host never sees intermediate garbage.
//!
//! Colors, as everywhere, come from [`crate::ladder`]; the one addition this
//! kit makes is *no* addition — token highlight colors are the ladder's
//! existing status and label inks, and nothing here mints a grey.

pub mod color;
pub mod expression;
pub mod gradient;
pub mod number;
pub mod palette;
pub mod select;

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    canvas, div, point, px, App, Bounds, Div, DragMoveEvent, Pixels, Point, SharedString, Window,
};

use crate::{ladder, text, CONTROL_HEIGHT};

/// Where a stateless control's box landed, readable by its own mouse
/// listeners.
///
/// A click event carries a window position but not the element's bounds, and a
/// stateless widget has no entity to remember them in. So the widget lays an
/// invisible [`canvas`] into its box that records the bounds during paint;
/// paint runs before that frame's mouse events dispatch, so by the time a
/// click listener reads the cell it holds this frame's geometry. (Drags don't
/// need this — `DragMoveEvent::bounds` carries the listener's box — which is
/// why only click-addressed widgets carry a probe.)
pub(crate) fn bounds_probe() -> (Rc<Cell<Option<Bounds<Pixels>>>>, impl IntoElement) {
    let cell: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    let write = cell.clone();
    let probe = canvas(move |bounds, _, _| write.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .size_full();
    (cell, probe)
}

/// A window-space x mapped to a fraction of `bounds`' width, clamped into
/// `0..=1`. `None` while the probe has not painted yet.
pub(crate) fn fraction_of(bounds: &Cell<Option<Bounds<Pixels>>>, x: Pixels) -> Option<f32> {
    let bounds = bounds.get()?;
    let span = f32::from(bounds.size.width);
    if span <= 0. {
        return None;
    }
    Some((f32::from(x - bounds.left()) / span).clamp(0., 1.))
}

/// A drag payload that names the control it started on.
///
/// gpui routes a drag by the *type* of its payload, so every control of one
/// kind hears every drag of that kind — the id is what lets a listener keep
/// only its own; without it, dragging one slider moves all of them.
/// [`drag_fraction`] is the listener half of the idiom; a payload that is
/// dropped rather than tracked (the palette's swatch) shares the naming and
/// does its own guard.
pub(crate) trait OwnedDrag {
    fn owner(&self) -> &SharedString;
}

/// The listener for a fraction-mapped drag: keep only the drags `owner`
/// started, map the pointer into a 0..=1 fraction of the listener's own box
/// (both axes; take the one that means something), and hand it to `apply`
/// with the payload. `event.bounds` is the element's *live* box, so the
/// mapping follows the control wherever layout put it.
///
/// The value derived from the fraction is absolute, not a delta — a drag that
/// outruns the box clamps instead of winding up (see `slider.rs`'s module
/// docs). A degenerate box maps to nothing.
pub(crate) fn drag_fraction<T: OwnedDrag + Clone + 'static>(
    owner: SharedString,
    apply: impl Fn(Point<f32>, &T, &mut Window, &mut App) + 'static,
) -> impl Fn(&DragMoveEvent<T>, &mut Window, &mut App) {
    move |event, window, cx| {
        let drag = event.drag(cx);
        if drag.owner() != &owner {
            return;
        }
        let drag = drag.clone();
        let bounds = event.bounds;
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if w <= 0. || h <= 0. {
            return;
        }
        let at = point(
            (f32::from(event.event.position.x - bounds.left()) / w).clamp(0., 1.),
            (f32::from(event.event.position.y - bounds.top()) / h).clamp(0., 1.),
        );
        apply(at, &drag, window, cx);
    }
}

/// The seam between a cell's label and its control.
const LABEL_GAP: f32 = 8.;

/// Height of a whole cell. One control row — the label rides *beside* the
/// control, not above it, so the strip is exactly as tall as the tallest thing
/// in it.
pub const CELL_HEIGHT: f32 = CONTROL_HEIGHT;

/// One slot of the strip: silkscreen label, then the control, on one line.
///
/// The label is inline rather than stacked above because this kit's consumer
/// is a strip docked to the *bottom* of a window, where every menu opens
/// upward and a label overhead is the first thing it covers. Beside the
/// control it stays legible with a menu up, and the strip loses a row of
/// height it was only spending on labels.
///
/// The row is a fixed-height, shrink-proof box, so a control that measures
/// oddly cannot move the cell's bottom edge. [`arg_cell_ghost`] goes through
/// here too; the two boxes cannot diverge because there is only one.
pub fn arg_cell(label: &str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(LABEL_GAP))
        .flex_shrink_0()
        .h(px(CELL_HEIGHT))
        .child(
            div()
                .flex_shrink_0()
                .whitespace_nowrap()
                .child(text::silkscreen(label.to_uppercase())),
        )
        .child(control)
}

/// The strip's empty state for one slot: same label, same box, and a dimmed
/// empty [`crate::float::field`] where the control would be. Same footprint as
/// [`arg_cell`] by construction — the whole point is that populating the strip
/// moves nothing — and the same *shape*, because a populated cell draws that
/// very field around its editor.
pub fn arg_cell_ghost(label: &str, width: f32) -> Div {
    arg_cell(
        label,
        crate::float::field()
            .w(px(width))
            .opacity(ladder::DISABLED_OPACITY),
    )
    .opacity(ladder::DISABLED_OPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cell is one control row tall — the label rides beside the control,
    /// so nothing above or below it may add height. Guards a future hand-edit
    /// of `CELL_HEIGHT` to a literal that then drifts from what `arg_cell`
    /// actually lays out.
    #[test]
    fn the_cell_is_one_control_row_tall() {
        assert_eq!(CELL_HEIGHT, CONTROL_HEIGHT);
    }
}
