//! The select menu row and the self-sizing trigger geometry.
//!
//! The menu is *stateless* here, like every other control in this crate — a
//! caller renders it only while its own state says the select is open, and
//! wires each item's click. That keeps the open/closed decision with whoever
//! already owns the screen's state instead of introducing a second, hidden
//! store inside the design system.

use crate::icons::IconName;
use crate::rpx;
use gpui::*;
use gpui_component::Icon;

use crate::float::RowState;

/// The 12px chevron a trigger ends with.
pub(crate) fn chevron(color: Hsla) -> Icon {
    Icon::new(IconName::ChevronDown)
        .size(rpx(12.))
        .text_color(color)
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
            .gap(rpx(8.))
            .child(text)
            .child(chevron(chevron_color))
    };
    shell
        .child(
            div()
                .invisible()
                .mx(rpx(pad))
                .flex()
                .flex_col()
                .children(rows.into_iter().map(row)),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .px(rpx(pad))
                .flex()
                .items_center()
                .child(row(visible).flex_1()),
        )
}

/// One row of an open select menu — a
/// [`crate::float::menu_row`] carrying the label, and a check on the chosen
/// row (a fixed-width hole otherwise, so a label does not shift when the
/// selection moves to it). Float menu rows are sentence case, and the row owns
/// its casing together with [`crate::float::picker_chip`]: wire
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
