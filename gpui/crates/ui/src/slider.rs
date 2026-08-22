//! GPUI port of `<Slider>` (src/shared/components/ui/slider.tsx).
//!
//! Not a thumb-on-a-track slider: the app's is an Ableton-style value box —
//! a recessed `--input` slab with a `--primary` fill bar at `opacity-20`
//! covering value% of the *content* box, and the numeric value drawn over it
//! in 10px mono. The web version's range `<input>` is invisible, so the
//! captured frame is exactly these three layers.

use gpui::*;

use crate::ladder;

pub fn luma_slider(value: f32, min: f32, max: f32, width: f32) -> Div {
    let fraction = ((value - min) / (max - min)).clamp(0., 1.);
    div()
        .relative()
        .overflow_hidden()
        .flex_shrink_0()
        .w(px(width))
        .h(px(28.))
        .border_1()
        .border_color(ladder::control_border())
        .bg(ladder::apex())
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .w(relative(fraction))
                .bg(ladder::primary())
                .opacity(0.2),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .px(px(8.))
                .flex()
                .items_center()
                // Tailwind's `font-mono` stack resolves to `ui-monospace` on
                // WebKit/macOS, i.e. SF Mono — naming it explicitly makes the
                // two stacks shape the same glyphs (Menlo is ~0.5pt wider).
                .font_family("SF Mono")
                .text_size(px(10.))
                .text_color(ladder::primary())
                .child(format!("{value}")),
        )
}
