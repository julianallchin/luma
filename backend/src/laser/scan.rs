//! The scan plan: one projector's beams become one point stream.
//!
//! A beam is the scanner dwelling on one direction; a line is the scanner
//! moving with the light on. Between beams the light is off while the
//! mirrors jump. The frame aims for [`FRAME_RATE`] scans a second: the points
//! a jump and a line need come first, and what is left of the budget is
//! shared out as dwell on the beams. A frame with more to draw than the
//! budget runs long, and so refreshes slower, as a real projector would.
//!
//! Brightness is power times time: a projector that splits a frame over
//! sixteen beams gives each a sixteenth of the time, and so a sixteenth of
//! the light. Nothing here hides that.
use super::{LaserRig, Projector, COLOR_SAMPLES};
use crate::models::universe::UniverseState;
use luma_patterns::laser::Beam;
use serde::{Deserialize, Serialize};

/// Scans a second a frame aims for.
pub const FRAME_RATE: u32 = 30;
/// The longest mirror move between two points, in galvo units, with the light
/// off.
const BLANK_STEP: f64 = 0.08;
/// Points spent still at either end of a jump, so the mirrors settle before
/// the light turns on and do not trail it when it turns off.
const SETTLE: usize = 3;
/// The longest move between two points of a lit line: short enough that the
/// mirrors follow the line rather than cut its corners.
const LINE_STEP: f64 = 0.02;
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

/// A beam on its way to the stream: galvo place, colour, and how lit the
/// move to the next vertex of its path is.
#[derive(Clone, Copy, Debug)]
struct Vertex {
    at: [f64; 2],
    rgb: [f64; 3],
    join: f64,
}

pub(super) fn frame(
    rig: &LaserRig,
    projector: &Projector,
    state: &UniverseState,
) -> Option<LaserFrame> {
    let mut beams: Vec<&Beam> = state
        .beams
        .iter()
        .filter(|beam| beam.projector == projector.id && beam.intensity > 0.)
        .collect();
    beams.sort_by(|a, b| a.path.cmp(&b.path).then(a.s.total_cmp(&b.s)));
    // Each path's beams in order; a beam the scanner cannot reach breaks
    // the path there.
    let mut paths: Vec<Vec<Vertex>> = Vec::new();
    let mut last_path = None;
    for beam in beams {
        let direction = rig.zone(&beam.zone).direction(beam.x, beam.y);
        let Some(at) = projector.galvo(direction) else {
            last_path = None;
            continue;
        };
        let rgb = color(state, &projector.id, beam.s).map(|c| c * beam.intensity.clamp(0., 1.));
        let vertex = Vertex {
            at,
            rgb,
            join: beam.join.clamp(0., 1.),
        };
        match paths.last_mut() {
            Some(path) if last_path == Some(beam.path) => path.push(vertex),
            _ => paths.push(vec![vertex]),
        }
        last_path = Some(beam.path);
    }
    if paths.is_empty() {
        return None;
    }
    let points = plan(&paths, projector.rate);
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

/// The colour at place `s` along a laser clip's order: the projector's
/// colour heads as the Color clips over it left them, read at `s`. With no
/// colour on the projector, white at full.
fn color(state: &UniverseState, projector: &str, s: f64) -> [f64; 3] {
    let head = |i: usize| {
        state
            .primitives
            .get(&format!("{projector}:{i}"))
            .map(|head| head.color.map(|c| f64::from(c * head.dimmer)))
    };
    let at = s.clamp(0., 1.) * (COLOR_SAMPLES - 1) as f64;
    let (below, t) = (at.floor() as usize, at.fract());
    let above = (below + 1).min(COLOR_SAMPLES - 1);
    match (head(below), head(above)) {
        (None, None) => [1.; 3],
        (a, b) => {
            let (a, b) = (a.unwrap_or([0.; 3]), b.unwrap_or([0.; 3]));
            std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t)
        }
    }
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

/// The stream for `paths`: jumps in black between paths, each vertex dwelt
/// on by how much of it is a beam, each lit move drawn as a line, and a
/// jump back to the start so the scan repeats seamlessly.
fn plan(paths: &[Vec<Vertex>], rate: u32) -> Vec<([f64; 2], [f64; 3])> {
    let black = [0.; 3];
    let start = paths[0][0].at;
    // A vertex is a beam by `1 − join` of the move out of it; the last of
    // a path has no move out and is all beam.
    let beamness = |path: &[Vertex], i: usize| {
        if i + 1 < path.len() {
            1. - path[i].join
        } else {
            1.
        }
    };
    // What the moves cost, so the rest of the budget can go to dwell.
    let mut fixed = 0;
    let mut at = start;
    for path in paths {
        fixed += steps(at, path[0].at, BLANK_STEP).count() + 2 * SETTLE;
        for pair in path.windows(2) {
            let step = if pair[0].join > 0. {
                LINE_STEP
            } else {
                BLANK_STEP
            };
            fixed += steps(pair[0].at, pair[1].at, step).count();
        }
        at = path[path.len() - 1].at;
    }
    fixed += steps(at, start, BLANK_STEP).count();
    let weight: f64 = paths
        .iter()
        .flat_map(|path| (0..path.len()).map(move |i| beamness(path, i)))
        .sum();
    let budget = (rate / FRAME_RATE) as usize;
    let per = budget.saturating_sub(fixed) as f64 / weight.max(1e-9);
    let dwell = |beam: f64| {
        if beam <= 0. {
            1
        } else {
            ((per * beam).round() as usize).clamp(DWELL[0], DWELL[1])
        }
    };

    let mut out = Vec::new();
    let mut at = start;
    for path in paths {
        // Jump in black and settle.
        out.extend(steps(at, path[0].at, BLANK_STEP).map(|p| (p, black)));
        out.extend(std::iter::repeat_n((path[0].at, black), SETTLE));
        for (i, vertex) in path.iter().enumerate() {
            out.extend(std::iter::repeat_n(
                (vertex.at, vertex.rgb),
                dwell(beamness(path, i)),
            ));
            let Some(next) = path.get(i + 1) else {
                break;
            };
            if vertex.join > 0. {
                let n = steps(vertex.at, next.at, LINE_STEP).count() as f64;
                out.extend(
                    steps(vertex.at, next.at, LINE_STEP)
                        .enumerate()
                        .map(|(k, p)| {
                            let t = (k + 1) as f64 / n;
                            let rgb = std::array::from_fn(|c| {
                                (vertex.rgb[c] + (next.rgb[c] - vertex.rgb[c]) * t) * vertex.join
                            });
                            (p, rgb)
                        }),
                );
            } else {
                out.extend(std::iter::repeat_n((vertex.at, black), SETTLE));
                out.extend(steps(vertex.at, next.at, BLANK_STEP).map(|p| (p, black)));
                out.extend(std::iter::repeat_n((next.at, black), SETTLE));
            }
        }
        at = path[path.len() - 1].at;
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
    use crate::models::universe::PrimitiveState;

    fn beam(projector: &str, path: u32, s: f64, x: f64, join: f64) -> Beam {
        Beam {
            projector: projector.into(),
            zone: String::new(),
            path,
            s,
            x,
            y: 0.,
            join,
            intensity: 1.,
        }
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

    fn solve(beams: Vec<Beam>) -> LaserFrame {
        let state = UniverseState {
            beams,
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

    #[test]
    fn no_beams_no_frame() {
        assert!(rig().solve(&UniverseState::default()).is_empty());
    }

    /// Beams with no join light only their own spots; every move between
    /// them is dark.
    #[test]
    fn beams_dwell_and_jump_in_black() {
        let frame = solve(
            (0..4)
                .map(|i| beam("p", 0, i as f64 / 3., i as f64 / 3., 0.))
                .collect(),
        );
        let spots: std::collections::BTreeSet<(i32, i32)> = lit(&frame)
            .iter()
            .map(|p| ((p.x * 1e4) as i32, (p.y * 1e4) as i32))
            .collect();
        assert_eq!(spots.len(), 4, "{spots:?}");
        for pair in frame.points.windows(2) {
            if pair[0].is_lit() && pair[1].is_lit() {
                assert_eq!((pair[0].x, pair[0].y), (pair[1].x, pair[1].y));
            }
        }
    }

    /// A joined path is a lit line: many lit places, each move short.
    #[test]
    fn a_joined_path_draws_a_line() {
        let frame = solve(vec![beam("p", 0, 0., -1., 1.), beam("p", 0, 1., 1., 1.)]);
        let places: std::collections::BTreeSet<i32> =
            lit(&frame).iter().map(|p| (p.x * 1e4) as i32).collect();
        assert!(places.len() > 20, "{}", places.len());
    }

    /// Splitting the frame over more beams gives each less time: the
    /// projector's light is shared, not multiplied.
    #[test]
    fn more_beams_share_the_frame() {
        let dwell = |n: usize| {
            let frame = solve(
                (0..n)
                    .map(|i| beam("p", 0, i as f64 / n as f64, i as f64 / n as f64 - 0.5, 0.))
                    .collect(),
            );
            lit(&frame).len() as f64 / frame.points.len() as f64 / n as f64
        };
        assert!(dwell(2) > dwell(12));
    }

    #[test]
    fn the_scan_returns_to_its_start() {
        let frame = solve(vec![beam("p", 0, 0., -0.5, 0.), beam("p", 0, 1., 0.5, 0.)]);
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
            beams: vec![beam("p", 0, 0., -0.5, 0.), beam("p", 1, 0., 0.5, 0.)],
            ..UniverseState::default()
        };
        let frame = rig.solve(&state).pop().unwrap();
        assert!(lit(&frame).iter().all(|p| p.x < 0.), "{frame:?}");
        assert!(!lit(&frame).is_empty());
    }

    #[test]
    fn a_beam_takes_the_colour_at_its_place() {
        let mut state = UniverseState::default();
        for i in 0..COLOR_SAMPLES {
            let red = i as f32 / (COLOR_SAMPLES - 1) as f32;
            state.primitives.insert(
                format!("p:{i}"),
                PrimitiveState {
                    color: [red, 0., 1. - red],
                    dimmer: 1.,
                    ..crate::eval::composite::nothing()
                },
            );
        }
        assert_eq!(color(&state, "p", 0.), [0., 0., 1.]);
        assert_eq!(color(&state, "p", 1.), [1., 0., 0.]);
        assert_eq!(color(&UniverseState::default(), "p", 0.3), [1.; 3]);
    }

    #[test]
    fn a_beam_outside_the_field_is_dropped() {
        let mut rig = rig();
        rig.zones[0].1.turn = [-80., 80.];
        let state = UniverseState {
            beams: vec![beam("p", 0, 0., 1., 0.), beam("p", 0, 0.5, 0., 0.)],
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
