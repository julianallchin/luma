//! Laser elements on a zone's canvas: what a clip's elements draw, and how a
//! layer of elements combines with the elements under it.
//!
//! A laser clip draws figures, each `count` elements of one kind: beams,
//! lines, rings or polygons. On each projector it selects the figures'
//! elements are heads `projector:0` up, figure after figure.
//! An element's place along its clip's order is `s`, 0 to 1, which never
//! depends on how many elements exist, so a morph can follow elements of any
//! count. Within an element, `along` runs 0 to 1 from a line's start to its
//! end, or once round a ring; an input that reads it is sampled at
//! [`ALONG_SAMPLES`] places and read between them as finely as the solver
//! draws. Canvas coordinates run from −1 to 1 across and up a zone; a solver
//! turns them into directions and then into galvo angles.
use crate::runtime::EvaluatedValue;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// The graph output with the alpha and samples of each element of figure
/// `figure`, the figure's place in the laser's `figures`.
pub fn element_output(figure: usize) -> String {
    format!("element{figure}")
}
/// Channels of one sample of [`element_output`]: x, y, the four of the
/// element's frame, r, g, b.
pub const SAMPLE_CHANNELS: usize = 9;
/// How many places along an element an input that reads `along` is sampled
/// at, ends included.
pub const ALONG_SAMPLES: usize = 32;

/// What a figure's elements are: its `kind` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Points, in a row across the canvas by default.
    Beams,
    /// Straight lines.
    Line,
    /// Circles.
    Ring,
    /// Regular polygons of `sides` sides.
    Polygon,
    /// Points, in rows and columns by default.
    Grid,
}

impl Kind {
    pub fn from_setting(setting: Option<&str>) -> Self {
        match setting {
            Some("line") => Kind::Line,
            Some("ring") => Kind::Ring,
            Some("polygon") => Kind::Polygon,
            Some("grid") => Kind::Grid,
            _ => Kind::Beams,
        }
    }

    /// The code the laser kernel reads.
    pub fn code(self) -> f64 {
        match self {
            Kind::Beams => 0.,
            Kind::Line => 1.,
            Kind::Ring => 2.,
            Kind::Polygon => 3.,
            Kind::Grid => 4.,
        }
    }

    pub fn from_code(code: f64) -> Self {
        match code as u8 {
            1 => Kind::Line,
            2 => Kind::Ring,
            3 => Kind::Polygon,
            4 => Kind::Grid,
            _ => Kind::Beams,
        }
    }

    /// Whether each element is a single point: beams and a grid.
    pub fn is_point(self) -> bool {
        matches!(self, Kind::Beams | Kind::Grid)
    }

    /// How many elements a figure with no `count` draws on each projector.
    pub fn default_count(self) -> usize {
        if self.is_point() {
            8
        } else {
            1
        }
    }

    /// Element `index` of `count`'s home place at size 1, rotation 0: beams
    /// span the canvas in a row, a grid is as square as the count allows,
    /// and anything else sits in the middle. An element whose `x` or `y` is
    /// empty sits there, the layout scaled by `size` and turned by
    /// `rotation`.
    pub fn home(self, index: usize, count: usize) -> [f64; 2] {
        let spread = |i: usize, n: usize| {
            if n > 1 {
                2. * i as f64 / (n - 1) as f64 - 1.
            } else {
                0.
            }
        };
        match self {
            Kind::Beams => [spread(index, count), 0.],
            Kind::Grid => {
                let columns = (count as f64).sqrt().ceil().max(1.) as usize;
                let rows = count.div_ceil(columns);
                [
                    spread(index % columns, columns),
                    -spread(index / columns, rows),
                ]
            }
            Kind::Line | Kind::Ring | Kind::Polygon => [0., 0.],
        }
    }

    /// What an element of this kind draws.
    pub fn outline(self, sides: usize) -> Outline {
        match self {
            Kind::Beams | Kind::Grid => Outline::Point,
            Kind::Line => Outline::Line,
            Kind::Ring => Outline::Ring,
            Kind::Polygon => Outline::Polygon(sides.clamp(MIN_SIDES, MAX_SIDES) as u32),
        }
    }
}

/// The fewest and most sides a polygon has.
pub const MIN_SIDES: usize = 3;
pub const MAX_SIDES: usize = 12;

/// `size` and `rotation` as a frame: the outline scaled by
/// `size` and turned `rotation` degrees clockwise, as `[[a, b], [c, d]]`
/// taking `(u, v)` to `(a u + b v, c u + d v)`.
pub fn frame(size: f64, rotation: f64) -> [[f64; 2]; 2] {
    let (sin, cos) = rotation.to_radians().sin_cos();
    [[size * cos, size * sin], [-size * sin, size * cos]]
}

/// `frame` applied to `(u, v)`.
pub fn apply(frame: [[f64; 2]; 2], [u, v]: [f64; 2]) -> [f64; 2] {
    [
        frame[0][0] * u + frame[0][1] * v,
        frame[1][0] * u + frame[1][1] * v,
    ]
}

/// What one element draws, in its own unit frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outline {
    /// One point: the scanner dwells on it.
    Point,
    /// From (−1, 0) to (1, 0).
    Line,
    /// The unit circle, from the top clockwise.
    Ring,
    /// A regular polygon on the unit circle with a corner at the top.
    Polygon(u32),
}

impl Outline {
    /// The point at `along`, 0 to 1.
    pub fn unit(self, along: f64) -> [f64; 2] {
        let round = |a: f64| {
            let turn = std::f64::consts::TAU * a;
            [turn.sin(), turn.cos()]
        };
        match self {
            Outline::Point => [0., 0.],
            Outline::Line => [2. * along - 1., 0.],
            Outline::Ring => round(along),
            Outline::Polygon(sides) => {
                let sides = f64::from(sides.max(1));
                let at = along.clamp(0., 1.) * sides;
                let corner = at.floor().min(sides - 1.);
                let (a, b) = (round(corner / sides), round((corner + 1.) / sides));
                let t = at - corner;
                [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
            }
        }
    }

    /// The places along the outline where it turns a corner, ends included.
    pub fn corners(self) -> Vec<f64> {
        match self {
            Outline::Point => vec![0.],
            Outline::Line | Outline::Ring => vec![0., 1.],
            Outline::Polygon(sides) => (0..=sides)
                .map(|k| f64::from(k) / f64::from(sides))
                .collect(),
        }
    }
}

/// An element's place, frame and colour at one place along it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// The element's centre on the canvas.
    pub at: [f64; 2],
    /// Its size and turn ([`frame`]), and any mirror's reflection.
    pub frame: [[f64; 2]; 2],
    /// Its colour, each 0 to 1. Black is the light off.
    pub rgb: [f64; 3],
}

impl Sample {
    fn lerp(&self, other: &Sample, t: f64) -> Sample {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Sample {
            at: std::array::from_fn(|c| mix(self.at[c], other.at[c])),
            frame: std::array::from_fn(|r| {
                std::array::from_fn(|c| mix(self.frame[r][c], other.frame[r][c]))
            }),
            rgb: std::array::from_fn(|c| mix(self.rgb[c], other.rgb[c])),
        }
    }
}

/// One element after the layers under it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    /// The projector fixture's id.
    pub projector: String,
    /// The zone's id; empty is the venue's first zone.
    pub zone: String,
    /// The layer that drew it.
    pub layer: u32,
    pub outline: Outline,
    /// Its place along its clip's order, 0 to 1.
    pub s: f64,
    /// Its samples along it, evenly from 0 to 1; one when nothing reads
    /// `along`.
    pub samples: Vec<Sample>,
    /// How much of it shows, 0 to 1. A clip's fresh element carries its
    /// alpha here.
    pub intensity: f64,
}

impl Element {
    /// Each head's alpha and samples at each sample of `times`, read from
    /// [`element_output`]: one row per time, one entry per head of `ids`.
    pub fn read(
        value: &EvaluatedValue,
        ids: &[String],
        times: usize,
    ) -> Result<Vec<Vec<(f64, Vec<Sample>)>>> {
        let signal = value
            .signal()
            .filter(|signal| {
                let width = signal.values().dim().2;
                width > 1 && (width - 1) % SAMPLE_CHANNELS == 0
            })
            .ok_or_else(|| Error("a laser graph gives an alpha and samples per element".into()))?;
        let samples = (signal.values().dim().2 - 1) / SAMPLE_CHANNELS;
        let rows = ids
            .iter()
            .map(|id| match signal.fixtures() {
                None => Ok(0),
                Some(heads) => heads
                    .iter()
                    .position(|head| head == id)
                    .ok_or_else(|| Error(format!("the laser output is missing element {id}"))),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((0..times)
            .map(|t| {
                rows.iter()
                    .map(|&n| {
                        let v = |ch: usize| signal.at(n, t, ch);
                        let samples = (0..samples)
                            .map(|k| {
                                let c = 1 + k * SAMPLE_CHANNELS;
                                Sample {
                                    at: [v(c), v(c + 1)],
                                    frame: [[v(c + 2), v(c + 3)], [v(c + 4), v(c + 5)]],
                                    rgb: [v(c + 6), v(c + 7), v(c + 8)],
                                }
                            })
                            .collect();
                        (v(0), samples)
                    })
                    .collect()
            })
            .collect())
    }

    /// Its sample at `along`, read between the two around it.
    pub fn sample(&self, along: f64) -> Sample {
        let last = self.samples.len().saturating_sub(1);
        if last == 0 {
            return self.samples[0];
        }
        let at = along.clamp(0., 1.) * last as f64;
        let below = (at.floor() as usize).min(last - 1);
        self.samples[below].lerp(&self.samples[below + 1], at - below as f64)
    }

    /// Its canvas point and colour at `along`, colour at its intensity.
    pub fn point(&self, along: f64) -> ([f64; 2], [f64; 3]) {
        let sample = self.sample(along);
        let [u, v] = apply(sample.frame, self.outline.unit(along));
        let shown = self.intensity.clamp(0., 1.);
        (
            [sample.at[0] + u, sample.at[1] + v],
            sample.rgb.map(|c| c.clamp(0., 1.) * shown),
        )
    }

    /// The places along it a drawing must pass through: its corners and
    /// the places its inputs were sampled at, in order.
    pub fn breaks(&self) -> Vec<f64> {
        if self.outline == Outline::Point {
            return vec![0.];
        }
        let last = self.samples.len().saturating_sub(1).max(1);
        let mut breaks: Vec<f64> = self
            .outline
            .corners()
            .into_iter()
            .chain((0..=last).map(|k| k as f64 / last as f64))
            .collect();
        breaks.sort_by(f64::total_cmp);
        breaks.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        breaks
    }

    fn surface(&self) -> (&str, &str) {
        (&self.projector, &self.zone)
    }
}

/// Element `index` of `count`'s place along its clip's order, 0 to 1. A lone
/// element sits in the middle.
pub fn order(index: usize, count: usize) -> f64 {
    if count > 1 {
        index as f64 / (count - 1) as f64
    } else {
        0.5
    }
}

/// The elements of one projector and zone, sorted along their order, read
/// as one surface over `s` and along: a sample and an intensity at any `s`
/// by linear interpolation, held flat past either end.
struct Curve<'a> {
    elements: Vec<&'a Element>,
}

impl<'a> Curve<'a> {
    fn new(elements: impl IntoIterator<Item = &'a Element>) -> Option<Self> {
        let mut elements: Vec<&Element> = elements.into_iter().collect();
        if elements.is_empty() {
            return None;
        }
        elements.sort_by(|a, b| a.s.total_cmp(&b.s));
        Some(Curve { elements })
    }

    /// The sample at `along` and the intensity at `s`.
    fn at(&self, s: f64, along: f64) -> (Sample, f64) {
        let value = |e: &Element| (e.sample(along), e.intensity);
        let after = self.elements.partition_point(|e| e.s < s);
        match (after.checked_sub(1), self.elements.get(after)) {
            (None, Some(first)) => value(first),
            (Some(last), None) => value(self.elements[last]),
            (Some(below), Some(above)) => {
                let (a, b) = (self.elements[below], *above);
                let t = if b.s > a.s {
                    (s - a.s) / (b.s - a.s)
                } else {
                    0.
                };
                let ((a, ai), (b, bi)) = (value(a), value(b));
                (a.lerp(&b, t), ai + (bi - ai) * t)
            }
            (None, None) => unreachable!("a curve has an element"),
        }
    }
}

/// The places along `element` its samples sit at.
fn places(element: &Element) -> impl Iterator<Item = f64> {
    let last = element.samples.len().saturating_sub(1).max(1);
    (0..element.samples.len()).map(move |k| k as f64 / last as f64)
}

/// Draw `top` over `under` as well: an Add layer.
pub fn add(under: &mut Vec<Element>, top: Vec<Element>) {
    under.extend(top);
}

/// Lay `top` over `under` as a Replace layer: on each projector and zone the
/// top layer draws, the elements under it and the top's elements morph into
/// each other by the top's alpha, matched by their place along their order
/// and along each element. Under elements move toward the top's and fade by
/// `1 − alpha`; top elements start on the ones under them and show at
/// `alpha`. Each keeps its own outline and colour: an element is never a
/// blend of two colours. At alpha 1 only the top's elements show.
/// Elements on a projector or zone the top layer does not draw stay as they
/// are.
pub fn replace(under: &mut Vec<Element>, mut top: Vec<Element>) {
    let surfaces: Vec<(String, String)> = {
        let mut seen: Vec<(String, String)> = Vec::new();
        for element in &top {
            let key = (element.projector.clone(), element.zone.clone());
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
        seen
    };
    for (projector, zone) in &surfaces {
        let on = |e: &&Element| e.surface() == (projector.as_str(), zone.as_str());
        let below: Vec<Element> = under.iter().filter(on).cloned().collect();
        let above: Vec<Element> = top.iter().filter(on).cloned().collect();
        let below = Curve::new(&below);
        let above = Curve::new(&above).expect("a surface the top draws has elements");
        for element in under.iter_mut().filter(|e| on(&&**e)) {
            let alpha = above.at(element.s, 0.).1;
            let samples = places(element)
                .zip(&element.samples)
                .map(|(along, own)| Sample {
                    rgb: own.rgb,
                    ..own.lerp(&above.at(element.s, along).0, alpha)
                })
                .collect();
            element.samples = samples;
            element.intensity *= 1. - alpha;
        }
        if let Some(below) = below {
            for element in top.iter_mut().filter(|e| on(&&**e)) {
                let alpha = element.intensity;
                let samples = places(element)
                    .zip(&element.samples)
                    .map(|(along, own)| Sample {
                        rgb: own.rgb,
                        ..below.at(element.s, along).0.lerp(own, alpha)
                    })
                    .collect();
                element.samples = samples;
            }
        }
    }
    under.retain(|element| element.intensity > 0.);
    under.extend(top);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(layer: u32, s: f64, x: f64, intensity: f64) -> Element {
        Element {
            projector: "p".into(),
            zone: String::new(),
            layer,
            outline: Outline::Point,
            s,
            samples: vec![Sample {
                at: [x, 0.],
                frame: [[0.; 2]; 2],
                rgb: [1.; 3],
            }],
            intensity,
        }
    }

    #[test]
    fn beams_span_the_canvas_and_a_lone_one_sits_in_the_middle() {
        assert_eq!(Kind::Beams.home(0, 5), [-1., 0.]);
        assert_eq!(Kind::Beams.home(4, 5), [1., 0.]);
        assert_eq!(Kind::Beams.home(0, 1), [0., 0.]);
        assert_eq!(order(0, 1), 0.5);
        assert_eq!(order(3, 4), 1.);
    }

    #[test]
    fn a_grid_is_as_square_as_the_count_allows() {
        let places: Vec<[f64; 2]> = (0..9).map(|i| Kind::Grid.home(i, 9)).collect();
        assert_eq!(places[0], [-1., 1.]);
        assert_eq!(places[4], [0., 0.]);
        assert_eq!(places[8], [1., -1.]);
    }

    #[test]
    fn a_ring_and_a_polygon_stay_on_the_unit_circle_at_their_corners() {
        for k in 0..=12 {
            let [u, v] = Outline::Ring.unit(k as f64 / 12.);
            assert!((u.hypot(v) - 1.).abs() < 1e-12);
        }
        let square = Outline::Polygon(4);
        for corner in square.corners() {
            let [u, v] = square.unit(corner);
            assert!((u.hypot(v) - 1.).abs() < 1e-12, "{corner}");
        }
        // Between corners a polygon runs straight, inside the circle.
        let [u, v] = square.unit(0.125);
        assert!(u.hypot(v) < 0.99);
    }

    #[test]
    fn rotation_turns_a_line_upright() {
        let [x, y] = apply(frame(1., 90.), Outline::Line.unit(1.));
        assert!(x.abs() < 1e-12 && (y + 1.).abs() < 1e-12, "{x} {y}");
    }

    /// An input sampled along an element reads between its samples, so a
    /// line bent by `along` passes through each sample's place.
    #[test]
    fn an_element_reads_between_its_samples() {
        let mut line = element(0, 0.5, 0., 1.);
        line.outline = Outline::Line;
        line.samples = [0., 0.5, 0.]
            .into_iter()
            .map(|y| Sample {
                at: [0., y],
                frame: frame(1., 0.),
                rgb: [1.; 3],
            })
            .collect();
        assert_eq!(line.point(0.5).0, [0., 0.5]);
        assert_eq!(line.point(0.25).0, [-0.5, 0.25]);
        assert_eq!(line.breaks(), [0., 0.5, 1.]);
    }

    #[test]
    fn add_draws_both_layers() {
        let mut under = vec![element(0, 0.5, 0., 1.)];
        add(&mut under, vec![element(1, 0.5, 0.5, 0.5)]);
        assert_eq!(under.len(), 2);
    }

    /// Equal counts meet element for element: halfway through, each pair
    /// sits at the midpoint and their intensities sum to one element's.
    #[test]
    fn replace_morphs_equal_counts_element_for_element() {
        let under: Vec<Element> = (0..3).map(|i| element(0, order(i, 3), -1., 1.)).collect();
        let top: Vec<Element> = (0..3).map(|i| element(1, order(i, 3), 1., 0.5)).collect();
        let mut out = under;
        replace(&mut out, top);
        assert_eq!(out.len(), 6);
        for e in &out {
            assert!(e.samples[0].at[0].abs() < 1e-12, "{e:?}");
        }
        let total: f64 = out.iter().map(|e| e.intensity).sum();
        assert!((total - 3.).abs() < 1e-12);
    }

    /// A morph moves elements, never mixes their colours.
    #[test]
    fn replace_keeps_each_elements_colour() {
        let mut red = element(0, 0.5, -1., 1.);
        red.samples[0].rgb = [1., 0., 0.];
        let mut blue = element(1, 0.5, 1., 0.5);
        blue.samples[0].rgb = [0., 0., 1.];
        let mut out = vec![red];
        replace(&mut out, vec![blue]);
        let colours: Vec<[f64; 3]> = out.iter().map(|e| e.samples[0].rgb).collect();
        assert_eq!(colours, [[1., 0., 0.], [0., 0., 1.]]);
    }

    #[test]
    fn replace_at_full_alpha_leaves_only_the_top() {
        let mut out = vec![element(0, 0.2, -1., 1.), element(0, 0.8, -0.5, 1.)];
        replace(&mut out, vec![element(1, 0.5, 1., 1.)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].samples[0].at[0], 1.);
    }

    #[test]
    fn replace_leaves_other_projectors_alone() {
        let mut other = element(0, 0.5, -1., 1.);
        other.projector = "q".into();
        let mut out = vec![other.clone()];
        replace(&mut out, vec![element(1, 0.5, 1., 1.)]);
        assert!(out.contains(&other));
    }
}
