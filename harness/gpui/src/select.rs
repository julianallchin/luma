//! GPUI port of the closed `<Select>` trigger and of `<Selector>`
//! (src/shared/components/ui/select.tsx, selector.tsx).
//!
//! Only the trigger is ported: the harness captures the closed state, so the
//! portalled menu is out of scope here.

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
