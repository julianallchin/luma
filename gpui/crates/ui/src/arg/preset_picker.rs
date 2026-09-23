//! A preset picker: a chip that shows the current value as a small picture,
//! and a popover grid of named presets, one framed thumbnail each. A value is
//! a curve or a gradient.
//!
//! Stateless like [`super::select::luma_arg_select`], and animated the same
//! way in and out: the caller owns the [`MenuVisibility`], `on_toggle` fires
//! on the trigger and `on_pick` with the picked preset's index, or `None` for
//! the Custom tile. Closing the popover on pick is the caller's move.

use gpui::prelude::*;
use gpui::{canvas, div, px, App, Div, ElementId, SharedString, Window};
use gpui_component::tooltip::Tooltip;
use luma_patterns::Envelope;

use super::envelope::paint_envelope;
use super::gradient::{gradient_fill, Gradient};
use super::select::MenuVisibility;
use crate::node::{Instrument, Role};
use crate::{float, glass, ladder, select, CONTROL_HEIGHT};

/// What the picker calls a value that is none of the presets.
pub const CUSTOM: &str = "Custom";

/// A value the picker can draw.
#[derive(Clone, Debug, PartialEq)]
pub enum Thumb {
    Curve(Envelope),
    Gradient(Gradient),
}

/// Most tiles in one row of the popover grid.
const COLUMNS: usize = 5;
/// A tile's picture, and the tile that holds it and its name.
const THUMB: [f32; 2] = [40., 24.];
const TILE_W: f32 = 58.;
/// The picture on the trigger.
const CHIP_THUMB: [f32; 2] = [28., 14.];

/// How many tiles each row holds: as few rows as [`COLUMNS`] allows, filled
/// evenly, so 7 lay out as 4 + 3 and not 5 + 2.
fn columns(count: usize) -> usize {
    let rows = count.div_ceil(COLUMNS).max(1);
    count.div_ceil(rows).max(1)
}

/// A small framed picture of `value`, `size` wide and high: a curve's line
/// in a faint chart frame, or a gradient's fill.
pub fn thumb(value: &Thumb, size: [f32; 2], bright: bool) -> Div {
    let frame = div()
        .w(px(size[0]))
        .h(px(size[1]))
        .flex_none()
        .flex()
        .rounded(px(3.))
        .border_1()
        .border_color(glass::hairline(if bright { 0.22 } else { 0.12 }));
    match value {
        Thumb::Curve(curve) => {
            let curve = curve.clone();
            let alpha = if bright { 1. } else { 0.7 };
            frame.bg(glass::ink(0.03)).p(px(2.)).child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        // Inset by the stroke, so a line along an edge stays whole.
                        paint_envelope(
                            window,
                            bounds.inset(px(0.5)),
                            &curve,
                            px(1.25),
                            ladder::foreground_alpha(alpha),
                        );
                    },
                )
                .size_full(),
            )
        }
        Thumb::Gradient(gradient) => frame
            .overflow_hidden()
            .children(gradient_fill(gradient, 2.)),
    }
}

/// The trigger's label: the current preset's name, or [`CUSTOM`].
fn label(options: &[(SharedString, Thumb)], current: Option<usize>) -> SharedString {
    current
        .and_then(|at| options.get(at))
        .map_or(CUSTOM.into(), |(name, _)| name.clone())
}

/// The trigger, and while `open`, the grid of `options` under it. `value` is
/// what the trigger draws; `current` is the preset it equals, if any. With
/// `custom`, the grid ends in a Custom tile that shows `value`, chosen when
/// `current` is `None`.
#[allow(clippy::too_many_arguments)]
pub fn luma_preset_picker(
    id: impl Into<SharedString>,
    value: &Thumb,
    current: Option<usize>,
    options: &[(SharedString, Thumb)],
    custom: bool,
    visibility: impl Into<MenuVisibility>,
    on_toggle: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_pick: impl Fn(Option<usize>, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let visibility = visibility.into();
    let open = visibility.is_open();
    let closing = visibility.exit();
    let id = id.into();
    let shown = label(options, current);
    // The name sits in the same ghost stack a select uses, so the chip keeps
    // one width whichever preset it shows.
    let names = options
        .iter()
        .map(|(name, _)| name.to_string())
        .chain([CUSTOM.to_string()])
        .collect();
    let dismiss = on_toggle.clone();
    let trigger = float::chip_plate(crate::Enabled::Yes)
        .px(px(float::PICKER_CHIP_PAD))
        .child(thumb(value, CHIP_THUMB, true))
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
    // A flex row, so the chip keeps its own width in a stretching column.
    div()
        .relative()
        .flex()
        .child(trigger)
        .when(open || closing.is_some_and(|t| t < 1.0), |el| {
            let tile = |pick: Option<usize>, name: SharedString, picture: &Thumb| {
                let on_pick = on_pick.clone();
                let tip = name.clone();
                let chosen = current == pick;
                let key = pick.map_or("custom".to_string(), |at| at.to_string());
                div()
                    .w(px(TILE_W))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(3.))
                    .px(px(4.))
                    .py(px(5.))
                    .rounded(px(crate::radius::ROW))
                    .cursor_pointer()
                    .when(chosen, |tile| {
                        tile.bg(glass::card_selected_bg())
                            .shadow(glass::card_selected_shadows())
                    })
                    .when(!chosen, |tile| {
                        tile.hover(|style| style.bg(glass::glass_hover()))
                    })
                    .child(thumb(picture, THUMB, chosen))
                    .child(
                        div()
                            .w_full()
                            .text_center()
                            .text_size(px(10.5))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(ladder::foreground_alpha(if chosen { 1. } else { 0.55 }))
                            .child(name.clone()),
                    )
                    .id(ElementId::Name(format!("{id}:{key}").into()))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(move |_, window, cx| {
                        if open {
                            on_pick(pick, window, cx);
                        }
                    })
                    .agent_node(Role::Button, name.to_string())
            };
            let mut tiles: Vec<_> = options
                .iter()
                .enumerate()
                .map(|(index, (name, picture))| tile(Some(index), name.clone(), picture))
                .collect();
            if custom {
                tiles.push(tile(None, CUSTOM.into(), value));
            }
            let per_row = columns(tiles.len());
            let mut rows = Vec::new();
            let mut tiles = tiles.into_iter().peekable();
            while tiles.peek().is_some() {
                rows.push(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(2.))
                        .children(tiles.by_ref().take(per_row)),
                );
            }
            // Opaque: a value under the popover must not show through it.
            let content = float::popover_card()
                .p(px(4.))
                .gap(px(2.))
                .bg(ladder::apex())
                .children(rows)
                .agent_node(Role::Card, "Presets")
                .into_any_element();
            let menu_id = format!("{id}:menu");
            el.child(match closing {
                // It leaves the way every menu does: the same spring, reversed.
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
        assert_eq!(columns(9), 5);
        assert_eq!(columns(11), 4);
    }

    #[test]
    fn the_trigger_names_the_current_option_or_custom() {
        let options = [
            (
                "Ramp up".into(),
                Thumb::Curve(Envelope::linear(vec![[0., 0.], [1., 1.]])),
            ),
            (
                "Flat".into(),
                Thumb::Curve(Envelope::linear(vec![[0., 1.], [1., 1.]])),
            ),
        ];
        assert_eq!(label(&options, Some(1)), "Flat");
        assert_eq!(label(&options, None), CUSTOM);
        assert_eq!(label(&options, Some(5)), CUSTOM);
    }
}
