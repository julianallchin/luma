//! A curve picker: a chip that shows the current curve as a small picture,
//! and a popover grid of named curves, one thumbnail each.
//!
//! Stateless like [`super::select::luma_arg_select`]: the caller owns the open
//! flag, `on_toggle` fires on the trigger and `on_pick` with the picked
//! option's index. Closing the popover on pick is the caller's move.

use gpui::prelude::*;
use gpui::{canvas, div, px, App, Div, ElementId, SharedString, Window};
use gpui_component::tooltip::Tooltip;
use luma_patterns::Envelope;

use super::envelope::paint_envelope;
use super::select::MenuVisibility;
use crate::node::{Instrument, Role};
use crate::{float, glass, ladder, select, CONTROL_HEIGHT};

/// What the trigger calls a curve that is none of the options.
pub const CUSTOM: &str = "Custom";

/// Thumbnails per row of the popover grid.
const COLUMNS: usize = 4;
/// A grid cell, and the curve picture inside it.
const CELL: [f32; 2] = [52., 36.];
const THUMB: [f32; 2] = [36., 20.];
/// The picture on the trigger.
const CHIP_THUMB: [f32; 2] = [24., 12.];

/// A small picture of `value`'s line, `size` wide and high.
pub fn curve_thumb(value: &Envelope, size: [f32; 2], alpha: f32) -> impl IntoElement {
    let value = value.clone();
    div().w(px(size[0])).h(px(size[1])).flex_none().child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                // Inset by the stroke, so a line along an edge stays whole.
                paint_envelope(
                    window,
                    bounds.inset(px(1.)),
                    &value,
                    px(1.25),
                    ladder::foreground_alpha(alpha),
                );
            },
        )
        .size_full(),
    )
}

/// The trigger's label: the current option's name, or [`CUSTOM`].
fn label(options: &[(SharedString, Envelope)], current: Option<usize>) -> SharedString {
    current
        .and_then(|at| options.get(at))
        .map_or(CUSTOM.into(), |(name, _)| name.clone())
}

/// The trigger, and while open, the grid of `options` under it. `value` is the
/// curve the trigger draws; `current` is the option it equals, if any.
pub fn luma_curve_picker(
    id: impl Into<SharedString>,
    value: &Envelope,
    current: Option<usize>,
    options: &[(SharedString, Envelope)],
    visibility: impl Into<MenuVisibility>,
    on_toggle: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_pick: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let visibility = visibility.into();
    let open = visibility.is_open();
    let closing = visibility.exit();
    let id = id.into();
    let shown = label(options, current);
    // The name sits in the same ghost stack a select uses, so the chip keeps
    // one width whichever curve it shows.
    let names = options
        .iter()
        .map(|(name, _)| name.to_string())
        .chain([CUSTOM.to_string()])
        .collect();
    let dismiss = on_toggle.clone();
    let trigger = float::chip_plate(crate::Enabled::Yes)
        .px(px(float::PICKER_CHIP_PAD))
        .child(curve_thumb(value, CHIP_THUMB, 0.9))
        .child(select::ghost_stack(
            div().relative().flex(),
            shown.to_string(),
            names,
            0.,
            ladder::foreground_alpha(0.45),
        ))
        .id(ElementId::Name(id.clone()))
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .agent_node(Role::Select, shown.to_string());
    let menu_id: SharedString = format!("{id}:menu").into();
    div()
        .relative()
        .child(trigger)
        .when(open || closing.is_some_and(|t| t < 1.0), |el| {
            let grid = div()
                .w(px(CELL[0] * COLUMNS as f32))
                .flex()
                .flex_row()
                .flex_wrap()
                .children(options.iter().enumerate().map(|(index, (name, curve))| {
                    let on_pick = on_pick.clone();
                    let tip = name.clone();
                    let chosen = current == Some(index);
                    let cell_id: SharedString = format!("{id}:{index}").into();
                    div()
                        .w(px(CELL[0]))
                        .h(px(CELL[1]))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(crate::radius::ROW))
                        .cursor_pointer()
                        .when(chosen, |cell| cell.bg(glass::card_selected_bg()))
                        .when(!chosen, |cell| {
                            cell.hover(|style| style.bg(glass::glass_hover()))
                        })
                        .child(curve_thumb(curve, THUMB, if chosen { 1. } else { 0.7 }))
                        .id(ElementId::Name(cell_id))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .on_click(move |_, window, cx| {
                            if open {
                                on_pick(index, window, cx);
                            }
                        })
                        .agent_node(Role::Button, name.to_string())
                }));
            let content = float::popover_card()
                .p(px(4.))
                .child(grid)
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
    fn the_trigger_names_the_current_option_or_custom() {
        let options = [
            ("Ramp up".into(), Envelope::linear(vec![[0., 0.], [1., 1.]])),
            ("Flat".into(), Envelope::linear(vec![[0., 1.], [1., 1.]])),
        ];
        assert_eq!(label(&options, Some(1)), "Flat");
        assert_eq!(label(&options, None), CUSTOM);
        assert_eq!(label(&options, Some(5)), CUSTOM);
    }
}
