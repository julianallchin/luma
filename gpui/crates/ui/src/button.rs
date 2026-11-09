//! Shared action buttons use the Comet-style chip on every app surface.
//! Keep labels in their supplied case; do not recreate the old square,
//! bordered, uppercase button style.

use crate::icons::IconName;
use crate::node::{AgentNode as _, Instrument, Role};
use crate::rpx;
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
    /// Dimmed to [`ladder::DISABLED_OPACITY`], inert under the pointer: a
    /// [`Control`] drops the click handler a caller attaches.
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

/// A shared control: what [`button`], [`icon_button`] and
/// [`pill::primary`](crate::pill::primary) return. It knows whether it is
/// enabled, so the caller attaches its click handler and names its automation
/// node whatever the state: a disabled control drops the handler and reports
/// itself disabled.
///
/// Press it like a `Div`: [`Self::id`], then [`StatefulControl::on_click`].
pub struct Control {
    el: Div,
    enabled: Enabled,
}

/// A [`Control`] with an id, which is what can take a click handler.
pub struct StatefulControl {
    el: Stateful<Div>,
    enabled: Enabled,
}

impl Control {
    pub(crate) fn new(el: Div, enabled: Enabled) -> Self {
        Self { el, enabled }
    }

    /// See [`InteractiveElement::id`].
    pub fn id(self, id: impl Into<ElementId>) -> StatefulControl {
        StatefulControl {
            el: self.el.id(id),
            enabled: self.enabled,
        }
    }

    /// See [`Instrument::agent_node`]. A disabled control is reported disabled.
    pub fn agent_node(
        self,
        role: Role,
        label: impl Into<SharedString>,
    ) -> <Div as Instrument>::Output {
        self.el
            .agent_node(role, label)
            .agent_disabled(self.enabled == Enabled::No)
    }
}

impl StatefulControl {
    /// Run `listener` when the control is clicked, by pointer or by key. A
    /// disabled control drops it, so nothing that reaches the control can run
    /// it. A press on a disabled control still reaches what is around it.
    pub fn on_click(
        mut self,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        if self.enabled == Enabled::Yes {
            self.el = self.el.on_click(listener);
        }
        self
    }

    /// See [`StatefulInteractiveElement::tooltip`]. A disabled control shows
    /// its tooltip too, which is where to say why it is disabled.
    pub fn tooltip(mut self, build: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.el = self.el.tooltip(build);
        self
    }

    /// See [`Instrument::agent_node`]. A disabled control is reported disabled.
    pub fn agent_node(
        self,
        role: Role,
        label: impl Into<SharedString>,
    ) -> <Stateful<Div> as Instrument>::Output {
        self.el
            .agent_node(role, label)
            .agent_disabled(self.enabled == Enabled::No)
    }
}

impl Styled for Control {
    fn style(&mut self) -> &mut StyleRefinement {
        self.el.style()
    }
}

impl Styled for StatefulControl {
    fn style(&mut self) -> &mut StyleRefinement {
        self.el.style()
    }
}

impl ParentElement for Control {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.el.extend(elements);
    }
}

impl ParentElement for StatefulControl {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.el.extend(elements);
    }
}

/// Focus, keys and mouse handlers other than the click, which only
/// [`StatefulControl::on_click`] attaches.
impl InteractiveElement for StatefulControl {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.el.interactivity()
    }
}

impl IntoElement for Control {
    type Element = Div;

    fn into_element(self) -> Div {
        self.el
    }
}

impl IntoElement for StatefulControl {
    type Element = Stateful<Div>;

    fn into_element(self) -> Stateful<Div> {
        self.el
    }
}

/// A compact rounded action button. The caller adds its id and handler.
pub fn button(label: &str, enabled: Enabled) -> Control {
    let el = float::chip_plate(enabled)
        .justify_center()
        .px(rpx(float::PICKER_CHIP_PAD))
        .map(|el| match enabled {
            Enabled::Yes => el.tab_index(0),
            Enabled::No => el.opacity(ladder::DISABLED_OPACITY),
        })
        .when(!label.is_empty(), |el| el.child(label.to_string()));
    Control::new(el, enabled)
}

/// The glyph size inside an [`icon_button`].
const ICON: f32 = 14.;

/// A square icon button: quiet ink in a rounded box that takes a wash on
/// hover. Nothing is painted at rest, so a row of these reads as chrome.
///
/// The caller adds the id, the handlers and the automation node. Do not add a
/// second `hover` style: gpui allows only one.
pub fn icon_button(icon: IconName, enabled: Enabled) -> Control {
    Control::new(icon_plate(icon, false, enabled), enabled)
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
        .size(rpx(CONTROL_HEIGHT))
        .rounded(rpx(radius::CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(icon).size(rpx(ICON)));
    match enabled {
        Enabled::Yes => toggle_paint(plate.cursor_pointer(), active),
        Enabled::No => plate
            .text_color(glass::ink(0.55))
            .opacity(ladder::DISABLED_OPACITY),
    }
}
