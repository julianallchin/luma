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

/// Most thumbnails in one row of the popover grid.
const COLUMNS: usize = 5;
/// A thumbnail's picture, and the cell that holds it and its name.
const THUMB: [f32; 2] = [56., 36.];
const CELL_W: f32 = 76.;
/// The picture on the trigger.
const CHIP_THUMB: [f32; 2] = [28., 14.];

/// How many thumbnails each row holds: as few rows as [`COLUMNS`] allows,
/// filled evenly, so 7 lay out as 4 + 3 and not 5 + 2.
fn columns(count: usize) -> usize {
    let rows = count.div_ceil(COLUMNS).max(1);
    count.div_ceil(rows).max(1)
}

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
    // A flex row, so the chip keeps its own width in a stretching column.
    div()
        .relative()
        .flex()
        .child(trigger)
        .when(open || closing.is_some_and(|t| t < 1.0), |el| {
            let per_row = columns(options.len());
            let cell = |index: usize, name: &SharedString, curve: &Envelope| {
                let on_pick = on_pick.clone();
                let tip = name.clone();
                let chosen = current == Some(index);
                div()
                    .w(px(CELL_W))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .p(px(6.))
                    .rounded(px(crate::radius::ROW))
                    .cursor_pointer()
                    .when(chosen, |cell| {
                        cell.bg(glass::card_selected_bg())
                            .shadow(glass::card_selected_shadows())
                    })
                    .when(!chosen, |cell| {
                        cell.hover(|style| style.bg(glass::glass_hover()))
                    })
                    .child(curve_thumb(curve, THUMB, if chosen { 1. } else { 0.75 }))
                    .child(
                        div()
                            .w_full()
                            .text_center()
                            .text_size(px(11.))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(ladder::foreground_alpha(if chosen { 1. } else { 0.6 }))
                            .child(name.clone()),
                    )
                    .id(ElementId::Name(format!("{id}:{index}").into()))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(move |_, window, cx| {
                        if open {
                            on_pick(index, window, cx);
                        }
                    })
                    .agent_node(Role::Button, name.to_string())
            };
            let rows = options
                .iter()
                .enumerate()
                .collect::<Vec<_>>()
                .chunks(per_row)
                .map(|row| {
                    div().flex().flex_row().gap(px(2.)).children(
                        row.iter()
                            .map(|(index, (name, curve))| cell(*index, name, curve)),
                    )
                })
                .collect::<Vec<_>>();
            // Opaque: a curve under the popover must not show through it.
            let content = float::popover_card()
                .p(px(6.))
                .gap(px(2.))
                .bg(ladder::apex())
                .children(rows)
                .agent_node(Role::Card, "Curve presets")
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
    fn the_grid_fills_its_rows_evenly() {
        assert_eq!(columns(1), 1);
        assert_eq!(columns(5), 5);
        assert_eq!(columns(7), 4);
        assert_eq!(columns(8), 4);
        assert_eq!(columns(10), 5);
    }

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
