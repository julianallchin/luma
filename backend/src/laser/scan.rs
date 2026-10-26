//! The scan plan: one projector's elements become one point stream.
//!
//! A beam is the scanner dwelling on one direction; a line, ring or polygon
//! is the scanner moving with the light on. Between them, and over an
//! element's dark stretches, the light is off while the mirrors jump. The
//! frame aims for [`FRAME_RATE`] scans a second: the points the jumps and
//! the lit strokes need come first, and what is left of the budget is
//! shared out as dwell on the beams. A frame with more to draw than the
//! budget runs long, and so refreshes slower, as a real projector would.
//!
//! Brightness is power times time: a projector that splits a frame over
//! sixteen beams gives each a sixteenth of the time, and so a sixteenth of
//! the light. Nothing here hides that.
use super::{LaserRig, Projector};
use crate::models::universe::UniverseState;
use luma_patterns::laser::{Element, Outline};
use serde::{Deserialize, Serialize};

/// Scans a second a frame aims for.
pub const FRAME_RATE: u32 = 30;
/// The longest mirror move between two points, in galvo units, with the light
/// off.
const BLANK_STEP: f64 = 0.08;
/// Points spent still at either end of a jump, so the mirrors settle before
/// the light turns on and do not trail it when it turns off: about two of
/// the mirrors' time constants at 30k points a second.
const SETTLE: usize = 8;
/// The longest move between two points of a lit stroke: short enough that
/// the mirrors follow it rather than cut its corners.
const LINE_STEP: f64 = 0.02;
/// How many pieces a stretch of an element is cut into to measure how long
/// it is in the projector's view, before it is drawn in steps of
/// [`LINE_STEP`].
const MEASURE: usize = 16;
/// The fewest and most points a beam dwells for.
const DWELL: [usize; 2] = [4, 80];
/// How far inside a limit's edge lit points must stay, in galvo units: the
/// mirrors overshoot a fast move by about this much.
const MARGIN: f64 = 0.02;

/// One point of a projector's stream: where its mirrors point, galvo
/// coordinates −1 to 1 across and up its field, and its light, 0 to 1 per
/// emitter. Black is the light off.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaserPoint {
    pub x: f32,
    pub y: f32,
    pub rgb: [f32; 3],
}

impl LaserPoint {
    pub fn is_lit(&self) -> bool {
        self.rgb.iter().any(|c| *c > 0.)
    }
}

/// One projector's scan, played on repeat at `rate` points a second until
/// the next frame replaces it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaserFrame {
    pub projector: String,
    pub rate: u32,
    pub points: Vec<LaserPoint>,
}

/// A galvo place and its light.
type Point = ([f64; 2], [f64; 3]);

/// What the scanner draws with the light on, between jumps in black.
#[derive(Clone, Debug)]
enum Stroke {
    /// A beam: one direction, dwelt on.
    Dot(Point),
    /// A lit run of an element, each point at most about [`LINE_STEP`] from
    /// the last.
    Line(Vec<Point>),
}

impl Stroke {
    fn first(&self) -> [f64; 2] {
        match self {
            Stroke::Dot((at, _)) => *at,
            Stroke::Line(points) => points[0].0,
        }
    }
    fn last(&self) -> [f64; 2] {
        match self {
            Stroke::Dot((at, _)) => *at,
            Stroke::Line(points) => points[points.len() - 1].0,
        }
    }
}

fn lit(rgb: [f64; 3]) -> bool {
    rgb.iter().any(|c| *c > 0.)
}

pub(super) fn frame(
    rig: &LaserRig,
    projector: &Projector,
    state: &UniverseState,
) -> Option<LaserFrame> {
    let mut elements: Vec<&Element> = state
        .lasers
        .iter()
        .filter(|element| element.projector == projector.id && element.intensity > 0.)
        .collect();
    elements.sort_by(|a, b| a.layer.cmp(&b.layer).then(a.s.total_cmp(&b.s)));
    let strokes: Vec<Stroke> = elements
        .into_iter()
        .flat_map(|element| strokes(rig, projector, element))
        .collect();
    if strokes.is_empty() {
        return None;
    }
    let points = plan(&strokes, projector.rate);
    let points = points
        .into_iter()
        .map(|(at, rgb)| {
            let inside = projector
                .limit
                .as_deref()
                .is_none_or(|limit| inside(limit, at, MARGIN));
            LaserPoint {
                x: at[0] as f32,
                y: at[1] as f32,
                rgb: if inside {
                    rgb.map(|c| c.clamp(0., 1.) as f32)
                } else {
                    [0.; 3]
                },
            }
        })
        .collect();
    Some(LaserFrame {
        projector: projector.id.clone(),
        rate: projector.rate,
        points,
    })
}

/// What `element` draws on `projector`: a beam it can reach and that is
/// lit, or the lit runs along an outline. A stretch the scanner cannot reach
/// or that is black is left out, and the scan jumps over it in black.
fn strokes(rig: &LaserRig, projector: &Projector, element: &Element) -> Vec<Stroke> {
    let zone = rig.zone(&element.zone);
    let view = |along: f64| -> Option<Point> {
        let ([x, y], rgb) = element.point(along);
        Some((projector.galvo(zone.direction(x, y))?, rgb))
    };
    if element.outline == Outline::Point {
        return view(0.)
            .filter(|(_, rgb)| lit(*rgb))
            .map(Stroke::Dot)
            .into_iter()
            .collect();
    }
    // Every break along it, and between two breaks as many steps as keep
    // each move within a line step.
    let breaks = element.breaks();
    let mut places = vec![breaks[0]];
    for pair in breaks.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let at = |k: usize| from + (to - from) * k as f64 / MEASURE as f64;
        let length: f64 = (0..MEASURE)
            .filter_map(|k| Some(distance(view(at(k))?.0, view(at(k + 1))?.0)))
            .sum();
        let n = ((length / LINE_STEP).ceil() as usize).max(1);
        places.extend((1..=n).map(|k| from + (to - from) * k as f64 / n as f64));
    }
    let mut runs: Vec<Vec<Point>> = Vec::new();
    let mut open = false;
    for along in places {
        match view(along).filter(|(_, rgb)| lit(*rgb)) {
            Some(point) if open => runs.last_mut().expect("an open run").push(point),
            Some(point) => {
                runs.push(vec![point]);
                open = true;
            }
            None => open = false,
        }
    }
    // A closed outline lit across its seam is one run.
    let closed = matches!(element.outline, Outline::Ring | Outline::Polygon(_));
    if closed && open && runs.len() > 1 && view(0.).is_some_and(|(_, rgb)| lit(rgb)) {
        let last = runs.pop().expect("runs");
        runs[0].splice(0..0, last);
    }
    runs.into_iter().map(Stroke::Line).collect()
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn lerp2(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Points along a move from `from` to `to`, `from` excluded and `to`
/// included, at most `step` apart.
fn steps(from: [f64; 2], to: [f64; 2], step: f64) -> impl Iterator<Item = [f64; 2]> {
    let n = (distance(from, to) / step).ceil().max(1.) as usize;
    (1..=n).map(move |i| lerp2(from, to, i as f64 / n as f64))
}

/// The stream for `strokes`: a jump in black to each, settled at both
/// ends, each beam dwelt on, each line drawn point by point, and a jump
/// back to the start so the scan repeats seamlessly.
fn plan(strokes: &[Stroke], rate: u32) -> Vec<Point> {
    let black = [0.; 3];
    let start = strokes[0].first();
    // What the jumps and lines cost, so the rest of the budget can go to
    // dwell.
    let mut fixed = 0;
    let mut at = start;
    for stroke in strokes {
        fixed += steps(at, stroke.first(), BLANK_STEP).count() + 2 * SETTLE;
        if let Stroke::Line(points) = stroke {
            fixed += points.len();
        }
        at = stroke.last();
    }
    fixed += steps(at, start, BLANK_STEP).count();
    let dots = strokes
        .iter()
        .filter(|stroke| matches!(stroke, Stroke::Dot(_)))
        .count();
    let budget = (rate / FRAME_RATE) as usize;
    let dwell = ((budget.saturating_sub(fixed) as f64 / dots.max(1) as f64).round() as usize)
        .clamp(DWELL[0], DWELL[1]);

    let mut out = Vec::new();
    let mut at = start;
    for stroke in strokes {
        // Jump in black and settle.
        out.extend(steps(at, stroke.first(), BLANK_STEP).map(|p| (p, black)));
        out.extend(std::iter::repeat_n((stroke.first(), black), SETTLE));
        match stroke {
            Stroke::Dot(point) => out.extend(std::iter::repeat_n(*point, dwell)),
            Stroke::Line(points) => out.extend(points.iter().copied()),
        }
        at = stroke.last();
        out.extend(std::iter::repeat_n((at, black), SETTLE));
    }
    out.extend(steps(at, start, BLANK_STEP).map(|p| (p, black)));
    out
}

/// Whether `p` is inside `polygon` and at least `margin` from its edges.
fn inside(polygon: &[[f64; 2]], p: [f64; 2], margin: f64) -> bool {
    let mut crossings = false;
    let mut nearest = f64::INFINITY;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                crossings = !crossings;
            }
        }
        let edge = [b[0] - a[0], b[1] - a[1]];
        let length = edge[0] * edge[0] + edge[1] * edge[1];
        let t = if length > 0. {
            (((p[0] - a[0]) * edge[0] + (p[1] - a[1]) * edge[1]) / length).clamp(0., 1.)
        } else {
            0.
        };
        nearest = nearest.min(distance(p, lerp2(a, b, t)));
    }
    crossings && nearest >= margin
}

#[cfg(test)]
mod tests {
    use super::super::tests::facing_house;
    use super::super::{LaserRig, Zone};
    use super::*;
    use luma_patterns::laser::{frame as turn, Sample};

    fn element(outline: Outline, layer: u32, s: f64, x: f64) -> Element {
        Element {
            projector: "p".into(),
            zone: String::new(),
            layer,
            outline,
            s,
            samples: vec![Sample {
                at: [x, 0.],
                frame: turn(0.5, 0.),
                rgb: [1.; 3],
            }],
            intensity: 1.,
        }
    }

    fn beam(layer: u32, s: f64, x: f64) -> Element {
        element(Outline::Point, layer, s, x)
    }

    fn rig() -> LaserRig {
        LaserRig {
            projectors: vec![facing_house("p", 0.)],
            zones: vec![(
                "z".into(),
                Zone {
                    turn: [-20., 20.],
                    tilt: [0., 20.],
                },
            )],
        }
    }

    fn solve(lasers: Vec<Element>) -> LaserFrame {
        let state = UniverseState {
            lasers,
            ..UniverseState::default()
        };
        rig().solve(&state).pop().expect("a frame")
    }

    fn lit(frame: &LaserFrame) -> Vec<LaserPoint> {
        frame
            .points
            .iter()
            .copied()
            .filter(LaserPoint::is_lit)
            .collect()
    }

    fn spots(points: &[LaserPoint]) -> std::collections::BTreeSet<(i32, i32)> {
        points
            .iter()
            .map(|p| ((p.x * 1e4) as i32, (p.y * 1e4) as i32))
            .collect()
    }

    #[test]
    fn nothing_to_draw_no_frame() {
        assert!(rig().solve(&UniverseState::default()).is_empty());
    }

    /// Beams light only their own spots; every move between them is dark.
    #[test]
    fn beams_dwell_and_jump_in_black() {
        let frame = solve(
            (0..4)
                .map(|i| beam(0, i as f64 / 3., i as f64 / 3.))
                .collect(),
        );
        assert_eq!(spots(&lit(&frame)).len(), 4);
        for pair in frame.points.windows(2) {
            if pair[0].is_lit() && pair[1].is_lit() {
                assert_eq!((pair[0].x, pair[0].y), (pair[1].x, pair[1].y));
            }
        }
    }

    /// A line is a sweep at an even pace: many lit places, each move short,
    /// and the mirrors never stop on the way.
    #[test]
    fn a_line_sweeps_without_stopping() {
        let frame = solve(vec![element(Outline::Line, 0, 0.5, 0.)]);
        let points = lit(&frame);
        assert!(spots(&points).len() > 20, "{}", points.len());
        for pair in points.windows(2) {
            assert_ne!((pair[0].x, pair[0].y), (pair[1].x, pair[1].y));
            let step = (pair[0].x - pair[1].x).hypot(pair[0].y - pair[1].y);
            assert!(f64::from(step) < LINE_STEP * 1.5, "{step}");
        }
    }

    /// A ring lit all round is one closed stroke: it ends where it began,
    /// with no jump in black across its seam.
    #[test]
    fn a_ring_is_drawn_round_once() {
        let frame = solve(vec![element(Outline::Ring, 0, 0.5, 0.)]);
        let first = frame.points.iter().position(LaserPoint::is_lit).unwrap();
        let last = frame.points.iter().rposition(LaserPoint::is_lit).unwrap();
        assert!(frame.points[first..=last].iter().all(LaserPoint::is_lit));
        let (a, b) = (frame.points[first], frame.points[last]);
        assert!((a.x - b.x).hypot(a.y - b.y) < 0.03, "{a:?} {b:?}");
    }

    /// Colour dark along part of a line is blanked: the scan jumps over the
    /// dark stretch in black rather than drawing it, so it takes fewer
    /// points than the same line lit all the way.
    #[test]
    fn a_dark_stretch_is_jumped_in_black() {
        let mut dashed = element(Outline::Line, 0, 0.5, 0.);
        dashed.samples = [1., 1., 0., 0., 1., 1.]
            .into_iter()
            .map(|level| Sample {
                rgb: [level; 3],
                ..dashed.samples[0]
            })
            .collect();
        let whole = solve(vec![element(Outline::Line, 0, 0.5, 0.)]);
        let dashes = solve(vec![dashed]);
        assert!(lit(&dashes).len() < lit(&whole).len());
        // Two dashes: the light goes off and on again once in the middle.
        let switches = dashes
            .points
            .windows(2)
            .filter(|pair| pair[0].is_lit() != pair[1].is_lit())
            .count();
        assert_eq!(switches, 4, "{dashes:?}");
    }

    /// Each element's colour is its own: a red beam beside a blue one.
    #[test]
    fn each_element_has_its_own_colour() {
        let mut red = beam(0, 0., -0.5);
        red.samples[0].rgb = [1., 0., 0.];
        let mut blue = beam(0, 1., 0.5);
        blue.samples[0].rgb = [0., 0., 1.];
        let colours: std::collections::BTreeSet<[u32; 3]> = lit(&solve(vec![red, blue]))
            .iter()
            .map(|p| p.rgb.map(|c| c as u32))
            .collect();
        assert_eq!(colours.len(), 2, "{colours:?}");
    }

    /// Splitting the frame over more beams gives each less time: the
    /// projector's light is shared, not multiplied.
    #[test]
    fn more_beams_share_the_frame() {
        let dwell = |n: usize| {
            let frame = solve(
                (0..n)
                    .map(|i| beam(0, i as f64 / n as f64, i as f64 / n as f64 - 0.5))
                    .collect(),
            );
            lit(&frame).len() as f64 / frame.points.len() as f64 / n as f64
        };
        assert!(dwell(2) > dwell(12));
    }

    #[test]
    fn the_scan_returns_to_its_start() {
        let frame = solve(vec![beam(0, 0., -0.5), beam(0, 1., 0.5)]);
        let (first, last) = (frame.points[0], frame.points[frame.points.len() - 1]);
        assert!(!last.is_lit());
        assert!((first.x - last.x).abs() < 0.1, "{first:?} {last:?}");
    }

    /// A limit blanks the light outside it; the beam inside still shows.
    #[test]
    fn a_limit_blanks_outside_and_keeps_inside() {
        let mut rig = rig();
        rig.projectors[0].limit = Some(vec![[-1., -1.], [0., -1.], [0., 1.], [-1., 1.]]);
        let state = UniverseState {
            lasers: vec![beam(0, 0., -0.5), beam(1, 0., 0.5)],
            ..UniverseState::default()
        };
        let frame = rig.solve(&state).pop().unwrap();
        assert!(lit(&frame).iter().all(|p| p.x < 0.), "{frame:?}");
        assert!(!lit(&frame).is_empty());
    }

    #[test]
    fn a_beam_outside_the_field_is_dropped() {
        let mut rig = rig();
        rig.zones[0].1.turn = [-80., 80.];
        let state = UniverseState {
            lasers: vec![beam(0, 0., 1.), beam(0, 0.5, 0.)],
            ..UniverseState::default()
        };
        let frame = rig.solve(&state).pop().unwrap();
        assert!(lit(&frame).iter().all(|p| p.x.abs() < 1e-6));
    }

    #[test]
    fn inside_honours_the_margin() {
        let square = [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]];
        assert!(inside(&square, [0., 0.], 0.1));
        assert!(!inside(&square, [0.95, 0.], 0.1));
        assert!(!inside(&square, [1.5, 0.], 0.));
    }
}
