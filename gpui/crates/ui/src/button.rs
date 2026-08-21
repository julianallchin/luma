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

/// Whether a control will accept input.
///
/// A word rather than a `bool` because the flag reads backwards from every
/// other control's state bit in this crate — `pressed`, `checked`, `selected`
/// are all "on", `disabled` is "off" — so a bare `luma_button("Back", false)`
/// gave a reader nothing to bind the `false` to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enabled {
    /// Control fill, hover lifts to `--hover`.
    Yes,
    /// Dimmed to [`ladder::DISABLED_OPACITY`], inert under the pointer.
    No,
}

/// `<Button>`: control fill, `text-foreground/90`, hover lifts to `--hover`.
pub fn luma_button(label: &str, enabled: Enabled) -> Div {
    slab()
        .bg(ladder::control())
        .text_color(ladder::foreground_90())
        .map(|el| match enabled {
            Enabled::Yes => el.hover(|s| s.bg(ladder::hover()).text_color(ladder::foreground())),
            Enabled::No => el.opacity(ladder::DISABLED_OPACITY),
        })
        .child(label.to_uppercase())
}
