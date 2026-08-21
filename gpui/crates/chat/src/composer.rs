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
//! about the same keys, and the two would drift.

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
    div().flex().flex_col().p(px(theme::SPACE_MD)).child(
        div()
            .key_context(luma_ui::TEXT_INPUT)
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
                    .min_h(px(theme::TEXTAREA_MIN))
                    .max_h(px(theme::TEXTAREA_MAX))
                    .text_size(px(14.0))
                    .line_height(px(22.75))
                    .text_color(theme.text)
                    .child(
                        Textarea::new(state)
                            .appearance(false)
                            .bordered(false)
                            .disabled(streaming),
                    )
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

/// What the plate carries under the field: what will answer, on the left, and
/// the one button that starts or stops it, on the right.
fn actions(
    chat: &Entity<AgentChat>,
    streaming: bool,
    empty: bool,
    model: Option<&str>,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .h(px(theme::ACTIONS_ROW_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        // The field's own inset, so the chip's left edge and the first
        // character of a prompt sit on one line.
        .px(px(theme::SPACE_LG))
        .child(match model {
            Some(model) => model_chip(model, theme).into_any_element(),
            // Not yet resolved. The row's height is declared, so the chip
            // arriving late moves nothing.
            None => div().into_any_element(),
        })
        .child(send(chat, streaming, empty, theme))
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
        .bg(theme::ink(0.04))
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

/// Send, or stop. Never both: a turn is either running or it is not, and two
/// buttons that could disagree about that is a state the panel cannot have.
///
/// One circular slug in both tenses — an arrow to send, a square to stop —
/// because the eye tracks a shape that stays put and swaps its glyph, and a
/// word that changes width does not.
fn send(chat: &Entity<AgentChat>, streaming: bool, empty: bool, theme: &Theme) -> impl IntoElement {
    let pressed = chat.clone();
    let (label, enabled) = if streaming {
        ("Stop", true)
    } else {
        ("Send", !empty)
    };
    div()
        .id("chat-send")
        .size(px(theme::SEND_DIAMETER))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme.text)
        // Dimmed rather than hidden: an empty composer still shows where the
        // button is, which is the whole of the affordance.
        .when(!enabled, |el| el.opacity(0.3))
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|style| style.opacity(0.85))
                .on_click(move |_, window, cx| {
                    pressed.update(cx, |this, cx| {
                        if streaming {
                            this.cancel(cx);
                        } else {
                            this.send(window, cx);
                        }
                    });
                })
        })
        .child(if streaming {
            div()
                .size(px(10.0))
                .rounded(px(2.0))
                .bg(theme.bg)
                .into_any_element()
        } else {
            Icon::new(IconName::ArrowUp)
                .size(px(15.0))
                .text_color(theme.bg)
                .into_any_element()
        })
        .agent_node(NodeRole::Button, label)
        .agent_disabled(!enabled)
}

/// A fresh composer: multi-line, growing between the two heights the plate is
/// designed around.
pub fn state(window: &mut Window, cx: &mut gpui::App) -> Entity<TextareaState> {
    cx.new(|cx| TextareaState::new(window, cx).placeholder(PLACEHOLDER))
}
