//! Shared modal geometry, lifecycle and interaction boundary.
//!
//! The host owns the modal plane — a dim tint over the shell — and the
//! foreground card's own frosted glass. Routes only supply content.
//!
//! # A dialog outlives its state
//!
//! gpui unmounts an element the frame the state behind it drops, so a route
//! that simply clears its overlay slot gets no exit at all — the card and its
//! scrim vanish between frames while the entrance they just played implied
//! they were objects with weight. [`Popup`] is the three-phase state that buys
//! the exit its frames: `open` → [`Popup::begin_close`] (still mounted, now
//! playing [`motion::DIALOG_OUT`] and taking no input) → [`Popup::tick_close`]
//! drops it once the exit has played out. The owner holds the `Popup` and ticks
//! it each frame; the host is handed only the closing instant and derives
//! everything else from the wall clock.

pub mod morph;

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    px, AnyElement, App, Bounds, Corners, Element, ElementId, FocusHandle, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Window,
};
use gpui_component::FocusTrapElement;

use crate::{glass, motion, node::AgentNode as _, node::Instrument as _, radius};

/// Keep cards clear of both viewport edges and the titlebar controls.
pub const VIEWPORT_GUTTER: f32 = 16.0;
pub const TITLEBAR_CLEARANCE: f32 = 38.0;
/// Backdrop-blur sigma behind a dialog card, and the only blur the modal plane
/// paints.
///
/// The plane behind it is a plain tint — see [`glass::DIALOG_SCRIM_ALPHA`]. The
/// card is a piece of glass laid on the shell, not a lens that smears it, and a
/// blur is how a *surface* says it is translucent; dimming is how a *plane*
/// says it is behind something. Blurring both said neither.
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
    Enabled(DismissHandler),
}

/// What a route runs when its scrim is clicked — typically "clear the overlay
/// slot", which under [`Popup`] means [`Popup::begin_close`] and then ticking
/// [`Popup::tick_close`] until the exit has played.
pub type DismissHandler = Box<dyn Fn(&mut Window, &mut App)>;

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
/// The card supplies route content and its desired size; its maximum
/// dimensions are clamped to the usable viewport, so the same primitive stays
/// operable in compact windows. The shell paints native traffic-light controls
/// after this plane, keeping those controls reachable.
///
/// A struct rather than a parameter list because none of these are the
/// *content* — they are the frame around it, and a caller reading
/// `Host { closing: None, .. }` at the call site can see which frame it built.
pub struct Host<'a> {
    pub id: ElementId,
    pub viewport: gpui::Size<Pixels>,
    pub focus: &'a FocusHandle,
    pub focused: bool,
    pub label: SharedString,
    pub scrim_dismiss: ScrimDismiss,
    /// When the exit began, from [`Popup::closing_since`]. `None` is a live
    /// dialog; `Some` is one still mounted only so it can finish leaving.
    pub closing: Option<Instant>,
}

impl Host<'_> {
    /// Paint the plane around `card`.
    #[must_use]
    pub fn render(self, card_body: AnyElement) -> AnyElement {
        // Exit progress comes off the wall clock, never off an animation's own
        // delta — see [`motion::exit_progress`]. `live` is the fraction of the
        // dialog still present: the scrim's alpha and the card's blur both ride
        // it. The blur especially — `paint_backdrop_blur` ignores element
        // opacity, so a blur held at full strength through the fade would pop
        // off at unmount instead of thinning out with the card.
        let exit = self
            .closing
            .map(|since| motion::exit_progress(&motion::DIALOG_OUT, since));
        let live = 1.0 - exit.unwrap_or(0.0);
        // A leaving dialog is a GHOST: visible, untouchable. Every hitbox,
        // occluder, focus trap and tab stop below is gated on this. The moment
        // it is dismissed the shell underneath is live again — a user who
        // clicks straight through a dismissal must land on what they aimed at,
        // not on the corpse of the thing they just closed.
        let interactive = exit.is_none();

        let max_width = (self.viewport.width - px(VIEWPORT_GUTTER * 2.0)).max(px(1.0));
        let max_height =
            (self.viewport.height - px(TITLEBAR_CLEARANCE + VIEWPORT_GUTTER)).max(px(1.0));

        // Clamped to the usable viewport, so the same primitive stays operable
        // in a compact window.
        let clamp = gpui::div()
            .relative()
            .max_w(max_width)
            .max_h(max_height)
            .overflow_hidden()
            .rounded(px(radius::MODAL));
        // Two whole branches rather than one chain: `occlude`/`tab_group`/
        // `focus_trap` are what make the card catch input and hold focus, and a
        // ghost must have none of them.
        let card = if interactive {
            clamp
                // The card is a sibling of the full-window scrim. Register its
                // hitbox as an occluder so controls inside it cannot also hit
                // the dismissal plane behind it.
                .occlude()
                // Current GPUI's tab-order model needs an explicit group
                // boundary: focusing its container then advances into this
                // group's first stop, instead of cycling through unrelated
                // shell controls first.
                .tab_group()
                // The handle contains the trap and is focused programmatically
                // while a route has no actionable child. It is not itself an
                // action: GPUI's tab contract requires container handles to opt
                // out or keyboard navigation stops on an invisible boundary
                // instead of wrapping.
                .tab_stop(false)
                .focus_trap(self.id.clone(), self.focus)
                .child(card_body)
                .into_any_element()
        } else {
            clamp.child(card_body).into_any_element()
        };
        // The card's own fill, rim and radius belong to `morph::card` — the
        // one place a dialog card is described. This wrapper only clamps and
        // clips it.
        let card = frosted(radius::MODAL, CARD_BLUR * live, card);

        let card = match exit {
            // A fresh animation id: reusing the entrance's would inherit its
            // finished clock and snap straight to the end state.
            Some(t) => motion::dialog_out(
                ElementId::from(SharedString::from(format!("{}-out", self.label))),
                t,
                gpui::div().child(card),
            )
            .into_any_element(),
            // A dialog names itself only while it is really there: a script
            // that found a card mid-exit would be looking at something it can
            // no longer act on.
            None => motion::dialog_in(self.id, gpui::div().child(card))
                .agent_node(crate::node::Role::Card, self.label)
                .agent_focused(self.focused)
                .into_any_element(),
        };

        let plate = gpui::div()
            .absolute()
            .inset_0()
            .bg(glass::scrim(glass::DIALOG_SCRIM_ALPHA * live));
        // The scrim is the widest hit target in the app, so a ghost's scrim is
        // the one that must go first: `.id()` is what registers its hitbox, and
        // a leaving dialog never takes it. The entrance fade is skipped too —
        // wrapping a scrim that is already fading out would replay it from 0
        // and fade it back *in* under the leaving card.
        let (scrim, dismissible) = match self.scrim_dismiss {
            ScrimDismiss::Enabled(dismiss) if interactive => (
                motion::fade_quick(
                    "dialog-scrim-in",
                    plate
                        .id("dialog-scrim")
                        .on_click(move |_, window, cx| dismiss(window, cx)),
                )
                .into_any_element(),
                true,
            ),
            _ if interactive => (
                motion::fade_quick("dialog-scrim-in", plate).into_any_element(),
                false,
            ),
            _ => (plate.into_any_element(), false),
        };

        // The dismissal margin, as real columns rather than padding. Clicks
        // fall through to the scrim beneath (neither column occludes); naming
        // the leading one gives a driver somewhere to aim that is guaranteed
        // empty, which the full-window scrim's centre — the card — is not.
        let gutter = || gpui::div().w(px(VIEWPORT_GUTTER)).h_full().flex_none();
        let leading_gutter = if dismissible {
            gutter()
                .agent_node(crate::node::Role::Button, "Dismiss dialog")
                .into_any_element()
        } else {
            gutter().into_any_element()
        };

        let mut modal = gpui::div()
            .relative()
            .w(self.viewport.width)
            .h(self.viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .pt(px(TITLEBAR_CLEARANCE))
            .pb(px(VIEWPORT_GUTTER));
        if interactive {
            modal = modal.occlude();
        }
        modal
            .child(scrim)
            .child(leading_gutter)
            .child(card)
            .child(gutter())
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Overlay state with an exit phase — see the module docs.
///
/// Use [`Self::is_open`] for logic (a closing dialog already reads as closed,
/// so its handlers fall through) and [`Self::get`] / [`Self::closing_since`]
/// for rendering.
pub struct Popup<T> {
    /// `Some((state, closing_since))` while mounted; the inner `Some` is the
    /// exit-phase start.
    inner: Option<(T, Option<Instant>)>,
}

impl<T> Default for Popup<T> {
    fn default() -> Self {
        Self { inner: None }
    }
}

impl<T> Popup<T> {
    pub fn open(&mut self, value: T) {
        self.inner = Some((value, None));
    }

    /// Mounted and interactive.
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self.inner, Some((_, None)))
    }

    #[must_use]
    pub fn is_closing(&self) -> bool {
        matches!(self.inner, Some((_, Some(_))))
    }

    /// When the exit began — what [`Host::closing`] is fed.
    #[must_use]
    pub fn closing_since(&self) -> Option<Instant> {
        match &self.inner {
            Some((_, since)) => *since,
            None => None,
        }
    }

    /// The state while mounted — open OR playing the exit. Render paths use
    /// this; logic paths use [`Self::as_open`] / [`Self::open_mut`].
    #[must_use]
    pub fn get(&self) -> Option<&T> {
        self.inner.as_ref().map(|(value, _)| value)
    }

    /// The state while mounted, mutably. Render-side bookkeeping — ticking a
    /// route animation, say — has to keep running through the exit, which is
    /// the whole reason the state is still here.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.inner.as_mut().map(|(value, _)| value)
    }

    /// The state only while genuinely open.
    #[must_use]
    pub fn as_open(&self) -> Option<&T> {
        match &self.inner {
            Some((value, None)) => Some(value),
            _ => None,
        }
    }

    pub fn open_mut(&mut self) -> Option<&mut T> {
        match &mut self.inner {
            Some((value, None)) => Some(value),
            _ => None,
        }
    }

    /// Begin leaving. Returns `true` when an exit actually started, which is
    /// the caller's cue to keep frames coming until [`Self::finish_close`]
    /// takes; `false` when there is nothing to play — already closing, already
    /// closed, or unmounted outright because motion is off.
    ///
    /// Reduced motion drops the state here rather than animating to nothing,
    /// so a driver that dismisses a dialog and immediately asserts it is gone
    /// is not racing a timer.
    ///
    /// The motion policy is read here, from `cx`, rather than passed in: a
    /// caller that forgot the flag would leave a dialog animating out under a
    /// user who has asked for no animation, and that is not a mistake a call
    /// site should be able to make. [`Self::begin_close_at`] is the pure core,
    /// for tests that have no `App` — the same split
    /// [`crate::motion::HoverFades`] keeps for wall time.
    pub fn begin_close(&mut self, cx: &App) -> bool {
        self.begin_close_at(motion::reduced_motion(cx))
    }

    /// [`Self::begin_close`] with the motion policy stated explicitly. Prefer
    /// the wrapper: this exists so the state machine can be exercised without
    /// a window.
    pub fn begin_close_at(&mut self, reduced_motion: bool) -> bool {
        if !self.is_open() {
            return false;
        }
        if reduced_motion {
            self.inner = None;
            return false;
        }
        if let Some((_, closing)) = &mut self.inner {
            *closing = Some(Instant::now());
        }
        true
    }

    /// Drop the state if the exit has run its course, and report whether it is
    /// still playing.
    ///
    /// Call this once per frame while [`Self::is_closing`]; it is the reaper.
    /// Deliberately frame-driven rather than timer-driven, for the same reason
    /// [`crate::pane`]'s tweens are: a background timer's clock is virtual
    /// under a headless harness and never advances, so a dialog dismissed in a
    /// test would stay mounted and occluding forever. Wall-clock progress read
    /// on each frame works identically in both worlds.
    ///
    /// A dialog reopened (or re-closed) since the matching
    /// [`Self::begin_close`] is left alone — the newer phase reaps itself.
    pub fn finish_close(&mut self) {
        if let Some((_, Some(since))) = &self.inner {
            if since.elapsed() >= exit_span() {
                self.inner = None;
            }
        }
    }

    /// Reap if due, then report whether frames must keep coming.
    #[must_use]
    pub fn tick_close(&mut self) -> bool {
        self.finish_close();
        self.is_closing()
    }
}

/// Wall-clock span of the dialog exit, honoring the measurement knob.
fn exit_span() -> Duration {
    motion::span(&motion::DIALOG_OUT)
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

    #[test]
    fn a_dialog_leaves_through_three_phases() {
        let mut popup: Popup<u8> = Popup::default();
        assert!(!popup.begin_close_at(false), "nothing to close");

        popup.open(7);
        assert!(popup.is_open());
        assert!(!popup.is_closing());
        assert_eq!(popup.as_open(), Some(&7));
        assert_eq!(popup.closing_since(), None);

        assert!(popup.begin_close_at(false), "the caller now owes a reap");
        // Closing reads as closed to logic, as present to paint.
        assert!(!popup.is_open());
        assert!(popup.is_closing());
        assert_eq!(popup.as_open(), None, "a leaving dialog takes no input");
        assert_eq!(popup.get(), Some(&7), "…but is still painted");
        assert!(popup.closing_since().is_some());

        // A second dismissal during the exit is inert — one reap, one drop.
        assert!(!popup.begin_close_at(false));

        // The reaper fires before the span is up: the state stays.
        popup.finish_close();
        assert!(popup.is_closing());
    }

    #[test]
    fn reopening_mid_exit_cancels_the_leaving() {
        let mut popup: Popup<u8> = Popup::default();
        popup.open(1);
        popup.begin_close_at(false);
        popup.open(2);
        assert!(popup.is_open());
        assert_eq!(popup.closing_since(), None);
        // The stale reap from the first close must not take the new state.
        popup.finish_close();
        assert_eq!(popup.as_open(), Some(&2));
    }

    /// Motion off (`LUMA_MOTION=off`, and the accessibility preference) means
    /// no exit at all — the state goes on the spot, so a driver that dismisses
    /// and asserts absence in the next frame is not racing the reaper.
    #[test]
    fn reduced_motion_unmounts_instead_of_leaving() {
        let mut popup: Popup<u8> = Popup::default();
        popup.open(3);
        assert!(
            !popup.begin_close_at(true),
            "an instant close leaves nothing to reap"
        );
        assert!(!popup.is_open());
        assert!(!popup.is_closing());
        assert_eq!(popup.get(), None);
    }

    /// The reaper must not fire before the animation it is reaping has run,
    /// or the card disappears mid-fade and the exit reads as a cut.
    #[test]
    fn the_reap_waits_out_the_exit() {
        assert_eq!(
            exit_span(),
            motion::DIALOG_OUT.total().mul_f32(motion::speed_scale())
        );
        let mut popup: Popup<u8> = Popup::default();
        popup.open(1);
        popup.begin_close_at(false);
        assert!(popup.tick_close(), "still playing, keep frames coming");
        assert!(popup.is_closing());
        // A popup that was never closing asks for no frames.
        let mut idle: Popup<u8> = Popup::default();
        idle.open(2);
        assert!(!idle.tick_close());
        assert!(idle.is_open());
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
