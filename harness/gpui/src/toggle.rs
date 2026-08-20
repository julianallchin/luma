//! GPUI port of `<Toggle>` / `<ToggleGroup>` (src/shared/components/ui/
//! toggle.tsx, toggle-group.tsx).
//!
//! A toggle is a `<Button>` slab that inverts when pressed: fill becomes
//! `--foreground` and the label becomes `--background`. Unpressed is byte-for-
//! byte the button's resting look, which is why both share `button::slab()`.

use gpui::*;

use crate::{button, ladder};

pub fn luma_toggle(label: &str, pressed: bool) -> Div {
    let slab = button::slab();
    let slab = if pressed {
        // `bg-foreground border-control-border text-background`
        slab.bg(ladder::foreground())
            .text_color(ladder::background())
    } else {
        // `bg-control border-control-border text-foreground/90`
        slab.bg(ladder::control())
            .text_color(ladder::foreground_90())
            .hover(|s| s.bg(ladder::hover()).text_color(ladder::foreground()))
    };
    slab.child(label.to_uppercase())
}

/// `<ToggleGroup>`: segments share one border line between them.
///
/// The web side does that with `-ml-px` (each segment after the first slides
/// 1px left so its border lands on its neighbour's). Taffy resolves a negative
/// margin on a flex item into the *container's* position, which knocks the
/// whole group out of the centered layout, so the port drops the left border
/// instead: same total width (`n·W − (n−1)`), same label positions, same single
/// hairline — and every border in the group is `--control-border`, so which
/// element owns the shared line is invisible (that is also why the web side's
/// `z-10` on the pressed segment has no visual counterpart here).
pub fn luma_toggle_group(value: &str, options: &[&str]) -> Div {
    div()
        .flex()
        .children(options.iter().enumerate().map(|(i, opt)| {
            let seg = luma_toggle(opt, *opt == value);
            if i > 0 {
                seg.border_l(px(0.))
            } else {
                seg
            }
        }))
}
