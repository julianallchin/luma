//! The palette editor: an ordered row of swatches — add, remove, reorder,
//! select. K colors with uniform spacing; there is no `t` here, which is the
//! whole difference from [`super::gradient`].
//!
//! Stateless: the host owns the `Vec` of colors and the selection, hears a
//! typed [`PaletteEvent`], and pairs the selection with a color editor of its
//! choosing (the kit's [`super::color::luma_hsv_picker`], usually). Reorder is
//! drag-and-drop between swatches; the web reference has no reorder at all —
//! rows there are fixed — but the brief's strip is horizontal and ordered, so
//! the drop targets are the swatches themselves.

use gpui::prelude::*;
use gpui::{div, px, App, Div, ElementId, Rgba, SharedString, Window};

use crate::drag::DragGhost;
use crate::ladder;
use crate::node::{Instrument, Role};

use super::OwnedDrag;
use crate::CONTROL_HEIGHT;

/// What the row tells its host. All of these are *requests* against the
/// host's `Vec`; the row re-renders from whatever the host decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteEvent {
    /// The `+` slab: append a copy of the last color (the web behavior).
    Add,
    /// The `×` slab: remove this index. Offered only while more than one
    /// color remains — a palette of zero colors paints nothing.
    Remove(usize),
    Select(usize),
    /// A swatch was dropped on another: move `from` to `to`'s position.
    Move {
        from: usize,
        to: usize,
    },
}

/// A swatch being carried, routed by row id so two palettes on screen cannot
/// drop into each other. A drop has no fraction to map, so the guard below is
/// written out rather than going through `drag_fraction`.
#[derive(Clone)]
struct SwatchDrag {
    id: SharedString,
    from: usize,
}

impl OwnedDrag for SwatchDrag {
    fn owner(&self) -> &SharedString {
        &self.id
    }
}

/// The row: one square per color, then `+`, then `×` for the selection.
/// Selection shows as a [`ladder::primary`] ring, the checkbox's own device.
pub fn luma_palette_row(
    id: impl Into<SharedString>,
    colors: &[Rgba],
    selected: Option<usize>,
    on_event: impl Fn(PaletteEvent, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let id = id.into();
    let count = colors.len();
    let add_id = ElementId::Name(format!("{id}:add").into());
    let remove_id = ElementId::Name(format!("{id}:remove").into());

    let swatches = colors.iter().enumerate().map(|(index, color)| {
        let is_selected = selected == Some(index);
        let select = on_event.clone();
        let drop = on_event.clone();
        let drop_id = id.clone();
        div()
            .id(ElementId::Name(format!("{id}:swatch:{index}").into()))
            .flex_shrink_0()
            .size(px(CONTROL_HEIGHT))
            .bg(*color)
            .map(|el| {
                if is_selected {
                    el.border_2().border_color(ladder::primary())
                } else {
                    el.border_1().border_color(ladder::control_border())
                }
            })
            .on_click(move |_, window, cx| select(PaletteEvent::Select(index), window, cx))
            .on_drag(
                SwatchDrag {
                    id: id.clone(),
                    from: index,
                },
                |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| DragGhost)
                },
            )
            .on_drop(move |drag: &SwatchDrag, window, cx| {
                if drag.owner() != &drop_id || drag.from == index {
                    return;
                }
                drop(
                    PaletteEvent::Move {
                        from: drag.from,
                        to: index,
                    },
                    window,
                    cx,
                );
            })
            .agent_node(Role::Button, format!("{id}:swatch:{index}"))
    });

    let add = on_event.clone();
    let row = div()
        .flex()
        .items_center()
        .gap(px(4.))
        .children(swatches)
        .child(
            action_slab("+")
                .id(add_id)
                .on_click(move |_, window, cx| add(PaletteEvent::Add, window, cx))
                .agent_node(Role::Button, "add color"),
        );
    match selected {
        Some(index) if count > 1 => {
            let remove = on_event;
            row.child(
                action_slab("×")
                    .id(remove_id)
                    .on_click(move |_, window, cx| {
                        remove(PaletteEvent::Remove(index), window, cx);
                    })
                    .agent_node(Role::Button, "remove color"),
            )
        }
        _ => row,
    }
}

/// The `+` / `×` slab: a square button in the control voice.
fn action_slab(glyph: &str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .size(px(CONTROL_HEIGHT))
        .border_1()
        .border_color(ladder::control_border())
        .bg(ladder::control())
        .text_size(px(12.))
        .text_color(ladder::foreground_90())
        .hover(|s| s.bg(ladder::hover()).text_color(ladder::foreground()))
        .child(glyph.to_string())
}
