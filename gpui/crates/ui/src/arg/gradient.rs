//! The gradient-stops editor: an ordered set of `(t, color)` stops drawn as a
//! bar, stops draggable along it.
//!
//! # The order is the type's, not the caller's
//!
//! [`Gradient`] owns the invariants — stops sorted by `t`, positions in
//! `0..=1`, never fewer than two — and every mutator preserves them, so no
//! host ever sorts, clamps, or index-juggles. The web reference re-sorts after
//! every pointer move and then hunts for where its dragged stop went; here
//! [`Gradient::move_stop`] instead clamps a drag between its neighbours, so a
//! stop can never cross another and **indices are stable for the whole drag**.
//! That is a deliberate divergence: the resort dance is exactly the
//! change-amplification the model exists to absorb.
//!
//! # The bar is exact, not sampled
//!
//! gpui draws 2-stop gradients only, and a gradient between adjacent stops is
//! precisely 2-stop — so the bar is flat before the first stop, one gradient
//! segment per adjacent pair, flat after the last. No approximation.

use gpui::prelude::*;
use gpui::{
    div, linear_color_stop, linear_gradient, px, App, ElementId, Rgba, SharedString, Stateful,
    Window,
};

use crate::drag::DragGhost;
use crate::ladder;
use crate::node::{Instrument, Role};

use super::{bounds_probe, drag_fraction, fraction_of, OwnedDrag};
use crate::CONTROL_HEIGHT;

/// One stop: a position along the bar and the color there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// `0..=1` along the bar. The [`Gradient`] holding this stop keeps it in
    /// range and in order.
    pub t: f32,
    pub color: Rgba,
}

/// The ordered stop set. Constructed through [`Gradient::new`], which is where
/// "sorted, clamped, at least two" becomes true and the mutators keep it true.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    stops: Vec<GradientStop>,
}

/// The pair a stopless construction falls back to — black to white, the web
/// side's fallback.
fn fallback() -> [GradientStop; 2] {
    [
        GradientStop {
            t: 0.,
            color: gpui::black().into(),
        },
        GradientStop {
            t: 1.,
            color: gpui::white().into(),
        },
    ]
}

impl Gradient {
    /// Adopt stops from anywhere — a JSON arg, a node param — sorting and
    /// clamping them, and padding to two with the fallback if given fewer.
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
        let mut fill = fallback().into_iter();
        while stops.len() < 2 {
            // Padding goes where its `t` says; re-sort the (tiny) list after.
            stops.push(fill.next().expect("fallback covers a two-stop deficit"));
            stops.sort_by(|a, b| a.t.total_cmp(&b.t));
        }
        Self { stops }
    }

    #[must_use]
    pub fn stops(&self) -> &[GradientStop] {
        &self.stops
    }

    /// The interpolated color at `t`: flat past either end, linear between the
    /// stops that bracket it.
    #[must_use]
    pub fn color_at(&self, t: f32) -> Rgba {
        let t = t.clamp(0., 1.);
        let first = self.stops.first().expect("a gradient has two stops");
        let last = self.stops.last().expect("a gradient has two stops");
        if t <= first.t {
            return first.color;
        }
        if t >= last.t {
            return last.color;
        }
        for pair in self.stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if t <= b.t {
                let span = b.t - a.t;
                let mix = if span > 0. { (t - a.t) / span } else { 0. };
                return Rgba {
                    r: a.color.r + (b.color.r - a.color.r) * mix,
                    g: a.color.g + (b.color.g - a.color.g) * mix,
                    b: a.color.b + (b.color.b - a.color.b) * mix,
                    a: a.color.a + (b.color.a - a.color.a) * mix,
                };
            }
        }
        last.color
    }

    /// Add a stop at `t` carrying the gradient's own color there — a new stop
    /// is invisible until dragged, which is what clicking a bar should do.
    /// Returns its index.
    pub fn insert(&mut self, t: f32) -> usize {
        let t = t.clamp(0., 1.);
        let stop = GradientStop {
            t,
            color: self.color_at(t),
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

    /// Remove a stop — unless that would leave fewer than two, in which case
    /// nothing happens and the caller is told. A gradient of one stop is a
    /// swatch; the type refuses to become one.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.stops.len() <= 2 {
            return false;
        }
        self.stops.remove(index);
        true
    }
}

/// What the bar tells its host. Colors are edited via
/// [`Gradient::set_color`] against the selection the host keeps — the bar
/// itself has no picker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientEvent {
    /// A stop handle was pressed.
    Select(usize),
    /// A handle is being dragged; apply via [`Gradient::move_stop`].
    Move { index: usize, t: f32 },
    /// The bar was clicked between handles; apply via [`Gradient::insert`].
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

/// The bar, one control row tall: the exact gradient as fill, a handle per
/// stop, click-to-add in the gaps.
pub fn luma_gradient_bar(
    id: impl Into<SharedString>,
    gradient: &Gradient,
    selected: Option<usize>,
    width: f32,
    on_event: impl Fn(GradientEvent, &mut Window, &mut App) + Clone + 'static,
) -> Stateful<gpui::Div> {
    let id = id.into();
    let stops = gradient.stops();
    let (bounds, probe) = bounds_probe();

    // Flat lead-in, one 2-stop segment per adjacent pair, flat tail.
    let mut segments: Vec<gpui::AnyElement> = Vec::with_capacity(stops.len() + 1);
    let first = stops.first().expect("a gradient has two stops");
    let last = stops.last().expect("a gradient has two stops");
    if first.t > 0. {
        segments.push(
            div()
                .h_full()
                .w(gpui::relative(first.t))
                .bg(first.color)
                .into_any_element(),
        );
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
                ))
                .into_any_element(),
        );
    }
    if last.t < 1. {
        segments.push(
            div()
                .h_full()
                .w(gpui::relative(1. - last.t))
                .bg(last.color)
                .into_any_element(),
        );
    }

    let moved = id.clone();
    let on_move = on_event.clone();
    let on_add = on_event.clone();
    let add_bounds = bounds.clone();

    let handles = stops.iter().enumerate().map(|(index, stop)| {
        let press = on_event.clone();
        let is_selected = selected == Some(index);
        div()
            .id(ElementId::Name(format!("{id}:stop:{index}").into()))
            .absolute()
            .left(gpui::relative(stop.t))
            .top_0()
            .ml(px(-4.))
            .w(px(8.))
            .h_full()
            .flex()
            .justify_center()
            .cursor_ew_resize()
            // The hairline is what keeps a handle legible over *any* gradient
            // color: the fill alone inverts — the accent vanishes over a
            // matching section exactly when selection should read loudest.
            .child(
                div()
                    .w(px(4.))
                    .h_full()
                    .border_1()
                    .border_color(ladder::control_border())
                    .bg(if is_selected {
                        ladder::primary()
                    } else {
                        gpui::white().into()
                    }),
            )
            .on_mouse_down(gpui::MouseButton::Left, {
                let press = press.clone();
                move |_, window, cx| {
                    cx.stop_propagation();
                    press(GradientEvent::Select(index), window, cx);
                }
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
    });

    div()
        .id(ElementId::Name(format!("{moved}:bar").into()))
        .relative()
        .flex()
        .flex_shrink_0()
        .w(px(width))
        .h(px(CONTROL_HEIGHT))
        .border_1()
        .border_color(ladder::control_border())
        .overflow_hidden()
        .children(segments)
        .child(probe)
        .children(handles)
        .on_click(move |event, window, cx| {
            if let Some(t) = fraction_of(&add_bounds, event.position().x) {
                on_add(GradientEvent::Add { t }, window, cx);
            }
        })
        .on_drag_move(drag_fraction(
            moved,
            move |at, drag: &StopDrag, window, cx| {
                on_move(
                    GradientEvent::Move {
                        index: drag.index,
                        t: at.x,
                    },
                    window,
                    cx,
                );
            },
        ))
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
    /// clamp, and fewer than two stops pads with the fallback pair.
    #[test]
    fn construction_establishes_the_invariants() {
        let g = Gradient::new([stop(0.9, 1.), stop(-0.5, 0.), stop(0.4, 0.5)]);
        assert_eq!(ts(&g), vec![0., 0.4, 0.9]);
        assert_sorted(&g);

        assert_eq!(Gradient::new([]).stops().len(), 2);
        let padded = Gradient::new([stop(0.5, 1.)]);
        assert_eq!(padded.stops().len(), 2);
        assert_sorted(&padded);
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
        let index = g.insert(0.25);
        assert_eq!(index, 1);
        assert_sorted(&g);
        assert!((g.stops()[1].color.r - 0.25).abs() < 1e-6);
    }

    /// Two stops are the floor: removal below it is refused, above it works.
    #[test]
    fn removal_stops_at_two() {
        let mut g = Gradient::new([stop(0., 0.), stop(0.5, 0.5), stop(1., 1.)]);
        assert!(g.remove(1));
        assert!(!g.remove(0));
        assert_eq!(g.stops().len(), 2);
    }

    /// The sampler: flat past the ends, linear between.
    #[test]
    fn color_at_interpolates() {
        let g = Gradient::new([stop(0.25, 0.), stop(0.75, 1.)]);
        assert_eq!(g.color_at(0.).r, 0.);
        assert_eq!(g.color_at(1.).r, 1.);
        assert!((g.color_at(0.5).r - 0.5).abs() < 1e-6);
    }
}
