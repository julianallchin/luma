//! GPUI port of `<Input>` (src/shared/components/ui/input.tsx).
//!
//! `h-6 px-2 border rounded-none` on control fill / control border, `text-xs`
//! data font (no uppercase, no bold — inputs hold values, not labels), and no
//! focus ring. Only the resting, unfocused state is ported: the harness
//! captures a static frame, so there is no caret, selection, or editing
//! behaviour here — see the note in fixtures.rs.

use gpui::*;

use crate::ladder;

/// The shell every input-shaped control shares. `placeholder` renders the
/// empty state in `--muted-foreground`; `value` renders it in `--foreground`.
pub fn luma_input(text: &str, placeholder: bool, width: f32) -> Div {
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .w(px(width))
        .h(px(24.))
        .px(px(8.))
        .border_1()
        .border_color(ladder::control_border())
        .bg(ladder::control())
        .text_size(px(12.))
        .text_color(if placeholder {
            ladder::muted_foreground()
        } else {
            ladder::foreground()
        })
        .child(text.to_string())
}
