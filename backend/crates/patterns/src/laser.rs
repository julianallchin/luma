//! Laser beams on a zone's canvas: where a clip's beams sit, and how a layer
//! of beams combines with the beams under it.
//!
//! A laser clip draws its own beams on each projector it selects, `beams`
//! of them, as heads `projector:0` to `projector:{beams - 1}`. A beam's place
//! along its clip's order is `s`, 0 to 1, which never depends on how many
//! beams exist, so a colour or a morph can follow beams of any count. Canvas
//! coordinates run from −1 to 1 across and up a zone; a solver turns them
//! into directions and then into galvo angles.
use crate::runtime::EvaluatedValue;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// The graph output with each beam's canvas place, join and alpha.
pub const BEAM_OUTPUT: &str = "beam";
/// Channels of [`BEAM_OUTPUT`]: x, y, join, alpha.
pub const BEAM_CHANNELS: usize = 4;

/// How a clip lays its beams out before its size, rotation and offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// In a row across the canvas.
    Fan,
    /// Evenly round a circle.
    Circle,
    /// In rows and columns, as square as the count allows.
    Grid,
}

impl Shape {
    pub fn from_setting(setting: Option<&str>) -> Self {
        match setting {
            Some("circle") => Shape::Circle,
            Some("grid") => Shape::Grid,
            _ => Shape::Fan,
        }
    }

    /// Beam `index` of `count` at size 1, rotation 0 and no offset: a fan
    /// spans the canvas, a circle touches its edges.
    pub fn layout(self, index: usize, count: usize) -> [f64; 2] {
        let spread = |i: usize, n: usize| {
            if n > 1 {
                2. * i as f64 / (n - 1) as f64 - 1.
            } else {
                0.
            }
        };
        match self {
            Shape::Fan => [spread(index, count), 0.],
            Shape::Circle => {
                let turn = std::f64::consts::TAU * index as f64 / count.max(1) as f64;
                [turn.sin(), turn.cos()]
            }
            Shape::Grid => {
                let columns = (count as f64).sqrt().ceil().max(1.) as usize;
                let rows = count.div_ceil(columns);
                [
                    spread(index % columns, columns),
                    -spread(index / columns, rows),
                ]
            }
        }
    }
}

/// Beam `index` of `count`'s place along its clip's order, 0 to 1. A lone
/// beam sits in the middle.
pub fn along(index: usize, count: usize) -> f64 {
    if count > 1 {
        index as f64 / (count - 1) as f64
    } else {
        0.5
    }
}

/// Beam `index` of `count` on the canvas: the shape's place scaled by
/// `size`, turned by `rotation` degrees (clockwise), then moved to `(x, y)`.
pub fn place(
    shape: Shape,
    index: usize,
    count: usize,
    size: f64,
    rotation: f64,
    at: [f64; 2],
) -> [f64; 2] {
    let [u, v] = shape.layout(index, count).map(|c| c * size);
    let (sin, cos) = rotation.to_radians().sin_cos();
    [at[0] + u * cos + v * sin, at[1] - u * sin + v * cos]
}

/// One beam after the layers under it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Beam {
    /// The projector fixture's id.
    pub projector: String,
    /// The zone's id; empty is the projector's first zone.
    pub zone: String,
    /// The layer that drew it: consecutive beams of one path may be joined.
    pub path: u32,
    /// Its place along its clip's order, 0 to 1.
    pub s: f64,
    /// Canvas place, each −1 to 1.
    pub x: f64,
    pub y: f64,
    /// 0 dark between this beam and the next on its path, 1 a lit line.
    pub join: f64,
    /// How much of the beam shows, 0 to 1. A clip's fresh beam carries its
    /// alpha here.
    pub intensity: f64,
}

impl Beam {
    /// Each head's beam at each sample of `times`, read from
    /// [`BEAM_OUTPUT`]: one row per time, one `[x, y, join, alpha]` per head
    /// of `ids`.
    pub fn read(
        value: &EvaluatedValue,
        ids: &[String],
        times: usize,
    ) -> Result<Vec<Vec<[f64; BEAM_CHANNELS]>>> {
        let signal = value
            .signal()
            .filter(|signal| signal.values().dim().2 == BEAM_CHANNELS)
            .ok_or_else(|| Error("a laser graph gives four channels per beam".into()))?;
        let rows = ids
            .iter()
            .map(|id| match signal.fixtures() {
                None => Ok(0),
                Some(heads) => heads
                    .iter()
                    .position(|head| head == id)
                    .ok_or_else(|| Error(format!("the laser output is missing beam {id}"))),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((0..times)
            .map(|t| {
                rows.iter()
                    .map(|&n| std::array::from_fn(|ch| signal.at(n, t, ch)))
                    .collect()
            })
            .collect())
    }

    fn surface(&self) -> (&str, &str) {
        (&self.projector, &self.zone)
    }
}

/// The beams of one projector and zone, sorted along their order, read as
/// one curve over `s`: place and join at any `s` by linear interpolation,
/// held flat past either end.
struct Curve<'a> {
    beams: Vec<&'a Beam>,
}

impl<'a> Curve<'a> {
    fn new(beams: impl IntoIterator<Item = &'a Beam>) -> Option<Self> {
        let mut beams: Vec<&Beam> = beams.into_iter().collect();
        if beams.is_empty() {
            return None;
        }
        beams.sort_by(|a, b| a.s.total_cmp(&b.s));
        Some(Curve { beams })
    }

    /// `[x, y, join, intensity]` at `s`.
    fn at(&self, s: f64) -> [f64; 4] {
        let value = |b: &Beam| [b.x, b.y, b.join, b.intensity];
        let after = self.beams.partition_point(|b| b.s < s);
        match (after.checked_sub(1), self.beams.get(after)) {
            (None, Some(first)) => value(first),
            (Some(last), None) => value(self.beams[last]),
            (Some(below), Some(above)) => {
                let (a, b) = (self.beams[below], *above);
                let t = if b.s > a.s {
                    (s - a.s) / (b.s - a.s)
                } else {
                    0.
                };
                let (a, b) = (value(a), value(b));
                std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t)
            }
            (None, None) => unreachable!("a curve has a beam"),
        }
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Draw `top` over `under` as well: an Add layer. Each beam keeps its own
/// place; `top`'s show at their alpha.
pub fn add_beams(under: &mut Vec<Beam>, top: Vec<Beam>) {
    under.extend(top);
}

/// Lay `top` over `under` as a Replace layer: on each projector and zone the
/// top layer draws, the beams under it and the top's beams morph into each
/// other by the top's alpha, matched by their place along their order.
/// Under beams move toward the top's curve and fade by `1 − alpha`; top beams
/// start on the curve under them and show at `alpha`. At alpha 1 only the
/// top's beams show; with equal counts the two sets meet beam for beam.
/// Beams on a projector or zone the top layer does not draw stay as they
/// are.
pub fn replace_beams(under: &mut Vec<Beam>, mut top: Vec<Beam>) {
    let surfaces: Vec<(String, String)> = {
        let mut seen: Vec<(String, String)> = Vec::new();
        for beam in &top {
            let key = (beam.projector.clone(), beam.zone.clone());
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
        seen
    };
    for (projector, zone) in &surfaces {
        let on = |b: &&Beam| b.surface() == (projector.as_str(), zone.as_str());
        let below: Vec<Beam> = under.iter().filter(on).cloned().collect();
        let above: Vec<Beam> = top.iter().filter(on).cloned().collect();
        let below = Curve::new(&below);
        let above = Curve::new(&above).expect("a surface the top draws has beams");
        for beam in under.iter_mut().filter(|b| on(&&**b)) {
            let [x, y, join, alpha] = above.at(beam.s);
            beam.x = lerp(beam.x, x, alpha);
            beam.y = lerp(beam.y, y, alpha);
            beam.join = lerp(beam.join, join, alpha);
            beam.intensity *= 1. - alpha;
        }
        if let Some(below) = below {
            for beam in top.iter_mut().filter(|b| on(&&**b)) {
                let [x, y, join, _] = below.at(beam.s);
                let alpha = beam.intensity;
                beam.x = lerp(x, beam.x, alpha);
                beam.y = lerp(y, beam.y, alpha);
                beam.join = lerp(join, beam.join, alpha);
            }
        }
    }
    under.retain(|beam| beam.intensity > 0.);
    under.extend(top);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beam(path: u32, s: f64, x: f64, intensity: f64) -> Beam {
        Beam {
            projector: "p".into(),
            zone: String::new(),
            path,
            s,
            x,
            y: 0.,
            join: 0.,
            intensity,
        }
    }

    #[test]
    fn a_fan_spans_the_canvas_and_a_lone_beam_sits_in_the_middle() {
        assert_eq!(Shape::Fan.layout(0, 5), [-1., 0.]);
        assert_eq!(Shape::Fan.layout(4, 5), [1., 0.]);
        assert_eq!(Shape::Fan.layout(0, 1), [0., 0.]);
        assert_eq!(along(0, 1), 0.5);
        assert_eq!(along(3, 4), 1.);
    }

    #[test]
    fn a_grid_is_as_square_as_the_count_allows() {
        let places: Vec<[f64; 2]> = (0..9).map(|i| Shape::Grid.layout(i, 9)).collect();
        assert_eq!(places[0], [-1., 1.]);
        assert_eq!(places[4], [0., 0.]);
        assert_eq!(places[8], [1., -1.]);
    }

    #[test]
    fn a_circle_keeps_its_beams_on_the_ring() {
        for i in 0..7 {
            let [u, v] = Shape::Circle.layout(i, 7);
            assert!((u.hypot(v) - 1.).abs() < 1e-12);
        }
    }

    /// Width and offset are enough to grow a fan from one edge: with the
    /// offset at `size − 1`, the fan's outer beam stays on the left edge.
    #[test]
    fn a_fan_grows_from_one_edge_by_size_and_offset() {
        for size in [0.1, 0.4, 0.9] {
            let outer = place(Shape::Fan, 0, 6, size, 0., [-1. + size, 0.]);
            assert!((outer[0] + 1.).abs() < 1e-12, "size {size}: {outer:?}");
        }
    }

    #[test]
    fn rotation_turns_a_fan_upright() {
        let [x, y] = place(Shape::Fan, 4, 5, 1., 90., [0., 0.]);
        assert!(x.abs() < 1e-12 && (y + 1.).abs() < 1e-12, "{x} {y}");
    }

    #[test]
    fn add_draws_both_layers() {
        let mut under = vec![beam(0, 0.5, 0., 1.)];
        add_beams(&mut under, vec![beam(1, 0.5, 0.5, 0.5)]);
        assert_eq!(under.len(), 2);
    }

    /// Equal counts meet beam for beam: halfway through, each pair sits at
    /// the midpoint and their intensities sum to one beam's.
    #[test]
    fn replace_morphs_equal_counts_beam_for_beam() {
        let under: Vec<Beam> = (0..3).map(|i| beam(0, along(i, 3), -1., 1.)).collect();
        let top: Vec<Beam> = (0..3).map(|i| beam(1, along(i, 3), 1., 0.5)).collect();
        let mut out = under;
        replace_beams(&mut out, top);
        assert_eq!(out.len(), 6);
        for b in &out {
            assert!(b.x.abs() < 1e-12, "{b:?}");
        }
        let total: f64 = out.iter().map(|b| b.intensity).sum();
        assert!((total - 3.).abs() < 1e-12);
    }

    #[test]
    fn replace_at_full_alpha_leaves_only_the_top() {
        let mut out = vec![beam(0, 0.2, -1., 1.), beam(0, 0.8, -0.5, 1.)];
        replace_beams(&mut out, vec![beam(1, 0.5, 1., 1.)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].x, 1.);
    }

    #[test]
    fn replace_leaves_other_projectors_alone() {
        let mut other = beam(0, 0.5, -1., 1.);
        other.projector = "q".into();
        let mut out = vec![other.clone()];
        replace_beams(&mut out, vec![beam(1, 0.5, 1., 1.)]);
        assert!(out.contains(&other));
    }
}
