//! The gradient value: an ordered set of `(t, color)` stops, and its exact
//! fill. [`super::strip::CurveStrip`] edits it.
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
//! Stops are light colors, linear Rec. 2020; the bar shows each end mapped
//! into sRGB ([`Light::display`]).

pub use super::color::Light;
use gpui::prelude::*;
use gpui::{div, linear_color_stop, linear_gradient, px};
use luma_patterns::color_space;

/// One stop: a position along the bar and the color there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// `0..=1` along the bar. The [`Gradient`] holding this stop keeps it in
    /// range and in order.
    pub t: f32,
    pub color: Light,
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
    pub fn color_at(&self, t: f32) -> Light {
        let t = t.clamp(0., 1.);
        let (Some(first), Some(last)) = (self.stops.first(), self.stops.last()) else {
            return Light::BLACK;
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
        Light {
            rgb: color_space::interpolate(a.color.channels(), b.color.channels(), f64::from(mix))
                .map(|v| v as f32),
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
                Light::WHITE
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

    pub fn set_color(&mut self, index: usize, color: Light) {
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

/// The exact gradient as a row of fills, to lay in a flex row the height of
/// the fill: a flat lead-in, one two-stop segment per adjacent pair, a flat
/// tail. The end segments take `radius` on their outer corners, since a
/// content mask does not clip children to a rounded parent.
pub fn gradient_fill(gradient: &Gradient, radius: f32) -> Vec<gpui::Div> {
    let stops = gradient.stops();
    let mut segments: Vec<gpui::Div> = Vec::with_capacity(stops.len() + 1);
    if let Some(first) = stops.first().filter(|first| first.t > 0.) {
        segments.push(
            div()
                .h_full()
                .w(gpui::relative(first.t))
                .bg(first.color.display()),
        );
    }
    for pair in stops.windows(2) {
        segments.push(
            div()
                .h_full()
                .w(gpui::relative(pair[1].t - pair[0].t))
                .bg(linear_gradient(
                    90.,
                    linear_color_stop(pair[0].color.display(), 0.),
                    linear_color_stop(pair[1].color.display(), 1.),
                )
                .color_space(gpui::ColorSpace::Oklab)),
        );
    }
    if let Some(last) = stops.last().filter(|last| last.t < 1.) {
        segments.push(
            div()
                .h_full()
                .w(gpui::relative(1. - last.t))
                .bg(last.color.display()),
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(t: f32, r: f32) -> GradientStop {
        GradientStop {
            t,
            color: Light {
                rgb: [r, 0., 0.],
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
            assert!((g.color_at(t).rgb[0] - before.color_at(t).rgb[0]).abs() < 1e-5);
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
        assert_eq!(g.color_at(0.5), Light::BLACK);
        assert_eq!(g.insert(0.5), 0);
        assert_eq!(g.stops().len(), 1);
        assert_eq!(g.color_at(0.5), Light::WHITE);
    }

    /// The sampler: flat past the ends, and between them the engine's own
    /// perceptual blend, so the bar shows what plays.
    #[test]
    fn color_at_interpolates() {
        let g = Gradient::new([stop(0.25, 0.), stop(0.75, 1.)]);
        assert_eq!(g.color_at(0.).rgb[0], 0.);
        assert_eq!(g.color_at(1.).rgb[0], 1.);
        let engine = color_space::interpolate([0.; 3], [1., 0., 0.], 0.5);
        assert!((f64::from(g.color_at(0.5).rgb[0]) - engine[0]).abs() < 1e-6);
    }

    /// A color sRGB cannot show is painted as the nearest one it can, never
    /// with a channel out of range.
    #[test]
    fn a_wide_color_displays_in_range() {
        let green = Light {
            rgb: [0., 1., 0.],
            a: 1.,
        };
        let shown = green.display();
        assert!([shown.r, shown.g, shown.b]
            .iter()
            .all(|v| (0. ..=1.).contains(v)));
        assert!(shown.g > 0.8 && shown.r < 0.5 && shown.b < 0.5, "{shown:?}");
    }

    #[test]
    fn coincident_stops_select_the_color_after_the_jump() {
        let g = Gradient::new([stop(0., 0.), stop(0.5, 0.), stop(0.5, 1.), stop(1., 1.)]);
        assert_eq!(g.color_at(0.499).rgb[0], 0.);
        assert_eq!(g.color_at(0.5).rgb[0], 1.);
    }
}
