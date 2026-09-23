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

/// Logical visibility plus a retained, noninteractive closing frame.
#[derive(Clone, Copy, Default)]
pub enum MenuVisibility {
    #[default]
    Closed,
    Open,
    Closing(std::time::Instant),
}
impl From<bool> for MenuVisibility {
    fn from(open: bool) -> Self {
        if open {
            Self::Open
        } else {
            Self::Closed
        }
    }
}
impl MenuVisibility {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
    pub fn close(&mut self) {
        if self.is_open() {
            *self = Self::Closing(std::time::Instant::now());
        }
    }
    pub fn toggle(&mut self) {
        if self.is_open() {
            self.close();
        } else {
            *self = Self::Open;
        }
    }
    pub fn tick_close(&mut self, reduced_motion: bool) -> bool {
        if let Some(t) = self.exit() {
            if reduced_motion || t >= 1.0 {
                *self = Self::Closed;
            } else {
                return true;
            }
        }
        false
    }
    fn exit(self) -> Option<f32> {
        match self {
            Self::Closing(since) => Some(crate::motion::exit_progress(
                &crate::motion::MENU_OUT,
                since,
            )),
            _ => None,
        }
    }
}

/// The trigger, ghost-sized to the widest option, and — while `open` — its
/// menu, floated by [`crate::float::anchored_below`] (which is what keeps it
/// inside the window wherever the trigger sits). `on_toggle` fires on the
/// trigger; `on_pick` fires with the picked option's index, and closing the
/// menu on pick is the caller's move (it owns the flag).
pub fn luma_arg_select(
    id: impl Into<SharedString>,
    value: &str,
    options: &[&str],
    visibility: impl Into<MenuVisibility>,
    on_toggle: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_pick: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let visibility = visibility.into();
    let open = visibility.is_open();
    let closing = visibility.exit();
    let id = id.into();
    // The trigger's press and the dismissing press are the same gesture seen
    // from two sides: a click on an open trigger lands *outside* the card, so
    // `Dismiss` swallows it and closes the menu, and the toggle below never
    // runs. Which is the wanted outcome — a toggle that also fired would close
    // and reopen in one press.
    let dismiss = on_toggle.clone();
    let trigger = float::picker_chip(value, options)
        .id(ElementId::Name(id.clone()))
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .agent_node(Role::Select, value);
    let value = value.to_string();
    // The menu is a float, and floats size like floats: an explicit
    // comfortable minimum, not the trigger's width — the same choice every
    // popover in the app makes (`add_tracks::source_menu` sets 248).
    let menu_id: SharedString = format!("{id}:menu").into();
    div()
        .relative()
        .child(trigger)
        .when(open || closing.is_some_and(|t| t < 1.0), |el| {
            let content = options
                .iter()
                .enumerate()
                .fold(
                    float::popover_card().min_w(px(144.)).gap(px(0.)).p(px(3.)),
                    |menu, (index, option)| {
                        let on_pick = on_pick.clone();
                        menu.child(
                            luma_select_item(option, float::RowState::of(*option == value, false))
                                .h(px(26.))
                                .py(px(0.))
                                .text_size(px(12.))
                                .id(ElementId::Name(format!("{id}:{option}").into()))
                                .on_click(move |_, window, cx| {
                                    if open {
                                        on_pick(index, window, cx);
                                    }
                                })
                                .agent_node(Role::Button, option.to_string()),
                        )
                    },
                )
                .into_any_element();
            el.child(match closing {
                Some(t) => float::anchored_below_closing(menu_id, CONTROL_HEIGHT, content, t),
                None => float::anchored_below(
                    menu_id,
                    CONTROL_HEIGHT,
                    float::Dismiss::on_press_out(move |window, cx| dismiss(window, cx)),
                    content,
                ),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_is_retained_then_removed_and_can_reopen() {
        let mut state = MenuVisibility::Open;
        state.close();
        assert!(!state.is_open());
        assert!(state.tick_close(false));
        state.toggle();
        assert!(state.is_open());
        state.close();
        assert!(!state.tick_close(true));
        assert!(matches!(state, MenuVisibility::Closed));
        state =
            MenuVisibility::Closing(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(!state.tick_close(false));
        assert!(matches!(state, MenuVisibility::Closed));
    }
}
