//! GPUI port of `<Select>` / `<Selector>` (src/shared/components/ui/
//! select.tsx, selector.tsx): the closed trigger, and the open menu.
//!
//! The menu is *stateless* here, like every other control in this crate — a
//! caller renders it only while its own state says the select is open, and
//! wires each item's click. That keeps the open/closed decision with whoever
//! already owns the screen's state instead of introducing a second, hidden
//! store inside the design system.

use crate::icons::IconName;
use gpui::*;
use gpui_component::Icon;

use crate::float::RowState;
use crate::ladder;

/// The `size-3` chevron every trigger ends with (`[&_svg]:size-3`). The color
/// differs by call site — `<Select>` dims it with `opacity-50`, `<Dropdown>`
/// inherits the trigger's `text-foreground/90` — so it's a parameter.
pub(crate) fn chevron(color: Hsla) -> Icon {
    Icon::new(IconName::ChevronDown)
        .size(px(12.))
        .text_color(color)
}

/// Rounded chip shared by fixed-width and self-sizing dropdown triggers.
pub(crate) fn trigger_shell() -> Div {
    crate::float::chip_plate(crate::Enabled::Yes).relative()
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

/// The self-sizing geometry every value trigger in the app is built on: an
/// invisible column of every row ("text + gap + chevron") is the only thing in
/// flow, so the trigger is exactly as wide as the widest row, and the visible
/// row is overlaid on it. Width is therefore invariant across label changes —
/// that is the whole point of the pattern.
///
/// The tier supplies `shell` (its own box and type) and `pad` (the inset the
/// rows sit at); this owns only the stacking, so a second tier cannot acquire
/// a second sizing rule. `rows` are the strings that participate in sizing,
/// `visible` the one actually drawn, already cased by the caller — casing is
/// shared by triggers and menu items.
pub(crate) fn ghost_stack(
    shell: Div,
    visible: String,
    rows: Vec<String>,
    pad: f32,
    chevron_color: Hsla,
) -> Div {
    let row = move |text: String| {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(text)
            .child(chevron(chevron_color))
    };
    shell
        .child(
            div()
                .invisible()
                .mx(px(pad))
                .flex()
                .flex_col()
                .children(rows.into_iter().map(row)),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .px(px(pad))
                .flex()
                .items_center()
                .child(row(visible).flex_1()),
        )
}

/// A self-sizing chip with the same casing as its menu items.
pub(crate) fn ghost_trigger(visible: &str, rows: &[&str], chevron_color: Hsla) -> Div {
    ghost_stack(
        trigger_shell(),
        sentence_case(visible),
        rows.iter().map(|row| sentence_case(row)).collect(),
        10.,
        chevron_color,
    )
}

/// A rounded selector sized to its widest option.
pub fn luma_selector(value: &str, options: &[&str]) -> Div {
    ghost_trigger(value, options, ladder::foreground_alpha(0.45))
}

/// `<SelectItem>` on the float tier: one row of an open menu — a
/// [`crate::float::menu_row`] carrying the label, and a check on the chosen
/// row (a fixed-width hole otherwise, so a label does not shift when the
/// selection moves to it). Float menu rows are sentence case, and the row owns
/// its casing together with [`ghost_trigger`]: wire
/// spellings arrive raw ("replace") and are display-cased here, so no caller
/// keeps a parallel display list. Rows that are *code* (the expression
/// suggestions) use [`crate::float::menu_row`] directly and stay lowercase.
///
/// The label doubles as the row's hover fade key, which is unique enough
/// because the strip and the settings screen open one menu at a time; a
/// consumer that floats two menus with identical rows at once supplies its
/// own keys via [`crate::float::menu_row`] directly.
///
/// `state` is [`RowState::of`] over the caller's two facts; a menu without
/// keyboard navigation passes `cursor: false`.
pub fn luma_select_item(label: &str, state: RowState) -> Div {
    let selected = state == RowState::Selected;
    crate::float::menu_row(state, label.to_string())
        .justify_between()
        .child(sentence_case(label))
        .child(crate::float::check(selected))
}

/// First letter up, the rest untouched — idempotent on labels that already
/// arrive cased ("Multiply"). The float tier's casing, shared with the
/// trigger ([`crate::float::picker_chip`]) so a row and the value it becomes
/// are spelled the same.
pub(crate) fn sentence_case(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
