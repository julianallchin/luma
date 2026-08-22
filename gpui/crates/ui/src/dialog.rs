//! Shared modal geometry and interaction boundary.
//!
//! The host owns three real backdrop samples: the shell, a stronger leading
//! sidebar band, and the foreground card. Routes only supply content.

pub mod morph;

use gpui::prelude::*;
use gpui::{
    px, AnyElement, App, Bounds, Corners, Element, ElementId, FocusHandle, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Window,
};
use gpui_component::FocusTrapElement;

use crate::{glass, motion, node::AgentNode as _, node::Instrument as _};

/// Keep cards clear of both viewport edges and the titlebar controls.
pub const VIEWPORT_GUTTER: f32 = 16.0;
pub const TITLEBAR_CLEARANCE: f32 = 38.0;
pub const CARD_RADIUS: f32 = 16.0;
pub const SHELL_BLUR: f32 = 18.0;
pub const SIDEBAR_BLUR: f32 = 26.0;
pub const CARD_BLUR: f32 = 44.0;
/// Per-element backdrop sampling currently has a native implementation on
/// macOS. Other platforms deliberately render the already-opaque glass palette
/// without asking a sharp translucent fallback to masquerade as blur.
pub const BACKDROP_BLUR_SUPPORTED: bool = cfg!(target_os = "macos");

/// Route-owned policy for clicks on the modal ground.
///
/// The host owns the hit target and card boundary, while the route decides
/// whether dismissal is legal. Venue onboarding, for example, remains modal
/// until a venue exists.
pub enum ScrimDismiss {
    Disabled,
    Enabled(Box<dyn Fn(&mut Window, &mut App)>),
}

pub fn frosted(corner_radius: f32, blur_radius: f32, child: impl IntoElement) -> Frosted {
    Frosted {
        corner_radius,
        blur_radius,
        child: child.into_any_element(),
    }
}

/// Isolate `child` into a transparent texture, blur only that subtree, and
/// composite it scaled about its center. A zero radius is a sharp offscreen
/// layer, which lets one transition API interpolate blur → sharp.
pub fn filtered(blur_radius: f32, scale: f32, child: impl IntoElement) -> Filtered {
    Filtered {
        blur_radius,
        scale,
        child: child.into_any_element(),
    }
}

pub struct Filtered {
    blur_radius: f32,
    scale: f32,
    child: AnyElement,
}

impl Element for Filtered {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_filtered_layer(bounds, px(self.blur_radius), self.scale, |window| {
            self.child.paint(window, cx)
        });
    }
}

impl IntoElement for Filtered {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct Frosted {
    corner_radius: f32,
    blur_radius: f32,
    child: AnyElement,
}

impl Element for Frosted {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if !BACKDROP_BLUR_SUPPORTED {
            self.child.paint(window, cx);
            return;
        }
        window.paint_layer(bounds, |window| {
            window.paint_backdrop_blur(
                bounds,
                Corners::all(px(self.corner_radius)),
                px(self.blur_radius),
            );
            self.child.paint(window, cx);
        });
    }
}

impl IntoElement for Frosted {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// A full-window modal plane with one focus-contained foreground card.
///
/// `card` supplies route content and its desired size. Its maximum dimensions
/// are clamped to the usable viewport, so the same primitive remains operable
/// in compact windows. The shell paints native traffic-light controls after
/// this plane, keeping those controls reachable.
pub fn host(
    id: impl Into<ElementId>,
    viewport: gpui::Size<gpui::Pixels>,
    leading_width: Pixels,
    focus: &FocusHandle,
    focused: bool,
    semantic_label: impl Into<SharedString>,
    scrim_dismiss: ScrimDismiss,
    card: AnyElement,
) -> AnyElement {
    let id = id.into();
    let max_width = (viewport.width - px(VIEWPORT_GUTTER * 2.0)).max(px(1.0));
    let max_height = (viewport.height - px(TITLEBAR_CLEARANCE + VIEWPORT_GUTTER)).max(px(1.0));

    let card = gpui::div()
        .relative()
        // The card is a sibling of the full-window scrim. Register its hitbox
        // as an occluder so controls inside it cannot also hit the dismissal
        // plane behind it.
        .occlude()
        .max_w(max_width)
        .max_h(max_height)
        .overflow_hidden()
        .rounded(px(CARD_RADIUS))
        // Current GPUI's tab-order model needs an explicit group boundary:
        // focusing its container then advances into this group's first stop,
        // instead of cycling through unrelated shell controls first.
        .tab_group()
        // The handle contains the trap and is focused programmatically while
        // a route has no actionable child. It is not itself an action: GPUI's
        // tab contract requires container handles to opt out or keyboard
        // navigation stops on an invisible boundary instead of wrapping.
        .tab_stop(false)
        .focus_trap(id.clone(), focus)
        .child(card)
        .child(
            gpui::div()
                .absolute()
                .inset_0()
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(glass::hairline(0.10)),
        );
    let card = frosted(CARD_RADIUS, CARD_BLUR, card);
    let card = motion::dialog_in(id, gpui::div().child(card))
        .agent_node(crate::node::Role::Card, semantic_label)
        .agent_focused(focused);

    let mut scrim = gpui::div()
        .id("dialog-scrim")
        .absolute()
        .inset_0()
        .bg(glass::scrim(glass::DIALOG_SCRIM_ALPHA));
    if let ScrimDismiss::Enabled(dismiss) = scrim_dismiss {
        scrim = scrim
            .on_click(move |_, window, cx| dismiss(window, cx))
            // A semantic target restricted to the guaranteed empty left
            // gutter lets automation exercise a real scrim click without
            // aiming at the full-screen node's centre (which is the card).
            .child(
                gpui::div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .w(px(VIEWPORT_GUTTER))
                    .h_full()
                    .agent_node(crate::node::Role::Button, "Dismiss dialog"),
            );
    }

    let modal = gpui::div()
        .occlude()
        .relative()
        .w(viewport.width)
        .h(viewport.height)
        .flex()
        .items_center()
        .justify_center()
        .pt(px(TITLEBAR_CLEARANCE))
        .px(px(VIEWPORT_GUTTER))
        .pb(px(VIEWPORT_GUTTER))
        .child(scrim)
        .child(card)
        .into_any_element();
    let leading_width = leading_width.min(viewport.width).max(px(0.0));
    let plane = gpui::div()
        .relative()
        .w(viewport.width)
        .h(viewport.height)
        .when(leading_width > px(0.0), |plane| {
            plane.child(
                gpui::div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(leading_width)
                    .h(viewport.height)
                    .child(frosted(0.0, SIDEBAR_BLUR, gpui::div().size_full())),
            )
        })
        .child(modal);
    frosted(0.0, SHELL_BLUR, plane).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_viewport_keeps_positive_card_area() {
        let viewport = gpui::size(px(1.0), px(1.0));
        assert_eq!(
            (viewport.width - px(VIEWPORT_GUTTER * 2.0)).max(px(1.0)),
            px(1.0)
        );
        assert_eq!(
            (viewport.height - px(TITLEBAR_CLEARANCE + VIEWPORT_GUTTER)).max(px(1.0)),
            px(1.0)
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platforms_choose_an_opaque_readable_fallback() {
        assert!(!BACKDROP_BLUR_SUPPORTED);
        assert_eq!(crate::glass::GLASS_ALPHA, 1.0);
        assert_eq!(crate::glass::PANEL_ALPHA, 1.0);
        assert_eq!(crate::glass::OVERLAY_ALPHA, 1.0);
    }
}
