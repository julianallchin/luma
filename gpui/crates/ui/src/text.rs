//! The two text shapes every screen writes: a quiet caption, and the plate a
//! screen shows when it has nothing else to show.
//!
//! Neither is screen-local. A restyled label or a restyled empty state that
//! only landed on four screens out of five would be two design systems, which
//! is the thing this crate exists to prevent.

use crate::rpx;
use gpui::prelude::*;
use gpui::{div, AnyElement, Hsla, SharedString};

use crate::float;
use crate::node::{Instrument, Role};

/// A quiet sentence-case readout: [`float::label`], published as a text node.
///
/// The caller passes the text in the case it is shown in, and the same words
/// are the automation label, because a caption is read, never pressed.
pub fn caption(label: impl Into<SharedString>) -> impl IntoElement {
    let label = label.into();
    float::label(label.clone()).agent_node(Role::Text, label)
}

/// The whole body when there is nothing to list: one centred line that says
/// so, named so a script can read the reason instead of inferring it from an
/// empty node list.
///
/// `color` is how much the reason weighs — [`ladder::muted_foreground`] for
/// "loading" or "nothing here", [`ladder::danger`] for a failure — and is the
/// only thing that differs between screens.
pub fn plate(message: impl Into<String>, color: impl Into<Hsla>) -> AnyElement {
    let message = message.into();
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(rpx(12.))
        .text_color(color.into())
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
}
