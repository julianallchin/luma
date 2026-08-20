//! GPUI port of `BUTTON_CLASS` (src/shared/components/ui/button.tsx) — the one
//! slab geometry every pressable control in the app composes against.
//!
//! `<Toggle>` reuses the exact same box and only swaps the fill/text pair, so
//! the geometry lives here once and both call sites style on top of it.
//!
//! Known gap: letter-spacing (`tracking-wider`) has no gpui styled equivalent
//! yet, so every label here is ~0.05em/glyph narrower than the web side.

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::ladder;

/// `inline-flex items-center justify-center gap-1 shrink-0 h-6 px-2 border
/// rounded-none` + `text-[9px] uppercase font-bold`. Colors are the caller's.
pub fn slab() -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .flex_shrink_0()
        .h(px(24.))
        .px(px(8.))
        .border_1()
        .border_color(ladder::control_border())
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
}

/// `<Button>`: control fill, `text-foreground/90`, hover lifts to `--hover`.
pub fn luma_button(label: &str, disabled: bool) -> Div {
    slab()
        .bg(ladder::control())
        .text_color(ladder::foreground_90())
        .when(disabled, |el| el.opacity(0.5))
        .when(!disabled, |el| {
            el.hover(|s| s.bg(ladder::hover()).text_color(ladder::foreground()))
        })
        .child(label.to_uppercase())
}
