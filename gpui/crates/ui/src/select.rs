//! GPUI port of `<Select>` / `<Selector>` (src/shared/components/ui/
//! select.tsx, selector.tsx): the closed trigger, and the open menu.
//!
//! The menu is *stateless* here, like every other control in this crate — a
//! caller renders it only while its own state says the select is open, and
//! wires each item's click. That keeps the open/closed decision with whoever
//! already owns the screen's state instead of introducing a second, hidden
//! store inside the design system.

use gpui::*;
use gpui_component::{Icon, IconName};

use crate::ladder;

/// The `size-3` chevron every trigger ends with (`[&_svg]:size-3`). The color
/// differs by call site — `<Select>` dims it with `opacity-50`, `<Dropdown>`
/// inherits the trigger's `text-foreground/90` — so it's a parameter.
pub(crate) fn chevron(color: Hsla) -> Icon {
    Icon::new(IconName::ChevronDown)
        .size(px(12.))
        .text_color(color)
}

/// The shared trigger shell: `h-6 border px-2` on control fill, square, no
/// focus ring. Padding is expressed as a margin on the content instead of
/// padding on the shell so that absolutely-positioned children (the ghost
/// stack overlay in `luma_selector`) share one unambiguous box.
pub(crate) fn trigger_shell() -> Div {
    div()
        .relative()
        .flex()
        .items_center()
        .flex_shrink_0()
        .h(px(24.))
        .border_1()
        .border_color(ladder::control_border())
        .bg(ladder::control())
        .text_color(ladder::foreground_90())
        .hover(|s| s.bg(ladder::hover()).text_color(ladder::foreground()))
}

/// Raw `<Select>` trigger: an explicit width (`w-40`, `w-28`, …) and the
/// primitive's `text-xs` data font — no uppercase, no bold.
pub fn luma_select(value: &str, width: f32) -> Div {
    trigger_shell().w(px(width)).text_size(px(12.)).child(
        div()
            .mx(px(8.))
            .flex_1()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(value.to_string())
            .child(chevron(ladder::foreground_alpha(0.45))),
    )
}

/// The self-sizing geometry both `<Selector>` and `<Dropdown>` are built on:
/// an invisible column of every row ("text + gap-2 + chevron") is the only
/// thing in flow, so the trigger is exactly as wide as the widest row, and
/// the visible row is overlaid on it. Width is therefore invariant across
/// label changes — that is the whole point of the pattern.
///
/// `rows` are the strings that participate in sizing; `visible` is the one
/// that's actually drawn. Both are uppercased here, as `BUTTON_CLASS` does.
pub(crate) fn ghost_trigger(visible: &str, rows: &[&str], chevron_color: Hsla) -> Div {
    let row = |text: String| {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(text)
            .child(chevron(chevron_color))
    };
    trigger_shell()
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .child(
            div()
                .invisible()
                .mx(px(8.))
                .flex()
                .flex_col()
                .children(rows.iter().map(|r| row(r.to_uppercase()))),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .px(px(8.))
                .flex()
                .items_center()
                .child(row(visible.to_uppercase()).flex_1()),
        )
}

/// `<Selector>`: the brutalist control font over the ghost stack, sized to the
/// widest option.
pub fn luma_selector(value: &str, options: &[&str]) -> Div {
    ghost_trigger(value, options, ladder::foreground_alpha(0.45))
}

/// `<SelectContent>`: the open menu. Control fill on a control border, square,
/// no shadow and no animation, hung one pixel *under* the trigger's bottom
/// border so the two boxes share one hairline (the web side's `sideOffset:
/// -1`).
///
/// Positioned absolutely, so the caller wraps trigger and menu in one
/// `div().relative()`, and `deferred`s it to paint above later siblings.
pub fn luma_select_menu() -> Div {
    div()
        .absolute()
        .top(px(23.))
        .left_0()
        .flex()
        .flex_col()
        .border_1()
        .border_color(ladder::control_border())
        .bg(ladder::control())
}

/// `<SelectItem>`: one row of an open menu — `h-[22px]`, the control font,
/// `--hover` under the pointer, and a check on the chosen row.
///
/// The web side absolutely-positions that check at `right-2` and reserves room
/// for it with `pr-8`; here the row is a `justify-between` flex and the check
/// occupies a real 12px slot, so the padding is `px-2` on both sides. Same ink,
/// one less way to be wrong about the gap.
pub fn luma_select_item(label: &str, selected: bool) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .h(px(22.))
        .pl(px(8.))
        .pr(px(8.))
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(ladder::foreground_90())
        .hover(|s| s.bg(ladder::hover()).text_color(ladder::foreground()))
        .child(label.to_uppercase())
        .child(if selected {
            Icon::new(IconName::Check).size(px(12.)).into_any_element()
        } else {
            // A fixed-width hole, so a row's label does not shift when the
            // selection moves to it.
            div().size(px(12.)).into_any_element()
        })
}
