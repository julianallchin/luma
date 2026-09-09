//! Shared action buttons use the Comet-style chip on every app surface.
//! Keep labels in their supplied case; do not recreate the old square,
//! bordered, uppercase button style.

use crate::{float, ladder};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// Whether a control will accept input.
///
/// A word rather than a `bool` because the flag reads backwards from every
/// other control's state bit in this crate — `pressed`, `checked`, `selected`
/// are all "on", `disabled` is "off" — so a bare `button("Back", false)`
/// gave a reader nothing to bind the `false` to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enabled {
    /// Rounded chip with a subtle hover wash.
    Yes,
    /// Dimmed to [`ladder::DISABLED_OPACITY`], inert under the pointer.
    No,
}

impl From<bool> for Enabled {
    /// `true` is [`Enabled::Yes`]. Call sites almost always hold a predicate
    /// rather than the enum, and the two-armed `if` they were writing to bridge
    /// the gap said nothing the type did not already.
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Yes
        } else {
            Self::No
        }
    }
}

/// A compact rounded action button. The caller adds its id and handler.
pub fn button(label: &str, enabled: Enabled) -> Div {
    float::chip_plate(enabled)
        .justify_center()
        .px(px(float::PICKER_CHIP_PAD))
        .map(|el| match enabled {
            Enabled::Yes => el.tab_index(0),
            Enabled::No => el.opacity(ladder::DISABLED_OPACITY),
        })
        .when(!label.is_empty(), |el| el.child(label.to_string()))
}
