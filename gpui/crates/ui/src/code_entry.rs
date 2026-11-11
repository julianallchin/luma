//! A short code typed one symbol to a place: an invite code, the code mailed
//! at sign-in.
//!
//! The places are the field. A [`TextInput`] sits with them unseen, taking
//! the keys and the paste, and the lit place carries the caret — so how much
//! is left, and which symbol cannot be right, are both on screen, and the
//! value is in one place only.

use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::text_input::{self, TextInput};
use crate::{glass, ladder, radius};

/// The row of places for a code `len` symbols long.
///
/// `typed` is what has been entered so far, each symbol with whether it is
/// one no code can hold. `split` puts a dash before that place, for a code
/// read in groups. `input` is the field and its focus, or `None` for a copy
/// that only paints. `accepting` is false while the code is being spent, when
/// there is no next place to type into.
///
/// The caller names the row for drivers (`agent_node`); a press anywhere on
/// it gives the field the keyboard.
pub fn places(
    typed: &[(char, bool)],
    len: usize,
    split: Option<usize>,
    input: Option<(&Entity<TextInput>, &FocusHandle)>,
    accepting: bool,
    window: &Window,
) -> Div {
    let focused = input.is_some_and(|(_, focus)| focus.is_focused(window));
    let place = |index: usize| {
        let symbol = typed.get(index);
        let next = accepting && index == typed.len();
        div()
            .w(px(44.0))
            .h(px(54.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(radius::ROW))
            .bg(glass::ink(0.05))
            .border_1()
            .border_color(glass::hairline(if next { 0.4 } else { 0.08 }))
            .text_size(px(24.0))
            .font_weight(FontWeight::MEDIUM)
            .when(symbol.is_some_and(|(_, wrong)| *wrong), |place| {
                place.text_color(ladder::danger())
            })
            .children(symbol.map(|(symbol, _)| symbol.to_string()))
            // The caret, in the place the next symbol lands, on the text
            // input's own beat.
            .when(next && focused, |place| {
                place.child(
                    div()
                        .w(px(2.0))
                        .h(px(26.0))
                        .rounded_full()
                        .bg(ladder::foreground())
                        .with_animation(
                            "code-caret",
                            Animation::new(Duration::from_millis(2 * text_input::CARET_BLINK_MS))
                                .repeat(),
                            |caret, phase| caret.opacity(if phase < 0.5 { 1.0 } else { 0.0 }),
                        ),
                )
            })
    };
    let mut row = div().relative().flex().items_center().gap(px(8.0));
    for index in 0..len {
        if split == Some(index) {
            row = row.child(
                div()
                    .w(px(10.0))
                    .h(px(2.0))
                    .rounded_full()
                    .bg(glass::hairline(0.25)),
            );
        }
        row = row.child(place(index));
    }
    let Some((field, focus)) = input else {
        return row;
    };
    let focus = focus.clone();
    // Clipped to nothing rather than made transparent: an element at zero
    // opacity is not painted, and an input that is not painted takes no keys.
    // Pushed clear of the clip, so not even its own caret shows through.
    row.child(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size(px(1.0))
            .overflow_hidden()
            .child(div().pl(px(8.0)).pt(px(8.0)).child(field.clone())),
    )
    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
        window.focus(&focus, cx);
        cx.stop_propagation();
    })
}
