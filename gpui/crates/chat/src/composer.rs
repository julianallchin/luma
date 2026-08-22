//! What the person types, and the two buttons that go with it.
//!
//! # Why it declares `TextInput`
//!
//! gpui matches key bindings *before* it delivers key events, so a focused
//! field cannot out-run a binding by consuming the keystroke. Every binding on
//! a key a person could be typing therefore carries `&& !TextInput`, and this
//! element declares that context — without it every space typed here would
//! toggle the transport and every escape would leave the screen.
//!
//! The consequence is that `enter` and `escape` are handled here, in a capture
//! handler, rather than as bindings: a binding for them would be a second rule
//! about the same keys, and the two would drift. `shift-enter` is deliberately
//! *not* handled: it falls through to the field, which inserts a newline.
//!
//! # How tall it gets
//!
//! The field grows with its content and then scrolls inside itself, which is
//! [`TextareaState::auto_grow`]'s own behaviour — the plate does not clamp it.
//! A `max_h` on the wrapper would clip instead of scroll, which is the same
//! shape of bug as measuring a fold: the container decides a height the content
//! has no way to honour. The range is stated in pixels by the theme and
//! converted to rows here, in [`ROWS_MIN`] / [`ROWS_MAX`], so there is one
//! definition of how tall the composer may be.
//!
//! # Why it is never disabled
//!
//! A turn in flight does not lock the field: what the person types during one
//! *steers* it (`TurnStream::steer`), applied at the next row boundary. So the
//! one button reads Send, Steer or Stop — see [`send`] — and there is no state
//! in which the composer is inert while a conversation is happening.

use gpui::{div, prelude::*, px, Entity, Focusable as _, KeyDownEvent, SharedString, Window};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{Icon, IconName};
use luma_ui::node::{AgentNode as _, Instrument, Role as NodeRole};

use crate::theme::{self, Theme};
use crate::AgentChat;

/// Placeholder, and the composer's only copy. Public because it is also the
/// composer's *name* in the automation tree — a driver has to find this field
/// among a screen's own inputs, and the label is what tells them apart.
pub const PLACEHOLDER: &str = "Ask about this pattern…";

/// The composer plate: the field, and send or stop.
pub fn composer(
    chat: &Entity<AgentChat>,
    state: &Entity<TextareaState>,
    streaming: bool,
    model: Option<&str>,
    theme: &Theme,
    window: &Window,
    cx: &mut gpui::App,
) -> impl IntoElement {
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    let value = state.read(cx).value().to_string();
    let keyed = chat.clone();
    // The plate sits on the same reading column as the transcript above it —
    // a full-bleed composer under a centered column is two column rules on
    // one pane.
    div()
        .flex()
        .flex_col()
        .items_center()
        // The transcript's own gutters, so the plate sits on the same reading
        // column as the prose above it — one column rule per pane.
        .px(px(theme::CONTENT_GUTTER))
        .py(px(theme::SPACE_MD))
        .child(
            div()
                .key_context(luma_ui::TEXT_INPUT)
                .w_full()
                .max_w(px(theme::MAX_CONTENT_WIDTH))
                .flex()
                .flex_col()
                .rounded(px(theme::COMPOSER_RADIUS))
                .bg(theme.input_bg)
                .border_1()
                .border_color(if focused {
                    theme.border_strong
                } else {
                    theme.border
                })
                // Capture, not bubble: the editor binds `enter` itself, and a
                // bubble handler would run after it had already inserted a
                // newline.
                .capture_key_down(move |event: &KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    let modified = event.keystroke.modifiers.shift;
                    match key {
                        "enter" if !modified => {
                            cx.stop_propagation();
                            keyed.update(cx, |this, cx| this.send(window, cx));
                        }
                        "escape" => {
                            cx.stop_propagation();
                            keyed.update(cx, |this, cx| this.escape(cx));
                        }
                        _ => {}
                    }
                })
                .child(
                    div()
                        .px(px(theme::SPACE_LG))
                        .pt(px(theme::SPACE_MD))
                        .text_size(px(14.0))
                        .line_height(px(theme::INPUT_LINE_HEIGHT))
                        .text_color(theme.text)
                        .child(Textarea::new(state).appearance(false).bordered(false))
                        .agent_node(
                            NodeRole::Input,
                            if value.is_empty() {
                                SharedString::from(PLACEHOLDER)
                            } else {
                                SharedString::from(value.clone())
                            },
                        )
                        .agent_focused(focused),
                )
                .child(actions(
                    chat,
                    streaming,
                    value.trim().is_empty(),
                    model,
                    theme,
                )),
        )
}

/// What the plate carries under the field: everything clustered on the right,
/// in comet's order — what will answer, then the one button that starts or
/// stops it.
fn actions(
    chat: &Entity<AgentChat>,
    streaming: bool,
    empty: bool,
    model: Option<&str>,
    theme: &Theme,
) -> impl IntoElement {
    let action = Action::of(streaming, !empty);
    div()
        .h(px(theme::ACTIONS_ROW_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_SM))
        .px(px(theme::SPACE_LG))
        .child(div().flex_1())
        .children(model.map(|model| model_chip(model, theme)))
        .child(send(chat, action, theme))
}

/// Which model answers. A readout, not a control — the model is chosen in
/// settings, and a second place to change it would be a second rule about it.
fn model_chip(model: &str, theme: &Theme) -> impl IntoElement {
    let label = SharedString::from(model.to_string());
    div()
        .h(px(theme::CHIP_SMALL_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_XS + 2.0))
        .px(px(theme::SPACE_SM))
        .rounded(px(theme::CONTROL_RADIUS))
        .bg(theme::card_bg())
        .border_1()
        .border_color(theme.border)
        .child(
            Icon::new(IconName::Bot)
                .size(px(12.0))
                .text_color(theme.text_faint),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(label.clone()),
        )
        .agent_node(NodeRole::Text, label)
}

/// What the one button does, which is a function of two facts and nothing
/// else: whether a turn is running, and whether anything is typed.
///
/// One button in three tenses rather than three buttons, because a turn is
/// either running or it is not — a pair that could disagree about that is a
/// state the panel must not be able to reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to send and no turn to stop. The only inert state the button
    /// has — notably *not* "a turn is running".
    Inert,
    /// No turn running, and something to say: start one.
    Send,
    /// A turn is running and there is text: redirect it at the next row
    /// boundary rather than queueing a second turn behind it.
    Steer,
    /// A turn is running and there is nothing to say: end it.
    Stop,
}

impl Action {
    /// The button's whole state, from the only two facts it depends on.
    ///
    /// Every combination names one action, including the inert one — so the
    /// button carries no second "enabled" flag that could contradict it, and
    /// there is no "disabled while streaming" case left to get wrong.
    #[must_use]
    pub fn of(streaming: bool, has_text: bool) -> Self {
        match (streaming, has_text) {
            (false, false) => Action::Inert,
            (false, true) => Action::Send,
            (true, true) => Action::Steer,
            (true, false) => Action::Stop,
        }
    }

    /// What the automation tree calls it. [`Action::Inert`] is still the send
    /// button — it reports so, and reports itself disabled.
    fn label(self) -> &'static str {
        match self {
            Action::Inert | Action::Send => "Send",
            Action::Steer => "Steer",
            Action::Stop => "Stop",
        }
    }
}

/// Send, steer, or stop.
///
/// One circular slug in every tense — an arrow to send or steer, a square to
/// stop — because the eye tracks a shape that stays put and swaps its glyph,
/// and a word that changes width does not.
fn send(chat: &Entity<AgentChat>, action: Action, theme: &Theme) -> impl IntoElement {
    let pressed = chat.clone();
    let enabled = action != Action::Inert;
    div()
        .id("chat-send")
        .size(px(theme::SEND_DIAMETER))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        // A filled light disc in every state, dimmed only in its *fill*.
        // Fading the whole slug composited the dark glyph away too, and on
        // these near-black grounds the result was a grey disc on grey — the
        // primary action reading as the least visible thing on the panel.
        .bg(if enabled {
            theme.text
        } else {
            theme.text.opacity(0.72)
        })
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|style| style.opacity(0.85))
                .on_click(move |_, window, cx| {
                    pressed.update(cx, |this, cx| match action {
                        Action::Stop => this.cancel(cx),
                        Action::Send | Action::Steer => this.send(window, cx),
                        // Unreachable: an inert button has no click handler.
                        Action::Inert => {}
                    });
                })
        })
        .child(if action == Action::Stop {
            div()
                .size(px(10.0))
                .rounded(px(2.0))
                .bg(theme.knockout)
                .into_any_element()
        } else {
            Icon::new(IconName::ArrowUp)
                .size(px(15.0))
                .text_color(theme.knockout)
                .into_any_element()
        })
        .agent_node(NodeRole::Button, action.label())
        .agent_disabled(!enabled)
}

/// The grow range in rows, from the plate's pixel range. Truncating is the
/// right rounding in both directions: a partial row at the floor would show a
/// clipped line, and one at the ceiling would overflow the plate.
const ROWS_MIN: usize =
    ((theme::TEXTAREA_MIN - theme::TEXTAREA_PAD_V) / theme::INPUT_LINE_HEIGHT) as usize;
const ROWS_MAX: usize =
    ((theme::TEXTAREA_MAX - theme::TEXTAREA_PAD_V) / theme::INPUT_LINE_HEIGHT) as usize;

/// A fresh composer: multi-line, growing with its content between the two
/// heights the plate is designed around, and scrolling inside itself past the
/// second.
pub fn state(window: &mut Window, cx: &mut gpui::App) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(PLACEHOLDER)
            .auto_grow(ROWS_MIN, ROWS_MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The button's four states, and the one that is inert. A turn running is
    /// never inert — that was the bug: the composer went dead mid-turn.
    #[test]
    fn only_an_idle_empty_composer_is_inert() {
        assert_eq!(Action::of(false, false), Action::Inert);
        assert_eq!(Action::of(false, true), Action::Send);
        assert_eq!(Action::of(true, true), Action::Steer);
        assert_eq!(Action::of(true, false), Action::Stop);
    }

    /// The two ranges are one range. If the theme's pixels move, the rows move
    /// with them — and a floor above the ceiling would be a plate that cannot
    /// hold its own minimum.
    #[test]
    fn the_grow_range_is_at_least_one_row_and_ordered() {
        assert!(ROWS_MIN >= 1, "the field must show a line when empty");
        assert!(ROWS_MIN < ROWS_MAX, "{ROWS_MIN}..{ROWS_MAX} does not grow");
        assert!(
            ROWS_MAX as f32 * theme::INPUT_LINE_HEIGHT + theme::TEXTAREA_PAD_V
                <= theme::TEXTAREA_MAX,
            "the grown field overflows the plate"
        );
    }
}
