//! State-owned dialog route morphing.
//!
//! Route content owns only a [`RouteDescriptor`]. This reducer owns the live
//! container size and every painted content pose, so rebuilding the element
//! tree cannot restart a transition.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{px, AnyElement, SharedString};

use crate::motion::{self, SURFACE};
use crate::node::{Instrument as _, Role};
use crate::{glass, radius};

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

    fn interpolate(self, to: Self, progress: f32) -> Self {
        Self {
            width: motion::lerp(self.width, to.width, progress),
            height: motion::lerp(self.height, to.height, progress),
        }
    }
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
    /// Going deeper: the new page comes in from the right and the old one
    /// leaves to the left.
    Forward,
    /// Going back: the reverse of [`Self::Forward`].
    Back,
    Scale,
    CrossFade,
    Custom(TransitionEvaluator),
}

impl std::fmt::Debug for MorphTransition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Forward => formatter.write_str("Forward"),
            Self::Back => formatter.write_str("Back"),
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
            Self::Forward | Self::Back => {
                // The new page always arrives from the side it travels
                // toward; `sign` only picks which side that is. Slide and
                // fade only: a blur would send both pages through an
                // offscreen target and a gaussian pass on every frame.
                let sign = if matches!(self, Self::Forward) {
                    1.0
                } else {
                    -1.0
                };
                match role {
                    LayerRole::Outgoing => LayerPose {
                        x: -sign * 16.0 * progress,
                        opacity: 1.0 - progress,
                        ..LayerPose::REST
                    },
                    LayerRole::Incoming => LayerPose {
                        x: sign * 16.0 * (1.0 - progress),
                        opacity: progress,
                        ..LayerPose::REST
                    },
                }
            }
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

/// The slide for arriving at a route: [`MorphTransition::Forward`] into a
/// child page, [`MorphTransition::Back`] into the root.
///
/// The dialogs that use this have two levels, so the route alone gives the
/// direction.
#[must_use]
pub fn direction(child: bool) -> MorphTransition {
    if child {
        MorphTransition::Forward
    } else {
        MorphTransition::Back
    }
}

/// Child-owned route metadata; content itself remains outside the reducer.
#[derive(Clone, Debug)]
pub struct RouteDescriptor<K> {
    pub key: K,
    /// The size the shared container becomes for this route.
    pub size: MorphSize,
    pub transition: MorphTransition,
}

impl<K> RouteDescriptor<K> {
    #[must_use]
    pub fn exact(key: K, width: f32, height: f32) -> Self {
        Self {
            key,
            size: MorphSize::new(width, height),
            transition: MorphTransition::Forward,
        }
    }

    #[must_use]
    pub fn with_transition(mut self, transition: MorphTransition) -> Self {
        self.transition = transition;
        self
    }

    /// The route at a size a card can be painted at.
    fn normalized(mut self) -> Self {
        self.size = self.size.normalized();
        self
    }
}

#[derive(Clone, Debug)]
struct AnimatedLayer<K> {
    route: RouteDescriptor<K>,
    from: LayerPose,
    to: LayerPose,
    /// Whether this is the route the flight is settling on, as opposed to one
    /// of the copies it is animating away. Decided at [`MorphDialog::begin`],
    /// where a reversal may reuse a layer already on screen rather than adding
    /// one.
    target: bool,
}

#[derive(Clone, Debug)]
struct Flight<K> {
    layers: Vec<AnimatedLayer<K>>,
    from_size: MorphSize,
    target: RouteDescriptor<K>,
    started: Instant,
}

/// An immutable layer sample for the current frame.
#[derive(Clone, Debug)]
pub struct SampledLayer<K> {
    route: RouteDescriptor<K>,
    pub key: K,
    /// Stable layout size for this route. It never follows the live
    /// container interpolation, so responsive children cannot reflow in flight.
    pub size: MorphSize,
    pub pose: LayerPose,
    /// Only a committed target receives input. Every in-flight layer is a
    /// paint-only copy; this prevents focus or pointer admission before the
    /// geometry and target have atomically settled.
    pub interactive: bool,
    /// Whether this is the route the card is becoming, rather than one it is
    /// animating away. Distinct from [`Self::interactive`], which turns on only
    /// once the flight commits: the target is the target from the first frame,
    /// and it is the reason [`card`] may skip an invisible layer without ever
    /// skipping this one.
    pub target: bool,
}

#[derive(Clone, Debug)]
pub struct MorphSample<K> {
    pub size: MorphSize,
    pub layers: Vec<SampledLayer<K>>,
    pub progress: f32,
    pub animating: bool,
}

impl<K: Clone> MorphSample<K> {
    /// A settled sample holding one route at a fixed size.
    ///
    /// The seam that makes [`card`] the only card mechanism: a dialog that
    /// never morphs does not need a reducer, but it does need the same box, so
    /// it builds the sample directly instead of growing a second painter.
    #[must_use]
    pub fn fixed(key: K, size: MorphSize) -> Self {
        let route = RouteDescriptor {
            key: key.clone(),
            size: size.normalized(),
            transition: MorphTransition::CrossFade,
        };
        Self {
            size: route.size,
            layers: vec![SampledLayer {
                key,
                size: route.size,
                route,
                pose: LayerPose::REST,
                interactive: true,
                target: true,
            }],
            progress: 1.0,
            animating: false,
        }
    }
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

/// Below this a layer contributes nothing a person can see, and is skipped —
/// see [`card`]. Not zero: a crossfade spends several frames under 1%.
const MIN_VISIBLE_OPACITY: f32 = 0.01;

/// **The** dialog card.
///
/// Every modal in the app is one of these: a fixed-size dialog is a morph with
/// a single route ([`MorphSample::fixed`]), and a multi-route dialog is the
/// same card with more layers in flight. There is no second card mechanism,
/// which is what makes "what does a dialog card look like" a question with one
/// answer — the fill, the rim and the radius are written down here and nowhere
/// else. Routes supply content; they do not describe the box around it.
///
/// The child callback owns route content; this function owns the card's
/// surface, geometry and clipping, and [`layers`] the layer order, the
/// filtered poses and the target's input plane.
pub fn card<K>(
    sample: &MorphSample<K>,
    semantic_label: impl Into<SharedString>,
    content: impl FnMut(&K, ContentMode) -> AnyElement,
) -> AnyElement {
    gpui::div()
        .relative()
        .flex_none()
        .w(px(sample.size.width))
        .h(px(sample.size.height))
        .overflow_hidden()
        .rounded(px(radius::MODAL))
        .bg(glass::dialog())
        .children(layers(sample, content))
        .child(rim())
        .agent_node(Role::Card, semantic_label)
        .into_any_element()
}

/// The sample's route layers, each posed and placed in a box of
/// `sample.size`, for [`card`] to lay on the dialog surface.
fn layers<K>(
    sample: &MorphSample<K>,
    mut content: impl FnMut(&K, ContentMode) -> AnyElement,
) -> Vec<AnyElement> {
    sample
        .layers
        .iter()
        // A layer the eye cannot see still costs a full content build and, if
        // it is filtered, an offscreen texture and a gaussian pass — every
        // frame. The tail of a crossfade is mostly this, and only an *outgoing*
        // layer has such a tail, which is why the saving is taken there alone.
        //
        // Two layers are never skipped, invisible or not. An interactive one
        // owns the focus and the hitboxes. The target owns the content the card
        // is becoming, and it enters at opacity zero: skipping it would unmount
        // the incoming route for the first frames of every flight, so its
        // children would be built — and their element state seeded — a frame
        // late, and a replacement would briefly contain nothing arriving at all.
        .filter(|layer| {
            layer.interactive || layer.target || layer.pose.opacity > MIN_VISIBLE_OPACITY
        })
        .map(|layer| {
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
                return shell.occlude().child(content(key, mode)).into_any_element();
            }
            // `filtered` costs an offscreen render target and a blur pass
            // whatever its arguments say, so a pose that asks for neither must
            // not pay for both. A transition that only slides and fades — which
            // is every transition, once its blur has run out — paints straight.
            let identity = pose.blur <= 0.0 && (pose.scale - 1.0).abs() <= f32::EPSILON;
            if identity {
                shell.child(content(key, mode)).into_any_element()
            } else {
                shell
                    .child(super::filtered(
                        pose.blur,
                        pose.scale,
                        gpui::div().size_full().child(content(key, mode)),
                    ))
                    .into_any_element()
            }
        })
        .collect()
}

/// The card's hairline outline, painted as its **last** child.
///
/// Not `border_1()` on the card itself: every content layer above is
/// absolutely positioned across the card's whole box, and gpui paints a
/// border before its children — so a real border lands underneath the content
/// and disappears. Painting the rim after the layers is what puts an edge on a
/// surface whose interior is a stack of overlapping planes. It occludes
/// nothing and takes no layout, so the content it outlines still fills the
/// card edge to edge.
fn rim() -> gpui::Div {
    gpui::div()
        .absolute()
        .inset_0()
        .rounded(px(radius::MODAL))
        .border_1()
        .border_color(glass::hairline(0.10))
}

/// A dialog that never morphs: one route, one size, built once.
///
/// The seam that lets [`card`] be the *only* card mechanism. A fixed-size
/// dialog still wants the palette's fill, rim and radius, and before this it
/// got them from a second painter that had to be kept in step by hand. Here it
/// is simply a morph whose reducer has nothing to do.
///
/// `body` is already built, so the route callback hands it over once — a
/// settled sample has exactly one layer and asks exactly once.
pub fn fixed_card(label: impl Into<SharedString>, size: MorphSize, body: AnyElement) -> AnyElement {
    let sample = MorphSample::fixed((), size);
    let mut body = Some(body);
    card(&sample, label, move |(), _| {
        body.take()
            .unwrap_or_else(|| gpui::div().into_any_element())
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteRequest {
    Unchanged,
    Transitioning,
    Settled,
}

/// The one durable reducer for a morphing dialog instance.
#[derive(Clone, Debug)]
pub struct MorphDialog<K> {
    settled: RouteDescriptor<K>,
    flight: Option<Flight<K>>,
    focus_after_commit: Option<K>,
}

impl<K: Clone + Eq> MorphDialog<K> {
    #[must_use]
    pub fn new(route: RouteDescriptor<K>) -> Self {
        Self {
            settled: route.normalized(),
            flight: None,
            focus_after_commit: None,
        }
    }

    #[must_use]
    pub fn target_key(&self) -> &K {
        self.flight
            .as_ref()
            .map(|flight| &flight.target.key)
            .unwrap_or(&self.settled.key)
    }

    /// Request a route. Asking again for the route a flight is already
    /// settling on leaves that flight alone, so a caller may request on every
    /// render.
    pub fn request(
        &mut self,
        route: RouteDescriptor<K>,
        now: Instant,
        reduced_motion: bool,
    ) -> RouteRequest {
        let target = route.normalized();
        if !reduced_motion
            && self.flight.as_ref().is_some_and(|flight| {
                flight.target.key == target.key && flight.target.size == target.size
            })
        {
            return RouteRequest::Unchanged;
        }
        self.begin(target, now, reduced_motion)
    }

    fn begin(
        &mut self,
        target: RouteDescriptor<K>,
        now: Instant,
        reduced_motion: bool,
    ) -> RouteRequest {
        let current = self.sample(now);
        if !current.animating && self.settled.key == target.key && self.settled.size == target.size
        {
            return RouteRequest::Unchanged;
        }
        if reduced_motion {
            self.settled = target;
            self.flight = None;
            self.focus_after_commit = Some(self.settled.key.clone());
            return RouteRequest::Settled;
        }

        let transition = target.transition;
        let mut layers = Vec::with_capacity(current.layers.len() + 1);
        let mut reused_target = false;
        for layer in current.layers {
            let is_target = layer.key == target.key && layer.size == target.size;
            reused_target |= is_target;
            layers.push(AnimatedLayer {
                route: layer.route,
                from: layer.pose,
                to: if is_target {
                    LayerPose::REST
                } else {
                    transition.pose(LayerRole::Outgoing, 1.0)
                },
                target: is_target,
            });
        }
        if !reused_target {
            layers.push(AnimatedLayer {
                route: target.clone(),
                from: transition.pose(LayerRole::Incoming, 0.0),
                to: LayerPose::REST,
                target: true,
            });
        }
        self.flight = Some(Flight {
            layers,
            from_size: current.size,
            target,
            started: now,
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
                    key: self.settled.key.clone(),
                    size: self.settled.size,
                    pose: LayerPose::REST,
                    interactive: true,
                    target: true,
                }],
                progress: 1.0,
                animating: false,
            };
        };
        let progress = motion::exit_progress_at(&SURFACE, flight.started, now);
        let animating = now.saturating_duration_since(flight.started) < motion::span(&SURFACE);
        MorphSample {
            size: flight.from_size.interpolate(flight.target.size, progress),
            layers: flight
                .layers
                .iter()
                .map(|layer| SampledLayer {
                    route: layer.route.clone(),
                    key: layer.route.key.clone(),
                    size: layer.route.size,
                    pose: layer.from.interpolate(layer.to, progress),
                    interactive: false,
                    target: layer.target,
                })
                .collect(),
            progress,
            animating,
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
            self.focus_after_commit = Some(self.settled.key.clone());
            return false;
        }
        if self.sample(now).animating {
            return true;
        }
        let flight = self.flight.take().unwrap();
        self.settled = flight.target;
        self.focus_after_commit = Some(self.settled.key.clone());
        false
    }

    pub fn take_focus_after_commit(&mut self) -> Option<K> {
        self.focus_after_commit.take()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn route(key: &'static str, width: f32, height: f32) -> RouteDescriptor<&'static str> {
        RouteDescriptor::exact(key, width, height)
    }

    fn at(start: Instant, milliseconds: u64) -> Instant {
        start + Duration::from_millis(milliseconds)
    }

    /// How long a flight runs — the same span [`MorphDialog::sample`] reads,
    /// so these tests cannot drift from the reducer.
    fn span() -> Duration {
        motion::span(&SURFACE)
    }

    /// The instant a flight begun at `start` is `fraction` of the way through.
    ///
    /// A fraction rather than a wall-clock literal because the poses pinned
    /// below are statements about *the curve*, not about a duration: they stay
    /// true when the ladder rung moves, and a literal would quietly re-point
    /// them at a different place on the same curve.
    fn part(start: Instant, fraction: f32) -> Instant {
        start + span().mul_f32(fraction)
    }

    /// Past the end of a flight begun at `start`.
    fn settled(start: Instant) -> Instant {
        start + span() + Duration::from_millis(1)
    }

    /// A fixed-size dialog must be indistinguishable from a settled morph, or
    /// the two would drift into two cards again.
    #[test]
    fn a_fixed_sample_is_a_settled_single_route_morph() {
        let fixed = MorphSample::fixed("venues", MorphSize::new(680.0, 460.0));
        let settled = MorphDialog::new(route("venues", 680.0, 460.0)).sample(Instant::now());
        assert_eq!(fixed.size, settled.size);
        assert_eq!(fixed.progress, settled.progress);
        assert_eq!(fixed.animating, settled.animating);
        assert_eq!(fixed.layers.len(), settled.layers.len());
        assert_eq!(fixed.layers[0].key, settled.layers[0].key);
        assert_eq!(fixed.layers[0].size, settled.layers[0].size);
        assert_eq!(fixed.layers[0].pose, settled.layers[0].pose);
        assert!(fixed.layers[0].interactive);
        // A degenerate request still yields a paintable box.
        assert_eq!(
            MorphSample::fixed("x", MorphSize::new(0.0, -4.0)).size,
            MorphSize::new(1.0, 1.0)
        );
    }

    #[test]
    fn forward_pins_start_mid_and_end_on_both_axes_and_poses() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0));
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
        assert_eq!(opening.layers[1].pose.x, 16.0);
        assert_eq!(opening.layers[1].pose.opacity, 0.0);
        assert_eq!(opening.layers[1].pose.blur, 0.0);

        let middle = dialog.sample(part(start, 0.5));
        // The shared spring has covered 96.233% of the distance at half-time.
        assert!((middle.size.width - 592.466).abs() < 0.01);
        assert!((middle.size.height - 432.466).abs() < 0.01);
        assert!((middle.layers[0].pose.x - -15.397).abs() < 0.01);
        assert!((middle.layers[1].pose.x - 0.603).abs() < 0.01);
        assert_eq!(middle.layers[0].size, MorphSize::new(400.0, 240.0));
        assert_eq!(middle.layers[1].size, MorphSize::new(600.0, 440.0));

        assert!(!dialog.tick(settled(start), false));
        let end = dialog.sample(settled(start));
        assert_eq!(end.size, MorphSize::new(600.0, 440.0));
        assert_eq!(end.layers.len(), 1);
        assert_eq!(end.layers[0].pose, LayerPose::REST);
        assert!(end.layers[0].interactive);
        assert_eq!(dialog.take_focus_after_commit(), Some("b"));
    }

    #[test]
    fn reversal_and_replacement_anchor_every_visible_pose_and_rect() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0));
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

        let done = settled(replace);
        assert!(!dialog.tick(done, false));
        let settled = dialog.sample(done);
        assert_eq!(settled.size, MorphSize::new(520.0, 300.0));
        assert_eq!(settled.layers.len(), 1);
        assert_eq!(settled.layers[0].key, "c");
        assert_eq!(settled.layers[0].size, MorphSize::new(520.0, 300.0));
        assert!(settled.layers[0].interactive);
    }

    /// The incoming route enters at opacity zero, so [`card`]'s "skip what the
    /// eye cannot see" filter would drop it for the first frames of every
    /// flight unless it is marked. Exactly one layer carries the mark, whether
    /// the flight added it or a reversal reused one already on screen.
    #[test]
    fn the_route_a_flight_is_settling_on_is_always_marked_exactly_once() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0));
        fn targets<'k>(sample: &MorphSample<&'k str>) -> Vec<&'k str> {
            sample
                .layers
                .iter()
                .filter(|layer| layer.target)
                .map(|layer| layer.key)
                .collect()
        }
        assert_eq!(targets(&dialog.sample(start)), ["a"], "a settled route");

        // Replacement: the incoming layer is brand new and fully transparent.
        dialog.request(route("b", 600.0, 440.0), start, false);
        let opening = dialog.sample(start);
        assert_eq!(targets(&opening), ["b"]);
        let incoming = opening.layers.iter().find(|l| l.key == "b").unwrap();
        assert!(
            incoming.pose.opacity <= MIN_VISIBLE_OPACITY,
            "the case the mark exists for: {:?}",
            incoming.pose
        );

        // Reversal: the target is a layer already on screen, not a new one.
        let flip = at(start, 70);
        dialog.request(route("a", 400.0, 240.0), flip, false);
        assert_eq!(targets(&dialog.sample(flip)), ["a"]);

        // …and a third route replacing an in-flight pair still marks only it.
        let replace = at(flip, 40);
        dialog.request(route("c", 520.0, 300.0), replace, false);
        assert_eq!(targets(&dialog.sample(replace)), ["c"]);
    }

    #[test]
    fn reused_key_with_changed_exact_size_hands_off_before_commit() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0));
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

        let done = settled(reverse);
        assert!(!dialog.tick(done, false));
        let settled = dialog.sample(done);
        assert_eq!(settled.layers.len(), 1);
        assert_eq!(settled.layers[0].size, MorphSize::new(520.0, 320.0));
        assert_eq!(settled.layers[0].pose, LayerPose::REST);
    }

    #[test]
    fn interrupted_same_key_resize_preserves_each_in_flight_layout() {
        let start = Instant::now();
        let mut dialog = MorphDialog::new(route("a", 400.0, 240.0));
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

        let done = settled(interrupt);
        assert!(!dialog.tick(done, false));
        let settled = dialog.sample(done);
        assert_eq!(settled.layers.len(), 1);
        assert_eq!(settled.layers[0].key, "a");
        assert_eq!(settled.layers[0].size, MorphSize::new(600.0, 400.0));
        assert_eq!(settled.layers[0].pose, LayerPose::REST);
        assert!(settled.layers[0].interactive);
    }

    /// Turning reduced motion on mid-flight snaps to the target and hands it
    /// the focus, whether the next thing to happen is a request or a tick.
    #[test]
    fn reduced_motion_toggled_mid_flight_snaps_to_the_target() {
        let start = Instant::now();
        let mut requested = MorphDialog::new(route("a", 400.0, 240.0));
        requested.request(route("b", 600.0, 440.0), start, false);
        assert_eq!(
            requested.request(route("b", 600.0, 440.0), at(start, 60), true),
            RouteRequest::Settled
        );
        let snapped = requested.sample(at(start, 60));
        assert_eq!(snapped.size, MorphSize::new(600.0, 440.0));
        assert_eq!(snapped.layers.len(), 1);
        assert_eq!(snapped.layers[0].key, "b");
        assert!(snapped.layers[0].interactive);
        assert_eq!(requested.take_focus_after_commit(), Some("b"));

        let mut ticked = MorphDialog::new(route("a", 400.0, 240.0));
        ticked.request(route("b", 620.0, 420.0), at(start, 10), false);
        assert!(!ticked.tick(at(start, 70), true));
        let snapped = ticked.sample(at(start, 70));
        assert_eq!(snapped.size, MorphSize::new(620.0, 420.0));
        assert_eq!(snapped.layers.len(), 1);
        assert_eq!(snapped.layers[0].key, "b");
        assert!(snapped.layers[0].interactive);
        assert_eq!(ticked.take_focus_after_commit(), Some("b"));
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
            let mut dialog = MorphDialog::new(route("a", 400.0, 240.0));
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
                MorphTransition::Forward | MorphTransition::Back => unreachable!(),
            }
            let done = settled(start);
            assert!(!dialog.tick(done, false));
            let settled = dialog.sample(done);
            assert_eq!(settled.layers.len(), 1);
            assert_eq!(settled.layers[0].key, "b");
            assert_eq!(settled.layers[0].pose, LayerPose::REST);
            assert!(settled.layers[0].interactive);
        }
    }
}
