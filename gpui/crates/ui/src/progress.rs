//! A dialog's progress bar: a rounded track and how much of it is done.

use gpui::prelude::*;
use gpui::{div, px, relative, Div};

use crate::{glass, ladder};

/// `fraction` of the way along, clamped to `0..=1`, at the full width of
/// whatever holds it.
pub fn bar(fraction: f32) -> Div {
    div()
        .w_full()
        .h(px(6.))
        .rounded(px(3.))
        .bg(glass::ink(0.08))
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0.0, 1.0)))
                .rounded(px(3.))
                .bg(ladder::foreground()),
        )
}
