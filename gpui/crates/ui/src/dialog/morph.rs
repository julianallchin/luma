//! State-owned dialog route morphing.
//!
//! Route content owns only a [`RouteDescriptor`]. This reducer owns the live
//! container size and every painted content pose, so rebuilding the element
//! tree cannot restart a transition. Intrinsic routes remain pending while a
//! hidden copy is measured; only [`MorphDialog::resolve_intrinsic`] commits the
//! new target.

use std::time::{Duration, Instant};

use std::ops::Deref;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    px, AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, SharedString, Window,
};

use crate::motion::{self, RESIZE};
use crate::node::{Instrument as _, Role};

/// Logical dialog content size before the host applies viewport clamps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MorphSize {
    pub width: f32,
    pub height: f32,
}

impl MorphSize {
    #[must_use]
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    fn normalized(self) -> Self {
        Self {
            width: self.width.max(1.0),
            height: self.height.max(1.0),
        }
    }

    fn min(self, maximum: Self) -> Self {
        Self {
            width: self.width.min(maximum.width),
            height: self.height.min(maximum.height),
        }
        .normalized()
    }

    fn interpolate(self, to: Self, progress: f32) -> Self {
        Self {
            width: motion::lerp(self.width, to.width, progress),
            height: motion::lerp(self.height, to.height, progress),
        }
    }
}

/// How a child declares the size it wants the shared container to become.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RouteSize {
    /// Both axes are known before the route is requested.
    Exact(MorphSize),
    /// Lay the target out invisibly, then clamp its measured size to this cap.
    Intrinsic { maximum: MorphSize },
}

/// Which side of a two-content choreography is being evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerRole {
    Outgoing,
    Incoming,
}

/// One content layer's paint-only pose. Layout stays at the route's resolved
/// size while the outer card clips an independently animated viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerPose {
    pub x: f32,
    pub opacity: f32,
    pub blur: f32,
    pub scale: f32,
}

impl LayerPose {
    pub const REST: Self = Self {
        x: 0.0,
        opacity: 1.0,
        blur: 0.0,
        scale: 1.0,
    };

    fn interpolate(self, to: Self, progress: f32) -> Self {
        Self {
            x: motion::lerp(self.x, to.x, progress),
            opacity: motion::lerp(self.opacity, to.opacity, progress),
            blur: motion::lerp(self.blur, to.blur, progress),
            scale: motion::lerp(self.scale, to.scale, progress),
        }
    }
}

/// Extension seam for route choreography. A custom evaluator returns the pose
/// at normalized progress for either role; the reducer and host stay unchanged.
pub type TransitionEvaluator = fn(LayerRole, f32) -> LayerPose;

#[derive(Clone, Copy)]
pub enum MorphTransition {
    Right,
    Scale,
    CrossFade,
    Custom(TransitionEvaluator),
}

impl std::fmt::Debug for MorphTransition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Right => formatter.write_str("Right"),
            Self::Scale => formatter.write_str("Scale"),
            Self::CrossFade => formatter.write_str("CrossFade"),
            Self::Custom(_) => formatter.write_str("Custom(..)"),
        }
    }
}

impl MorphTransition {
    #[must_use]
    pub fn pose(self, role: LayerRole, progress: f32) -> LayerPose {
        let progress = progress.clamp(0.0, 1.0);
        match self {
            Self::Right => match role {
                LayerRole::Outgoing => LayerPose {
                    x: 16.0 * progress,
                    opacity: 1.0 - progress,
                    blur: 16.0 * progress,
                    scale: 1.0,
                },
                LayerRole::Incoming => LayerPose {
                    x: -16.0 * (1.0 - progress),
                    opacity: progress,
                    blur: 16.0 * (1.0 - progress),
                    scale: 1.0,
                },
            },
            Self::Scale => match role {
                LayerRole::Outgoing => LayerPose {
                    opacity: 1.0 - progress,
                    scale: 1.0 + 0.04 * progress,
                    ..LayerPose::REST
                },
                LayerRole::Incoming => LayerPose {
                    opacity: progress,
                    scale: 0.96 + 0.04 * progress,
                    ..LayerPose::REST
                },
            },
            Self::CrossFade => LayerPose {
                opacity: match role {
                    LayerRole::Outgoing => 1.0 - progress,
                    LayerRole::Incoming => progress,
                },
                ..LayerPose::REST
            },
            Self::Custom(evaluate) => evaluate(role, progress),
        }
    }
}

/// Child-owned route metadata; content itself remains outside the reducer.
#[derive(Clone, Debug)]
pub struct RouteDescriptor<K> {
    pub key: K,
    pub size: RouteSize,
    pub transition: MorphTransition,
}

impl<K> RouteDescriptor<K> {
    #[must_use]
    pub fn exact(key: K, width: f32, height: f32) -> Self {
        Self {
            key,
            size: RouteSize::Exact(MorphSize::new(width, height)),
            transition: MorphTransition::Right,
        }
    }

    #[must_use]
    pub fn intrinsic(key: K, maximum: MorphSize) -> Self {
        Self {
            key,
            size: RouteSize::Intrinsic { maximum },
            transition: MorphTransition::Right,
        }
    }

    #[must_use]
    pub fn with_transition(mut self, transition: MorphTransition) -> Self {
        self.transition = transition;
        self
    }
}

#[derive(Clone, Debug)]
struct ResolvedRoute<K> {
    descriptor: RouteDescriptor<K>,
    size: MorphSize,
}

#[derive(Clone, Debug)]
struct AnimatedLayer<K> {
    route: ResolvedRoute<K>,
    from: LayerPose,
    to: LayerPose,
}

#[derive(Clone, Debug)]
struct Flight<K> {
    layers: Vec<AnimatedLayer<K>>,
    from_size: MorphSize,
    target: ResolvedRoute<K>,
    started: Instant,
    duration: Duration,
}

/// An immutable layer sample for the current frame.
#[derive(Clone, Debug)]
pub struct SampledLayer<K> {
    route: ResolvedRoute<K>,
    pub key: K,
    /// Stable resolved layout size for this route. It never follows the live
    /// container interpolation, so responsive children cannot reflow in flight.
    pub size: MorphSize,
    pub pose: LayerPose,
    /// Only a committed target receives input. Every in-flight layer is a
    /// paint-only copy; this prevents focus or pointer admission before the
    /// geometry and target have atomically settled.
    pub interactive: bool,
}

#[derive(Clone, Debug)]
pub struct MorphSample<K> {
    pub size: MorphSize,
    pub layers: Vec<SampledLayer<K>>,
    pub progress: f32,
    pub animating: bool,
}

/// Which version of route content the host asks the child to build.
///
/// `PaintOnly` must contain no focus handles or event listeners. It is the only
/// mode requested during a flight. Once committed, the target is rebuilt as
/// `Interactive` outside the filtered offscreen layer and receives the card's
/// occluding hitbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentMode {
    PaintOnly,
    Interactive,
}

/// Paint one sampled container. The child callback owns route content; this
/// function owns geometry, clipping, layer order, filtered poses and the
/// target's input plane.
pub fn card<K>(
    sample: &MorphSample<K>,
    semantic_label: impl Into<SharedString>,
    mut content: impl FnMut(&K, ContentMode) -> AnyElement,
) -> AnyElement {
    let layers = sample.layers.iter().map(|layer| {
        let key = &layer.key;
        let mode = if layer.interactive {
            ContentMode::Interactive
        } else {
            ContentMode::PaintOnly
        };
        let pose = layer.pose;
        let left = (sample.size.width - layer.size.width) / 2.0 + pose.x;
        let top = (sample.size.height - layer.size.height) / 2.0;
        let shell = gpui::div()
            .absolute()
            .top(px(top))
            .left(px(left))
            .w(px(layer.size.width))
            .h(px(layer.size.height))
            .opacity(pose.opacity);
        if mode == ContentMode::Interactive {
            shell.occlude().child(content(key, mode))
        } else {
            shell.child(super::filtered(
                pose.blur,
                pose.scale,
                gpui::div().size_full().child(content(key, mode)),
            ))
        }
    });
    gpui::div()
        .relative()
        .flex_none()
        .w(px(sample.size.width))
        .h(px(sample.size.height))
        .overflow_hidden()
        .children(layers)
        .agent_node(Role::Card, semantic_label)
        .into_any_element()
}

type MeasureCallback = Rc<dyn Fn(gpui::Size<Pixels>, &mut Window, &mut App)>;

/// Lay `child` out without prepainting or painting it, then defer its measured
/// size to `on_measure`. Skipping child prepaint is what makes the hidden copy
/// incapable of registering focus, hitboxes or event listeners.
pub fn premeasure(
    child: impl IntoElement,
    on_measure: impl Fn(gpui::Size<Pixels>, &mut Window, &mut App) + 'static,
) -> Premeasure {
    Premeasure {
        child: child.into_any_element(),
        on_measure: Rc::new(on_measure),
    }
}

pub struct Premeasure {
    child: AnyElement,
    on_measure: MeasureCallback,
}

impl Element for Premeasure {
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let measured = bounds.size;
        let on_measure = Rc::clone(&self.on_measure);
        window.defer(cx, move |window, cx| on_measure(measured, window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

impl IntoElement for Premeasure {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteRequest {
    Unchanged,
    NeedsMeasure,
    Transitioning,
    Settled,
}

/// Identifies one hidden layout request. Intrinsic content can request another
/// measurement without changing route identity, so a route key alone cannot
/// distinguish a late callback from the measurement currently in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasurementToken(u64);

#[derive(Clone, Debug)]
pub struct PendingMeasurement<K> {
    pub token: MeasurementToken,
    route: RouteDescriptor<K>,
}

impl<K> Deref for PendingMeasurement<K> {
    type Target = RouteDescriptor<K>;

    fn deref(&self) -> &Self::Target {
        &self.route
    }
}

/// The one durable reducer for a morphing dialog instance.
#[derive(Clone, Debug)]
pub struct MorphDialog<K> {
    settled: ResolvedRoute<K>,
    pending_measure: Option<PendingMeasurement<K>>,
    next_measurement: u64,
    flight: Option<Flight<K>>,
    focus_after_commit: Option<K>,
}

impl<K: Clone + Eq> MorphDialog<K> {
    #[must_use]
    pub fn new(route: RouteDescriptor<K>, initial_size: MorphSize) -> Self {
        Self {
            settled: ResolvedRoute {
                descriptor: route,
                size: initial_size.normalized(),
            },
            pending_measure: None,
            next_measurement: 0,
            flight: None,
            focus_after_commit: None,
        }
    }

    #[must_use]
    pub fn pending_measure(&self) -> Option<&PendingMeasurement<K>> {
        self.pending_measure.as_ref()
    }

    /// Explicitly invalidate an intrinsic route's measured geometry. This is
    /// the child-owned sizing hook for async/loading content whose route key is
    /// intentionally stable.
    pub fn remeasure_intrinsic(&mut self, route: RouteDescriptor<K>) -> RouteRequest {
        assert!(
            matches!(route.size, RouteSize::Intrinsic { .. }),
            "only intrinsic routes can be remeasured"
        );
        self.queue_measurement(route)
    }

    fn queue_measurement(&mut self, route: RouteDescriptor<K>) -> RouteRequest {
        let token = MeasurementToken(self.next_measurement);
        self.next_measurement = self.next_measurement.wrapping_add(1);
        self.pending_measure = Some(PendingMeasurement { token, route });
        RouteRequest::NeedsMeasure
    }

    #[must_use]
    pub fn target_key(&self) -> &K {
        self.flight
            .as_ref()
            .map(|flight| &flight.target.descriptor.key)
            .unwrap_or(&self.settled.descriptor.key)
    }

    /// Request a route. Intrinsic routes do not disturb the visible state;
    /// the host must premeasure them and call [`Self::resolve_intrinsic`].
    pub fn request(
        &mut self,
        route: RouteDescriptor<K>,
        now: Instant,
        reduced_motion: bool,
    ) -> RouteRequest {
        match route.size {
            RouteSize::Exact(size) => {
                let size = size.normalized();
                if !reduced_motion
                    && self.pending_measure.is_none()
                    && self.flight.as_ref().is_some_and(|flight| {
                        flight.target.descriptor.key == route.key && flight.target.size == size
                    })
                {
                    return RouteRequest::Unchanged;
                }
                self.pending_measure = None;
                self.begin(
                    ResolvedRoute {
                        descriptor: route,
                        size,
                    },
                    now,
                    reduced_motion,
                )
            }
            RouteSize::Intrinsic { .. } => {
                if reduced_motion {
                    let active_target = self
                        .flight
                        .as_ref()
                        .filter(|flight| flight.target.descriptor.key == route.key)
                        .map(|flight| flight.target.clone());
                    if let Some(target) = active_target {
                        self.pending_measure = None;
                        return self.begin(target, now, true);
                    }
                }
                if self
                    .pending_measure
                    .as_ref()
                    .is_some_and(|pending| pending.route.key == route.key)
                    || (self.target_key() == &route.key && self.pending_measure.is_none())
                {
                    RouteRequest::Unchanged
                } else {
                    self.queue_measurement(route)
                }
            }
        }
    }

    /// Commit the hidden intrinsic measurement iff it still belongs to the
    /// latest pending key. A stale measurement cannot replace a newer route.
    pub fn resolve_intrinsic(
        &mut self,
        token: MeasurementToken,
        measured: MorphSize,
        now: Instant,
        reduced_motion: bool,
    ) -> RouteRequest {
        let Some(pending) = self.pending_measure.as_ref() else {
            return RouteRequest::Unchanged;
        };
        if pending.token != token {
            return RouteRequest::Unchanged;
        }
        let route = self.pending_measure.take().unwrap().route;
        let RouteSize::Intrinsic { maximum } = route.size else {
            unreachable!("only intrinsic routes enter the measurement slot")
        };
        self.begin(
            ResolvedRoute {
                descriptor: route,
                size: measured.min(maximum),
            },
            now,
            reduced_motion,
        )
    }

    fn begin(
        &mut self,
        target: ResolvedRoute<K>,
        now: Instant,
        reduced_motion: bool,
    ) -> RouteRequest {
        let current = self.sample(now);
        if !current.animating
            && self.settled.descriptor.key == target.descriptor.key
            && self.settled.size == target.size
        {
            return RouteRequest::Unchanged;
        }
        if reduced_motion {
            self.settled = target;
            self.flight = None;
            self.focus_after_commit = Some(self.settled.descriptor.key.clone());
            return RouteRequest::Settled;
        }

        let transition = target.descriptor.transition;
        let mut layers = Vec::with_capacity(current.layers.len() + 1);
        let mut reused_target = false;
        for layer in current.layers {
            let is_target = layer.key == target.descriptor.key && layer.size == target.size;
            reused_target |= is_target;
            layers.push(AnimatedLayer {
                route: layer.route,
                from: layer.pose,
                to: if is_target {
                    LayerPose::REST
                } else {
                    transition.pose(LayerRole::Outgoing, 1.0)
                },
            });
        }
        if !reused_target {
            layers.push(AnimatedLayer {
                route: target.clone(),
                from: transition.pose(LayerRole::Incoming, 0.0),
                to: LayerPose::REST,
            });
        }
        self.flight = Some(Flight {
            layers,
            from_size: current.size,
            target,
            started: now,
            duration: RESIZE.total().mul_f32(motion::speed_scale()),
        });
        RouteRequest::Transitioning
    }

    #[must_use]
    pub fn sample(&self, now: Instant) -> MorphSample<K> {
        let Some(flight) = &self.flight else {
            return MorphSample {
                size: self.settled.size,
                layers: vec![SampledLayer {
                    route: self.settled.clone(),
                    key: self.settled.descriptor.key.clone(),
                    size: self.settled.size,
                    pose: LayerPose::REST,
                    interactive: true,
                }],
                progress: 1.0,
                animating: false,
            };
        };
        let raw = if flight.duration.is_zero() {
            1.0
        } else {
            now.saturating_duration_since(flight.started).as_secs_f32()
                / flight.duration.as_secs_f32()
        };
        let progress = RESIZE.progress(raw.clamp(0.0, 1.0));
        MorphSample {
            size: flight.from_size.interpolate(flight.target.size, progress),
            layers: flight
                .layers
                .iter()
                .map(|layer| SampledLayer {
                    route: layer.route.clone(),
                    key: layer.route.descriptor.key.clone(),
                    size: layer.route.size,
                    pose: layer.from.interpolate(layer.to, progress),
                    interactive: false,
                })
                .collect(),
            progress,
            animating: raw < 1.0,
        }
    }

    /// Settle a completed flight and report whether another animation frame is
    /// needed. Focus becomes available only in this commit step.
    pub fn tick(&mut self, now: Instant, reduced_motion: bool) -> bool {
        if self.flight.is_none() {
            return false;
        }
        if reduced_motion {
            let flight = self.flight.take().unwrap();
            self.settled = flight.target;
            self.focus_after_commit = Some(self.settled.descriptor.key.clone());
            return false;
        }
        if self.sample(now).animating {
            return true;
        }
        let flight = self.flight.take().unwrap();
        self.settled = flight.target;
        self.focus_after_commit = Some(self.settled.descriptor.key.clone());
        false
    }

    pub fn take_focus_after_commit(&mut self) -> Option<K> {
        self.focus_after_commit.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(key: &'static str, width: f32, height: f32) -> RouteDescriptor<&'static str> {
        RouteDescriptor::exact(key, width, height)
    }

    fn at(start: Instant, milliseconds: u64) -> Instant {
        start + Duration::from_millis(milliseconds)
    }

    #[test]
    fn right_pins_start_mid_and_end_on_both_axes_and_poses() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        assert_eq!(
            dialog.request(route("b", 600.0, 440.0), start, false),
            RouteRequest::Transitioning
        );
        assert_eq!(
            dialog.request(route("b", 600.0, 440.0), at(start, 20), false),
            RouteRequest::Unchanged,
            "a declarative re-request must not restart the live flight"
        );
        let opening = dialog.sample(start);
        assert_eq!(opening.size, MorphSize::new(400.0, 240.0));
        assert_eq!(opening.layers[0].size, MorphSize::new(400.0, 240.0));
        assert_eq!(opening.layers[1].size, MorphSize::new(600.0, 440.0));
        assert_eq!(opening.layers[0].pose, LayerPose::REST);
        assert_eq!(opening.layers[1].pose.x, -16.0);
        assert_eq!(opening.layers[1].pose.opacity, 0.0);
        assert_eq!(opening.layers[1].pose.blur, 16.0);

        let middle = dialog.sample(at(start, 100));
        assert!((middle.size.width - 594.356).abs() < 0.01);
        assert!((middle.size.height - 434.356).abs() < 0.01);
        assert!((middle.layers[0].pose.x - 15.548).abs() < 0.01);
        assert!((middle.layers[1].pose.x - -0.452).abs() < 0.01);
        assert_eq!(middle.layers[0].size, MorphSize::new(400.0, 240.0));
        assert_eq!(middle.layers[1].size, MorphSize::new(600.0, 440.0));

        assert!(!dialog.tick(at(start, 200), false));
        let end = dialog.sample(at(start, 200));
        assert_eq!(end.size, MorphSize::new(600.0, 440.0));
        assert_eq!(end.layers.len(), 1);
        assert_eq!(end.layers[0].pose, LayerPose::REST);
        assert!(end.layers[0].interactive);
        assert_eq!(dialog.take_focus_after_commit(), Some("b"));
    }

    #[test]
    fn reversal_and_replacement_anchor_every_visible_pose_and_rect() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        dialog.request(route("b", 600.0, 440.0), start, false);
        let flip = at(start, 70);
        let before = dialog.sample(flip);
        dialog.request(route("a", 400.0, 240.0), flip, false);
        let reversed = dialog.sample(flip);
        assert_eq!(reversed.size, before.size);
        for old in &before.layers {
            let new = reversed
                .layers
                .iter()
                .find(|layer| layer.key == old.key)
                .unwrap();
            assert_eq!(new.pose, old.pose);
        }

        let replace = at(flip, 40);
        let before = dialog.sample(replace);
        dialog.request(route("c", 520.0, 300.0), replace, false);
        let replaced = dialog.sample(replace);
        assert_eq!(replaced.size, before.size);
        for old in &before.layers {
            let new = replaced
                .layers
                .iter()
                .find(|layer| layer.key == old.key)
                .unwrap();
            assert_eq!(new.pose, old.pose);
        }
        assert_eq!(replaced.layers.last().unwrap().key, "c");
        assert!(replaced.layers.iter().all(|layer| !layer.interactive));

        assert!(!dialog.tick(at(replace, 200), false));
        let settled = dialog.sample(at(replace, 200));
        assert_eq!(settled.size, MorphSize::new(520.0, 300.0));
        assert_eq!(settled.layers.len(), 1);
        assert_eq!(settled.layers[0].key, "c");
        assert_eq!(settled.layers[0].size, MorphSize::new(520.0, 300.0));
        assert!(settled.layers[0].interactive);
    }

    #[test]
    fn intrinsic_waits_for_current_measurement_and_reduced_motion_commits_atomically() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        let intrinsic = RouteDescriptor::intrinsic("b", MorphSize::new(700.0, 500.0));
        assert_eq!(
            dialog.request(intrinsic, start, false),
            RouteRequest::NeedsMeasure
        );
        assert_eq!(
            dialog.sample(at(start, 50)).size,
            MorphSize::new(400.0, 240.0)
        );
        assert_eq!(
            dialog.resolve_intrinsic(
                MeasurementToken(u64::MAX),
                MorphSize::new(10.0, 10.0),
                at(start, 60),
                false,
            ),
            RouteRequest::Unchanged
        );
        let token = dialog.pending_measure().unwrap().token;
        assert_eq!(
            dialog.resolve_intrinsic(token, MorphSize::new(760.0, 420.0), at(start, 70), true),
            RouteRequest::Settled
        );
        let settled = dialog.sample(at(start, 70));
        assert_eq!(settled.size, MorphSize::new(700.0, 420.0));
        assert!(!settled.animating);
        assert_eq!(settled.layers.as_slice()[0].key, "b");
        assert_eq!(dialog.take_focus_after_commit(), Some("b"));
    }

    #[test]
    fn stale_intrinsic_replacement_cannot_displace_the_latest_measurement() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        dialog.request(
            RouteDescriptor::intrinsic("b", MorphSize::new(700.0, 500.0)),
            start,
            false,
        );
        let stale = dialog.pending_measure().unwrap().token;
        dialog.request(
            RouteDescriptor::intrinsic("c", MorphSize::new(640.0, 480.0)),
            at(start, 10),
            false,
        );
        let current = dialog.pending_measure().unwrap().token;
        assert_eq!(
            dialog.resolve_intrinsic(stale, MorphSize::new(620.0, 410.0), at(start, 20), false),
            RouteRequest::Unchanged
        );
        assert_eq!(dialog.pending_measure().unwrap().key, "c");
        assert_eq!(
            dialog.resolve_intrinsic(current, MorphSize::new(560.0, 360.0), at(start, 30), false),
            RouteRequest::Transitioning
        );
        assert!(!dialog.tick(at(start, 230), false));
        let settled = dialog.sample(at(start, 230));
        assert_eq!(settled.size, MorphSize::new(560.0, 360.0));
        assert_eq!(settled.layers.len(), 1);
        assert_eq!(settled.layers[0].key, "c");
    }

    #[test]
    fn same_key_intrinsic_content_can_invalidate_its_measurement() {
        let start = Instant::now();
        let descriptor = RouteDescriptor::intrinsic("a", MorphSize::new(700.0, 500.0));
        let mut dialog = MorphDialog::new(descriptor.clone(), MorphSize::new(400.0, 240.0));

        assert_eq!(
            dialog.request(descriptor.clone(), start, false),
            RouteRequest::Unchanged,
            "declarative rebuilds must not continuously remeasure"
        );
        assert_eq!(
            dialog.remeasure_intrinsic(descriptor),
            RouteRequest::NeedsMeasure
        );
        let stale = dialog.pending_measure().unwrap().token;
        assert_eq!(
            dialog.remeasure_intrinsic(RouteDescriptor::intrinsic(
                "a",
                MorphSize::new(720.0, 520.0),
            )),
            RouteRequest::NeedsMeasure
        );
        let current = dialog.pending_measure().unwrap().token;
        assert_ne!(stale, current);
        assert_eq!(
            dialog.resolve_intrinsic(stale, MorphSize::new(450.0, 260.0), at(start, 10), false),
            RouteRequest::Unchanged
        );
        assert_eq!(
            dialog.resolve_intrinsic(current, MorphSize::new(560.0, 360.0), at(start, 20), false,),
            RouteRequest::Transitioning
        );
        let opening = dialog.sample(at(start, 20));
        assert_eq!(opening.size, MorphSize::new(400.0, 240.0));
        assert_eq!(
            opening
                .layers
                .iter()
                .map(|layer| layer.size)
                .collect::<Vec<_>>(),
            vec![MorphSize::new(400.0, 240.0), MorphSize::new(560.0, 360.0)],
            "a resized same-key route needs an old-layout outgoing copy and a new-layout incoming copy"
        );
    }

    #[test]
    fn reused_key_with_changed_exact_size_hands_off_before_commit() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        dialog.request(route("b", 600.0, 440.0), start, false);
        let reverse = at(start, 70);
        let before = dialog.sample(reverse);

        dialog.request(route("a", 520.0, 320.0), reverse, false);
        let opening = dialog.sample(reverse);
        assert_eq!(opening.size, before.size);
        assert_eq!(opening.layers.len(), 3);
        assert_eq!(opening.layers[0].size, MorphSize::new(400.0, 240.0));
        assert_eq!(opening.layers[2].size, MorphSize::new(520.0, 320.0));
        assert_eq!(opening.layers[2].pose.opacity, 0.0);

        assert!(!dialog.tick(at(reverse, 200), false));
        let settled = dialog.sample(at(reverse, 200));
        assert_eq!(settled.layers.len(), 1);
        assert_eq!(settled.layers[0].size, MorphSize::new(520.0, 320.0));
        assert_eq!(settled.layers[0].pose, LayerPose::REST);
    }

    #[test]
    fn interrupted_same_key_resize_preserves_each_in_flight_layout() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        dialog.request(route("a", 560.0, 360.0), start, false);
        let interrupt = at(start, 70);
        let before = dialog.sample(interrupt);

        assert_eq!(
            before
                .layers
                .iter()
                .map(|layer| layer.size)
                .collect::<Vec<_>>(),
            vec![MorphSize::new(400.0, 240.0), MorphSize::new(560.0, 360.0)]
        );
        dialog.request(route("a", 600.0, 400.0), interrupt, false);
        let replaced = dialog.sample(interrupt);

        assert_eq!(replaced.size, before.size);
        assert_eq!(
            replaced
                .layers
                .iter()
                .take(before.layers.len())
                .map(|layer| layer.size)
                .collect::<Vec<_>>(),
            before
                .layers
                .iter()
                .map(|layer| layer.size)
                .collect::<Vec<_>>(),
            "replacement must preserve every sampled layout even when keys collide"
        );
        assert_eq!(
            replaced
                .layers
                .iter()
                .take(before.layers.len())
                .map(|layer| layer.pose)
                .collect::<Vec<_>>(),
            before
                .layers
                .iter()
                .map(|layer| layer.pose)
                .collect::<Vec<_>>(),
            "replacement must begin at the exact visible poses"
        );
        assert_eq!(
            replaced.layers.last().unwrap().size,
            MorphSize::new(600.0, 400.0)
        );

        assert!(!dialog.tick(at(interrupt, 200), false));
        let settled = dialog.sample(at(interrupt, 200));
        assert_eq!(settled.layers.len(), 1);
        assert_eq!(settled.layers[0].key, "a");
        assert_eq!(settled.layers[0].size, MorphSize::new(600.0, 400.0));
        assert_eq!(settled.layers[0].pose, LayerPose::REST);
        assert!(settled.layers[0].interactive);
    }

    #[test]
    fn reduced_motion_toggled_mid_flight_snaps_exact_and_intrinsic_targets() {
        let start = Instant::now();
        let mut exact = MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        exact.request(route("b", 600.0, 440.0), start, false);
        assert_eq!(
            exact.request(route("b", 600.0, 440.0), at(start, 60), true),
            RouteRequest::Settled
        );
        let snapped = exact.sample(at(start, 60));
        assert_eq!(snapped.size, MorphSize::new(600.0, 440.0));
        assert_eq!(snapped.layers.len(), 1);
        assert_eq!(snapped.layers[0].key, "b");
        assert!(snapped.layers[0].interactive);

        let mut intrinsic =
            MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
        let target = RouteDescriptor::intrinsic("b", MorphSize::new(700.0, 500.0));
        intrinsic.request(target, start, false);
        let token = intrinsic.pending_measure().unwrap().token;
        intrinsic.resolve_intrinsic(token, MorphSize::new(620.0, 420.0), at(start, 10), false);
        assert!(!intrinsic.tick(at(start, 70), true));
        let snapped = intrinsic.sample(at(start, 70));
        assert_eq!(snapped.size, MorphSize::new(620.0, 420.0));
        assert_eq!(snapped.layers.len(), 1);
        assert_eq!(snapped.layers[0].key, "b");
        assert_eq!(intrinsic.take_focus_after_commit(), Some("b"));
    }

    #[test]
    fn scale_crossfade_and_custom_run_through_the_reducer_and_prune() {
        fn custom(role: LayerRole, progress: f32) -> LayerPose {
            LayerPose {
                x: if role == LayerRole::Incoming {
                    -8.0 * (1.0 - progress)
                } else {
                    0.0
                },
                opacity: progress,
                blur: 0.0,
                scale: 1.0,
            }
        }
        let start = Instant::now();
        let transitions = [
            MorphTransition::Scale,
            MorphTransition::CrossFade,
            MorphTransition::Custom(custom),
        ];
        for transition in transitions {
            let mut dialog =
                MorphDialog::new(route("a", 400.0, 240.0), MorphSize::new(400.0, 240.0));
            dialog.request(
                route("b", 520.0, 320.0).with_transition(transition),
                start,
                false,
            );
            let opening = dialog.sample(start);
            assert_eq!(opening.layers.len(), 2);
            match transition {
                MorphTransition::Scale => {
                    assert_eq!(opening.layers[1].pose.scale, 0.96);
                    assert_eq!(opening.layers[1].pose.opacity, 0.0);
                }
                MorphTransition::CrossFade => {
                    assert_eq!(opening.layers[1].pose.scale, 1.0);
                    assert_eq!(opening.layers[1].pose.opacity, 0.0);
                }
                MorphTransition::Custom(_) => {
                    assert_eq!(opening.layers[1].pose.x, -8.0);
                    assert_eq!(opening.layers[0].pose.opacity, 1.0);
                }
                MorphTransition::Right => unreachable!(),
            }
            assert!(!dialog.tick(at(start, 200), false));
            let settled = dialog.sample(at(start, 200));
            assert_eq!(settled.layers.len(), 1);
            assert_eq!(settled.layers[0].key, "b");
            assert_eq!(settled.layers[0].pose, LayerPose::REST);
            assert!(settled.layers[0].interactive);
        }
    }
}
