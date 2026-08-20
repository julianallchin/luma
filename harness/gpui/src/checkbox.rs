//! GPUI port of `<Checkbox>` (src/shared/components/ui/checkbox.tsx).
//!
//! The luma checkbox has no check glyph: it is a 12px square whose *border*
//! carries the state — a 1px `--border` hairline when unchecked, and a 4px
//! `--primary` ring when checked. Because the box is border-box sized, the
//! outer 12px is invariant and only the surviving `--input` core shrinks from
//! 10px to 4px. That is the whole component.

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::ladder;

pub fn luma_checkbox(checked: bool) -> Div {
    div()
        .flex_shrink_0()
        .size(px(12.))
        .bg(ladder::input())
        .when(checked, |el| {
            el.border(px(4.)).border_color(ladder::primary())
        })
        .when(!checked, |el| el.border_1().border_color(ladder::border()))
}
