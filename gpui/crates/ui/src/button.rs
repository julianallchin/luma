//! Shared action buttons use the Comet-style chip on every app surface.
//! Keep labels in their supplied case; do not recreate the old square,
//! bordered, uppercase button style.

use crate::icons::IconName;
use crate::{float, glass, ladder, radius, CONTROL_HEIGHT};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::Icon;

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

/// The glyph size inside an [`icon_button`].
const ICON: f32 = 14.;

/// A square icon button: quiet ink in a rounded box that takes a wash on
/// hover. Nothing is painted at rest, so a row of these reads as chrome.
///
/// The caller adds the id, the handlers and the automation node. Do not add a
/// second `hover` style: gpui allows only one.
pub fn icon_button(icon: IconName, enabled: Enabled) -> Div {
    icon_plate(icon, false, enabled)
}

/// An [`icon_button`] that shows its own state. While `active`, it keeps the
/// wash a selected tab chip has, so a panel toggle shows whether its panel is
/// open.
pub fn icon_toggle(icon: IconName, active: bool) -> Div {
    icon_plate(icon, active, Enabled::Yes)
}

/// The paint of a chrome control that can be active: an icon toggle, a tab
/// chip. Active rests at [`glass::WASH_REST`]; hover lifts one step.
///
/// This sets the one `hover` style the element may have.
pub fn toggle_paint<E: Styled + InteractiveElement + FluentBuilder>(el: E, active: bool) -> E {
    el.text_color(glass::ink(if active { 0.92 } else { 0.55 }))
        .when(active, |el| el.bg(glass::wash(glass::WASH_REST)))
        .hover(move |el| {
            el.bg(glass::wash(if active {
                glass::WASH_EMPHASIS
            } else {
                glass::WASH_SUBTLE
            }))
            .text_color(glass::ink(if active { 0.92 } else { 0.90 }))
        })
}

fn icon_plate(icon: IconName, active: bool, enabled: Enabled) -> Div {
    let plate = div()
        .flex_none()
        .size(px(CONTROL_HEIGHT))
        .rounded(px(radius::CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(icon).size(px(ICON)));
    match enabled {
        Enabled::Yes => toggle_paint(plate.cursor_pointer(), active),
        Enabled::No => plate
            .text_color(glass::ink(0.55))
            .opacity(ladder::DISABLED_OPACITY),
    }
}
