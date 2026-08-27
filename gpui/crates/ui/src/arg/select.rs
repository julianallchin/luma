//! The strip's value picker: a `<Selector>` trigger plus its open menu, wired.
//!
//! The blend-mode cell is this widget verbatim. The nine blend names are
//! deliberately **not** written down here: the canonical list is
//! `luma_lib::models::node_graph::BlendMode` (and the score DSL's
//! `blend_mode_name` beside it), and this crate deliberately does not depend
//! on Luma's core — the same boundary [`crate::ladder::port`] documents. The
//! integration matches exhaustively on `BlendMode` to produce `options`, so a
//! new mode is a compile error there instead of a silent omission here.
//!
//! Open state is the caller's, as it is for every menu in this crate: the
//! strip already owns "which cell has its menu open", and a second, hidden
//! store inside the widget is how two menus end up open at once. What this
//! module adds over the raw pieces is the *wiring* — trigger, anchored menu,
//! per-item clicks.
//!
//! Trigger and menu are both on the float tier — [`crate::float::picker_chip`]
//! under [`crate::float::popover_card`], hung by
//! [`crate::float::anchored_below`]. A dropdown is one object, and its menu
//! always floats, so an opaque square slab opening a rounded translucent card
//! was two components wearing one name. See `picker_chip`'s own note.

use gpui::prelude::*;
use gpui::{div, px, App, Div, ElementId, SharedString, Window};

use crate::node::{Instrument, Role};
use crate::{float, luma_select_item, CONTROL_HEIGHT};

/// The trigger, ghost-sized to the widest option, and — while `open` — its
/// menu, floated by [`crate::float::anchored_below`] (which is what keeps it
/// inside the window wherever the trigger sits). `on_toggle` fires on the
/// trigger; `on_pick` fires with the picked option's index, and closing the
/// menu on pick is the caller's move (it owns the flag).
pub fn luma_arg_select(
    id: impl Into<SharedString>,
    value: &str,
    options: &[&str],
    open: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    on_pick: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let id = id.into();
    let trigger = float::picker_chip(value, options)
        .id(ElementId::Name(id.clone()))
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .agent_node(Role::Select, value);
    let value = value.to_string();
    // The menu is a float, and floats size like floats: an explicit
    // comfortable minimum, not the trigger's width — the same choice every
    // popover in the app makes (`add_tracks::source_menu` sets 248).
    let menu_id: SharedString = format!("{id}:menu").into();
    div().relative().child(trigger).when(open, |el| {
        el.child(float::anchored_below(
            menu_id,
            CONTROL_HEIGHT,
            options
                .iter()
                .enumerate()
                .fold(
                    float::popover_card().min_w(px(160.)),
                    |menu, (index, option)| {
                        let on_pick = on_pick.clone();
                        menu.child(
                            luma_select_item(option, float::RowState::of(*option == value, false))
                                .id(ElementId::Name(format!("{id}:{option}").into()))
                                .on_click(move |_, window, cx| on_pick(index, window, cx))
                                .agent_node(Role::Button, option.to_string()),
                        )
                    },
                )
                .into_any_element(),
        ))
    })
}
