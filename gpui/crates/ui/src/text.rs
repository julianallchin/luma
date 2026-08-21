//! The two text shapes every screen writes: the panel's silkscreen label, and
//! the plate a screen shows when it has nothing else to show.
//!
//! Neither is screen-local. A restyled label or a restyled empty state that
//! only landed on four screens out of five would be two design systems, which
//! is the thing this crate exists to prevent.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, FontWeight, Hsla};

use crate::ladder;
use crate::node::{Instrument, Role};

/// 9px uppercase silkscreen, the panel's one label style.
///
/// The caller passes the text already cased, and it doubles as the automation
/// node's label — a silkscreen is read, never pressed, so the words on it are
/// the whole of its identity.
pub fn silkscreen(label: impl Into<String>) -> impl IntoElement {
    let label = label.into();
    div()
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(ladder::muted_foreground())
        .child(label.clone())
        .agent_node(Role::Text, label)
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
        .text_size(px(12.))
        .text_color(color.into())
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
}
