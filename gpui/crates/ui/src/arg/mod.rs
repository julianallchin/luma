//! The pattern-arg widget kit: the vocabulary a pattern-args inspector (the
//! track editor's args sheet) and the graph editor's node params both compose
//! from.
//!
//! # One arg per row
//!
//! Every widget here is designed for one consuming shape: a **vertical**
//! column of full-width rows ([`arg_row`]), label over control. A row is the
//! shape that stops being wrong as a schema grows — a horizontal strip of
//! cells runs a pattern's third arg off the right edge of the window, where no
//! amount of scrolling makes it a control anybody finds — and it is the shape
//! a node's params want too, which is why it is stated once here.
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

use crate::text;

/// Where a stateless control's box landed, readable by its own mouse
/// listeners.
///
/// Public because it is not really an arg-editor's device: any element whose
/// click handler needs its *own* geometry has the same problem, and a second
/// copy of this canvas is how two of them drift.
///
/// A click event carries a window position but not the element's bounds, and a
/// stateless widget has no entity to remember them in. So the widget lays an
/// invisible [`canvas`] into its box that records the bounds during paint;
/// paint runs before that frame's mouse events dispatch, so by the time a
/// click listener reads the cell it holds this frame's geometry. (Drags don't
/// need this — `DragMoveEvent::bounds` carries the listener's box — which is
/// why only click-addressed widgets carry a probe.)
pub fn bounds_probe() -> (Rc<Cell<Option<Bounds<Pixels>>>>, impl IntoElement) {
    let cell: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    let probe = bounds_into(&cell);
    (cell, probe)
}

/// The same probe, writing into a cell the caller already owns — for an
/// element whose geometry outlives one frame's builder (a view that remembers
/// where its list painted, so a later click can measure against it).
pub fn bounds_into(cell: &Rc<Cell<Option<Bounds<Pixels>>>>) -> impl IntoElement {
    let write = Rc::clone(cell);
    canvas(move |bounds, _, _| write.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .size_full()
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

/// The seam between a row's label and its control.
const LABEL_GAP: f32 = 6.;

/// One arg of a schema: a 9px silkscreen label with its control under it,
/// spanning the column's whole width.
///
/// Label *over* rather than beside, which is the one thing this shape decides:
/// beside, every row spends its label's width — a few hundred pixels across a
/// schema — on words, and the controls that are left are as narrow as the
/// longest label is long. Over, every control gets the full column whatever it
/// is called, so a colour picker and an expression field are the same width
/// and the column reads as one stack rather than as ragged pairs.
pub fn arg_row(label: &str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(LABEL_GAP))
        .w_full()
        .child(text::silkscreen(label.to_uppercase()))
        .child(control)
}
