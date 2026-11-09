//! The one way the app says something did not work: a quiet line in plain
//! words behind a small mark.
//!
//! Failures are rare — most are bugs — so they do not get a red block inside
//! the panel they happened in. An action that did not land is one line in the
//! status bar (the app's `failure` module); the few things a person has to
//! fix where they are typing, a wrong sign-in code or a name already taken,
//! are this same line under the field. The words are the caller's, written
//! for the person: what did not happen, never a command name or a server's
//! error text, which go to the log.

use gpui::prelude::*;
use gpui::{div, Div, SharedString};

use crate::glass;
use crate::ladder;
use crate::node::{Instrument as _, Role};
use crate::rpx;

/// The mark ahead of a notice: a small dot in the attention hue, so the line
/// reads as "worth a look" without the words themselves turning red.
pub fn mark() -> Div {
    div()
        .flex_none()
        .size(rpx(6.))
        .rounded_full()
        .bg(ladder::status_warn())
}

/// A notice: [`mark`], then `text` in the secondary ink. Published as a text
/// node with the same words, so a script reads what a person reads.
pub fn line(text: impl Into<SharedString>) -> impl IntoElement {
    let text = text.into();
    div()
        .flex()
        .items_center()
        .gap(rpx(6.))
        .text_size(rpx(12.))
        .text_color(glass::ink(glass::TEXT_SECONDARY))
        .child(mark())
        .child(div().min_w_0().child(text.clone()))
        .agent_node(Role::Text, text)
}
