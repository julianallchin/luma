//! The gradient-stops editor: an ordered set of `(t, color)` stops drawn as a
//! bar, with a draggable color marker per stop under it.
//!
//! # The order is the type's, not the caller's
//!
//! [`Gradient`] owns the invariants — stops sorted by `t`, positions in
//! `0..=1`, including empty and single-color values — and every mutator preserves them, so no
//! host ever sorts, clamps, or index-juggles. [`Gradient::move_stop`] clamps a
//! drag between its neighbours, so a stop can never cross another and
//! **indices are stable for the whole drag**. No caller has to re-sort and
//! then search for the stop it was dragging.
//!
//! # The bar is exact, not sampled
//!
//! GPUI draws each adjacent pair in OKLab, matching the runtime's perceptual
//! interpolation. The bar is flat before the first stop and after the last.

use gpui::prelude::*;
use gpui::{
    div, linear_color_stop, linear_gradient, px, App, ElementId, Rgba, SharedString, Stateful,
    Window,
};

use crate::drag::DragGhost;
use crate::ladder;
use crate::node::{Instrument, Role};

use super::{bounds_probe, fraction_of, OwnedDrag};

/// One stop: a position along the bar and the color there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// `0..=1` along the bar. The [`Gradient`] holding this stop keeps it in
    /// range and in order.
    pub t: f32,
    pub color: Rgba,
}

/// The ordered stop set. Constructed through [`Gradient::new`], which is where
/// "sorted and clamped" becomes true and the mutators keep it true.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    stops: Vec<GradientStop>,
}

impl Gradient {
    /// Adopt stops from anywhere — a JSON arg, a node param — sorting and
    /// clamping them. Empty and single-color values retain their authored meaning.
    /// Total: there is no stop list this refuses.
    #[must_use]
    pub fn new(stops: impl IntoIterator<Item = GradientStop>) -> Self {
        let mut stops: Vec<GradientStop> = stops
            .into_iter()
            .map(|stop| GradientStop {
                t: stop.t.clamp(0., 1.),
                ..stop
            })
            .collect();
        stops.sort_by(|a, b| a.t.total_cmp(&b.t));
        Self { stops }
    }

    #[must_use]
    pub fn stops(&self) -> &[GradientStop] {
        &self.stops
    }

    /// The interpolated color at `t`: flat past either end, perceptual OKLab
    /// interpolation between stops, matching playback and the painted bar.
    #[must_use]
    pub fn color_at(&self, t: f32) -> Rgba {
        let t = t.clamp(0., 1.);
        let (Some(first), Some(last)) = (self.stops.first(), self.stops.last()) else {
            return gpui::black().into();
        };
        if t < first.t {
            return first.color;
        }
        if t >= last.t {
            return last.color;
        }
        let right = self.stops.partition_point(|stop| stop.t <= t);
        let (a, b) = (self.stops[right - 1], self.stops[right]);
        let mix = (t - a.t) / (b.t - a.t);
        let [r, g, blue] = luma_patterns::oklab::interpolate(
            [a.color.r, a.color.g, a.color.b],
            [b.color.r, b.color.g, b.color.b],
            mix,
        );
        Rgba {
            r,
            g,
            b: blue,
            a: a.color.a + (b.color.a - a.color.a) * mix,
        }
    }

    /// Add a stop at `t` carrying the gradient's own color there — a new stop
    /// is invisible until dragged, which is what clicking a bar should do.
    /// Returns its index.
    pub fn insert(&mut self, t: f32) -> usize {
        let t = t.clamp(0., 1.);
        let stop = GradientStop {
            t,
            color: if self.stops.is_empty() {
                gpui::white().into()
            } else {
                self.color_at(t)
            },
        };
        let index = self.stops.partition_point(|s| s.t <= t);
        self.stops.insert(index, stop);
        index
    }

    /// Drag `index` to `t`, clamped between its neighbours so stops never
    /// cross and the index stays true for the whole drag — see module docs.
    /// Returns the position actually applied.
    pub fn move_stop(&mut self, index: usize, t: f32) -> f32 {
        let floor = if index > 0 {
            self.stops[index - 1].t
        } else {
            0.
        };
        let ceil = self.stops.get(index + 1).map_or(1., |next| next.t);
        let t = t.clamp(0., 1.).clamp(floor, ceil);
        self.stops[index].t = t;
        t
    }

    pub fn set_color(&mut self, index: usize, color: Rgba) {
        self.stops[index].color = color;
    }

    /// Remove a stop, allowing the last color to be removed as well.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.stops.len() {
            return false;
        }
        self.stops.remove(index);
        true
    }
}

/// What the stops control tells its host. Colors are edited via
/// [`Gradient::set_color`] against the selection the host keeps — the bar
/// itself has no picker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientEvent {
    /// A stop marker was pressed.
    Select(usize),
    /// A marker is being dragged along the bar; apply via
    /// [`Gradient::move_stop`].
    Move { index: usize, t: f32 },
    /// A marker is being dragged off the bar (`off`), or back onto it. A stop
    /// released off the bar is removed.
    Detach { index: usize, off: bool },
    /// The pointer went up: a detached stop goes now.
    Release,
    /// A marker was right-clicked.
    Remove(usize),
    /// The bar was clicked; apply via [`Gradient::insert`].
    Add { t: f32 },
}

/// A stop drag in flight, routed by bar id like a slider's; the index is
/// stable across the drag because [`Gradient::move_stop`] cannot reorder.
#[derive(Clone)]
struct StopDrag {
    id: SharedString,
    index: usize,
}

impl OwnedDrag for StopDrag {
    fn owner(&self) -> &SharedString {
        &self.id
    }
}

/// The exact gradient as a row of fills, to lay in a flex row the height of
/// the fill: a flat lead-in, one two-stop segment per adjacent pair, a flat
/// tail. The end segments take `radius` on their outer corners, since a
/// content mask does not clip children to a rounded parent.
pub fn gradient_fill(gradient: &Gradient, radius: f32) -> Vec<gpui::Div> {
    let stops = gradient.stops();
    let mut segments: Vec<gpui::Div> = Vec::with_capacity(stops.len() + 1);
    if let Some(first) = stops.first().filter(|first| first.t > 0.) {
        segments.push(div().h_full().w(gpui::relative(first.t)).bg(first.color));
    }
    for pair in stops.windows(2) {
        segments.push(
            div()
                .h_full()
                .w(gpui::relative(pair[1].t - pair[0].t))
                .bg(linear_gradient(
                    90.,
                    linear_color_stop(pair[0].color, 0.),
                    linear_color_stop(pair[1].color, 1.),
                )
                .color_space(gpui::ColorSpace::Oklab)),
        );
    }
    if let Some(last) = stops.last().filter(|last| last.t < 1.) {
        segments.push(div().h_full().w(gpui::relative(1. - last.t)).bg(last.color));
    }
    let last_segment = segments.len().saturating_sub(1);
    segments
        .into_iter()
        .enumerate()
        .map(|(at, segment)| {
            segment
                .when(at == 0, |s| s.rounded_l(px(radius)))
                .when(at == last_segment, |s| s.rounded_r(px(radius)))
        })
        .collect()
}

/// How far past the control a dragged marker counts as off it.
const DETACH: f32 = 24.;
const BAR_H: f32 = 20.;
const MARKER: f32 = 12.;

/// The stops control: the exact gradient as a bar, one small color marker
/// per stop under it. Click the bar to add a stop; drag a marker to move it,
/// or off the control to remove it; right-click a marker to remove it.
/// `detached` is the stop being dragged off, drawn faded.
pub fn luma_gradient_stops(
    id: impl Into<SharedString>,
    gradient: &Gradient,
    selected: Option<usize>,
    detached: Option<usize>,
    on_event: impl Fn(GradientEvent, &mut Window, &mut App) + Clone + 'static,
) -> Stateful<gpui::Div> {
    let id = id.into();
    let (bounds, probe) = bounds_probe();
    let add = on_event.clone();
    let bar = div()
        .id(ElementId::Name(format!("{id}:fill").into()))
        .relative()
        .flex()
        .w_full()
        .h(px(BAR_H))
        .rounded(px(crate::radius::CONTROL))
        .border_1()
        .border_color(crate::glass::hairline(0.10))
        .bg(gpui::black())
        .overflow_hidden()
        .cursor_crosshair()
        .children(gradient_fill(gradient, crate::radius::CAP))
        .child(probe)
        .on_click(move |event, window, cx| {
            if let Some(t) = fraction_of(&bounds, event.position().x) {
                add(GradientEvent::Add { t }, window, cx);
            }
        })
        .agent_node(Role::Card, format!("{id} bar"));
    let markers = gradient
        .stops()
        .iter()
        .enumerate()
        .map(|(index, stop)| {
            let chosen = selected == Some(index);
            let press = on_event.clone();
            let remove = on_event.clone();
            div()
                .id(ElementId::Name(format!("{id}:stop:{index}").into()))
                .absolute()
                .left(gpui::relative(stop.t))
                .top(px(2.))
                .ml(px(-MARKER / 2.))
                .size(px(MARKER))
                .rounded(px(3.))
                .bg(Rgba {
                    a: 1.,
                    ..stop.color
                })
                .border_color(if chosen {
                    ladder::foreground()
                } else {
                    ladder::control_border()
                })
                .when(chosen, |marker| marker.border_2())
                .when(!chosen, |marker| marker.border_1())
                .when(detached == Some(index), |marker| marker.opacity(0.3))
                .cursor_ew_resize()
                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    press(GradientEvent::Select(index), window, cx);
                })
                .on_mouse_down(gpui::MouseButton::Right, move |_, window, cx| {
                    cx.stop_propagation();
                    remove(GradientEvent::Remove(index), window, cx);
                })
                .on_drag(
                    StopDrag {
                        id: id.clone(),
                        index,
                    },
                    |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| DragGhost)
                    },
                )
                .agent_node(Role::Slider, format!("{id}:stop:{index} = {}", stop.t))
        })
        .collect::<Vec<_>>();
    let owner = id.clone();
    let on_drag = on_event.clone();
    let release = on_event.clone();
    let release_out = on_event;
    div()
        .id(ElementId::Name(format!("{id}:bar").into()))
        .w_full()
        // Half a marker of air each side, so an end marker is whole.
        .px(px(MARKER / 2.))
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(bar)
        .child(
            div()
                .relative()
                .w_full()
                .h(px(MARKER + 4.))
                .children(markers),
        )
        .on_drag_move(move |event: &gpui::DragMoveEvent<StopDrag>, window, cx| {
            let drag = event.drag(cx);
            if drag.owner() != &owner {
                return;
            }
            let index = drag.index;
            let (b, at) = (event.bounds, event.event.position);
            let width = f32::from(b.size.width) - MARKER;
            if width <= 0. {
                return;
            }
            let off = at.y < b.top() - px(DETACH) || at.y > b.bottom() + px(DETACH);
            on_drag(GradientEvent::Detach { index, off }, window, cx);
            if !off {
                let t = ((f32::from(at.x - b.left()) - MARKER / 2.) / width).clamp(0., 1.);
                on_drag(GradientEvent::Move { index, t }, window, cx);
            }
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_, window, cx| {
            release(GradientEvent::Release, window, cx)
        })
        .on_mouse_up_out(gpui::MouseButton::Left, move |_, window, cx| {
            release_out(GradientEvent::Release, window, cx)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(t: f32, r: f32) -> GradientStop {
        GradientStop {
            t,
            color: Rgba {
                r,
                g: 0.,
                b: 0.,
                a: 1.,
            },
        }
    }

    fn ts(gradient: &Gradient) -> Vec<f32> {
        gradient.stops().iter().map(|s| s.t).collect()
    }

    fn assert_sorted(gradient: &Gradient) {
        assert!(
            gradient.stops().windows(2).all(|p| p[0].t <= p[1].t),
            "stops out of order: {:?}",
            ts(gradient)
        );
    }

    /// Construction is total: unsorted input sorts, out-of-range positions
    /// clamp, and empty or single-color values keep their meaning.
    #[test]
    fn construction_establishes_the_invariants() {
        let g = Gradient::new([stop(0.9, 1.), stop(-0.5, 0.), stop(0.4, 0.5)]);
        assert_eq!(ts(&g), vec![0., 0.4, 0.9]);
        assert_sorted(&g);

        assert_eq!(Gradient::new([]).stops().len(), 0);
        let single = Gradient::new([stop(0.5, 1.)]);
        assert_eq!(single.stops(), &[stop(0.5, 1.)]);
        for t in [-1., 0., 0.5, 1., 2.] {
            assert_eq!(single.color_at(t), stop(0.5, 1.).color);
        }
    }

    /// A drag clamps between its neighbours: order holds, the index stays
    /// true, and the applied position is reported back.
    #[test]
    fn a_moved_stop_cannot_cross_its_neighbours() {
        let mut g = Gradient::new([stop(0.2, 0.), stop(0.5, 0.5), stop(0.8, 1.)]);
        // Trying to drag the middle stop past both ends lands on each wall.
        assert_eq!(g.move_stop(1, 0.99), 0.8);
        assert_eq!(g.move_stop(1, 0.01), 0.2);
        assert_sorted(&g);
        // The ends clamp to the bar itself.
        assert_eq!(g.move_stop(0, -1.), 0.);
        assert_eq!(g.move_stop(2, 2.), 1.);
        assert_sorted(&g);
        // And a legal move just happens.
        assert_eq!(g.move_stop(1, 0.6), 0.6);
        assert_eq!(ts(&g), vec![0., 0.6, 1.]);
    }

    /// Insertion lands in order, carries the bar's own color at that point,
    /// and reports where it landed.
    #[test]
    fn insertion_keeps_order_and_samples_the_bar() {
        let mut g = Gradient::new([stop(0., 0.), stop(1., 1.)]);
        let before = g.clone();
        let inserted_color = g.color_at(0.25);
        let index = g.insert(0.25);
        assert_eq!(index, 1);
        assert_sorted(&g);
        assert_eq!(g.stops()[1].color, inserted_color);
        for t in [0.1, 0.5, 0.75, 0.9] {
            assert!((g.color_at(t).r - before.color_at(t).r).abs() < 1e-5);
        }
    }

    /// Clearing and refilling do not introduce synthetic fallback stops.
    #[test]
    fn removal_can_clear_a_palette_and_insertion_restores_one_color() {
        let mut g = Gradient::new([stop(0., 0.), stop(0.5, 0.5), stop(1., 1.)]);
        assert!(g.remove(1));
        assert!(g.remove(0));
        assert!(g.remove(0));
        assert!(!g.remove(0));
        assert!(g.stops().is_empty());
        assert_eq!(g.color_at(0.5), gpui::black().into());
        assert_eq!(g.insert(0.5), 0);
        assert_eq!(g.stops().len(), 1);
        assert_eq!(g.color_at(0.5), gpui::white().into());
    }

    /// The sampler: flat past the ends, perceptual between.
    #[test]
    fn color_at_interpolates() {
        let g = Gradient::new([stop(0.25, 0.), stop(0.75, 1.)]);
        assert_eq!(g.color_at(0.).r, 0.);
        assert_eq!(g.color_at(1.).r, 1.);
        assert!((g.color_at(0.5).r - 0.388573).abs() < 1e-5);
    }

    #[test]
    fn coincident_stops_select_the_color_after_the_jump() {
        let g = Gradient::new([stop(0., 0.), stop(0.5, 0.), stop(0.5, 1.), stop(1., 1.)]);
        assert_eq!(g.color_at(0.499).r, 0.);
        assert_eq!(g.color_at(0.5).r, 1.);
    }
}
