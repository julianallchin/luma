//! The heads pipeline: which cells form one unit, which span each unit
//! belongs to, and the stage geometry that `space`, `mirror` and `noise`
//! read. Units and spans are fixed at preparation; positions may move when
//! a mirror's plane is wired, so the geometry helpers here serve both the
//! lowering and the kernels.
use crate::mapping::{fixture_of, head_of};
use crate::Cell;

/// The unit and span of every cell, in cell order (sorted by id).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Units {
    /// Per cell: its unit's number.
    pub unit: Vec<usize>,
    /// Per cell: its span's number.
    pub span: Vec<usize>,
    /// Per cell: the index of its unit's first cell, whose id keys the
    /// unit's random draws.
    pub first: Vec<usize>,
    /// Per cell: its unit's place in the selection order (fixture, then
    /// head number).
    pub order: Vec<f64>,
}

/// What `split` makes a span of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SplitBy {
    Fixture,
    Group,
}

/// The selection order of every cell: by fixture, then head number.
fn selection_order(cells: &[Cell]) -> Vec<f64> {
    let mut sorted: Vec<usize> = (0..cells.len()).collect();
    sorted.sort_by(|a, b| {
        let key = |n: usize| {
            let id = cells[n].id.as_str();
            (fixture_of(id), head_of(id).unwrap_or(u64::MAX), id)
        };
        key(*a).cmp(&key(*b))
    });
    let mut order = vec![0.; cells.len()];
    for (rank, n) in sorted.into_iter().enumerate() {
        order[n] = rank as f64;
    }
    order
}

impl Units {
    /// Every cell its own unit, all in one span.
    pub fn base(cells: &[Cell]) -> Self {
        Units {
            unit: (0..cells.len()).collect(),
            span: vec![0; cells.len()],
            first: (0..cells.len()).collect(),
            order: selection_order(cells),
        }
    }

    /// Each fixture (or each group) becomes its own span. Units stay.
    pub fn split(&self, cells: &[Cell], by: SplitBy) -> Self {
        let mut names: Vec<(usize, &str)> = Vec::new();
        let span = (0..cells.len())
            .map(|n| {
                let first = &cells[self.first[n]];
                let name = match by {
                    SplitBy::Fixture => fixture_of(&first.id),
                    SplitBy::Group => first.group.as_str(),
                };
                let key = (self.span[n], name);
                match names.iter().position(|known| *known == key) {
                    Some(index) => index,
                    None => {
                        names.push(key);
                        names.len() - 1
                    }
                }
            })
            .collect();
        Units {
            span,
            ..self.clone()
        }
    }

    /// Units of `size` consecutive heads within a fixture, in head order;
    /// `None` makes each fixture one unit. Spans stay.
    pub fn group(&self, cells: &[Cell], size: Option<usize>) -> Self {
        let mut fixtures: Vec<((usize, &str), Vec<usize>)> = Vec::new();
        for (n, cell) in cells.iter().enumerate() {
            let key = (self.span[n], fixture_of(&cell.id));
            match fixtures.iter_mut().find(|(known, _)| *known == key) {
                Some((_, members)) => members.push(n),
                None => fixtures.push((key, vec![n])),
            }
        }
        let base = selection_order(cells);
        let mut unit = vec![0; cells.len()];
        let mut first = vec![0; cells.len()];
        let mut next = 0;
        for (_, members) in &mut fixtures {
            members.sort_by(|a, b| base[*a].total_cmp(&base[*b]));
            let chunk = size.unwrap_or(members.len()).max(1);
            for part in members.chunks(chunk) {
                for n in part {
                    unit[*n] = next;
                    first[*n] = part[0];
                }
                next += 1;
            }
        }
        let order = first.iter().map(|f| base[*f]).collect();
        Units {
            unit,
            span: self.span.clone(),
            first,
            order,
        }
    }
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
/// The unit vector of `v`, or `None` when it is zero or not finite.
pub(crate) fn unit_direction(v: [f64; 3]) -> Option<[f64; 3]> {
    if v.iter().any(|c| !c.is_finite()) {
        return None;
    }
    let scale = v.iter().map(|c| c.abs()).fold(0., f64::max);
    if scale == 0. {
        return None;
    }
    let v = v.map(|c| c / scale);
    let length = dot(v, v).sqrt();
    Some(v.map(|c| c / length))
}
pub(crate) fn centroid(points: &[[f64; 3]]) -> [f64; 3] {
    let n = points.len().max(1) as f64;
    std::array::from_fn(|a| points.iter().map(|p| p[a]).sum::<f64>() / n)
}

/// The spread of `points` around `center`: eigenvalues and eigenvectors
/// (as columns), by cyclic Jacobi rotations.
fn spread(points: &[[f64; 3]], center: [f64; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut m = [[0.0; 3]; 3];
    for p in points {
        for a in 0..3 {
            for b in 0..3 {
                m[a][b] += (p[a] - center[a]) * (p[b] - center[b]);
            }
        }
    }
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..32 {
        let off = m[0][1].powi(2) + m[0][2].powi(2) + m[1][2].powi(2);
        if off < 1e-30 {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if m[p][q].abs() < 1e-300 {
                continue;
            }
            let theta = (m[q][q] - m[p][p]) / (2.0 * m[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            let mut next = m;
            for k in 0..3 {
                next[k][p] = c * m[k][p] - s * m[k][q];
                next[k][q] = s * m[k][p] + c * m[k][q];
            }
            let rotated = next;
            for k in 0..3 {
                next[p][k] = c * rotated[p][k] - s * rotated[q][k];
                next[q][k] = s * rotated[p][k] + c * rotated[q][k];
            }
            m = next;
            for row in &mut v {
                let (a, b) = (row[p], row[q]);
                row[p] = c * a - s * b;
                row[q] = s * a + c * b;
            }
        }
    }
    ([m[0][0], m[1][1], m[2][2]], v)
}

/// The best-fit direction: the stage axis (+U, +V or +Z) the points spread
/// along most, U before V before Z on ties. A stage axis, never a tilted
/// one, so a symmetric rig never flips between two. `None` for one point or
/// points at one spot.
pub(crate) fn best_fit_axis(points: &[[f64; 3]]) -> Option<[f64; 3]> {
    let center = centroid(points);
    let spread: [f64; 3] =
        std::array::from_fn(|a| points.iter().map(|p| (p[a] - center[a]).powi(2)).sum());
    let most = spread.iter().copied().fold(0., f64::max);
    if most <= 1e-12 {
        return None;
    }
    let axis = (0..3).find(|a| spread[*a] >= most * (1. - 1e-9))?;
    Some(std::array::from_fn(|a| if a == axis { 1. } else { 0. }))
}

/// The point at `at` (0–1 per axis) within the box of `points`: (0.5, 0.5,
/// 0.5) is the middle of the box, whatever the points' centroid.
pub(crate) fn at_in_box(points: &[[f64; 3]], at: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|a| {
        let (low, high) = points
            .iter()
            .map(|p| p[a])
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            });
        if low.is_finite() {
            low + at[a] * (high - low)
        } else {
            0.
        }
    })
}

/// The direction of least spread, or `None` when the points lie on one
/// line or one spot.
fn least_spread(points: &[[f64; 3]], center: [f64; 3]) -> Option<[f64; 3]> {
    let (values, vectors) = spread(points, center);
    let mut order = [0, 1, 2];
    order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
    let [_, middle, most] = order.map(|i| values[i]);
    if most <= 1e-12 || middle <= most * 1e-9 {
        return None;
    }
    unit_direction(std::array::from_fn(|a| vectors[a][order[0]]))
}

/// The fixed planes as (normal, first direction): around up–down,
/// front–back and left–right.
const FIXED: [([f64; 3], [f64; 3]); 3] = [
    ([0., 0., 1.], [1., 0., 0.]),
    ([0., -1., 0.], [1., 0., 0.]),
    ([1., 0., 0.], [0., 1., 0.]),
];

/// The two in-plane directions `radial` and `angle` read in. `normal`
/// `None` takes the direction of least spread and snaps its sign and first
/// direction to the nearest fixed plane (ties up–down, front–back,
/// left–right); points on one line or spot read around up–down.
pub(crate) fn plane_basis(
    normal: Option<[f64; 3]>,
    points: &[[f64; 3]],
    center: [f64; 3],
) -> [[f64; 3]; 2] {
    let fixed = |index: usize| {
        let (normal, first) = FIXED[index];
        [first, cross(normal, first)]
    };
    let auto = normal.is_none();
    let Some(normal) = normal
        .and_then(unit_direction)
        .or_else(|| least_spread(points, center))
    else {
        return fixed(0);
    };
    let nearest = (0..3)
        .max_by(|a, b| {
            dot(normal, FIXED[*a].0)
                .abs()
                .total_cmp(&dot(normal, FIXED[*b].0).abs())
                .then_with(|| b.cmp(a))
        })
        .unwrap();
    let (reference, first) = FIXED[nearest];
    let normal = if auto && dot(normal, reference) < 0. {
        normal.map(|v| -v)
    } else {
        normal
    };
    let along = dot(first, normal);
    let Some(first) = unit_direction(std::array::from_fn(|a| first[a] - along * normal[a])) else {
        return fixed(nearest);
    };
    [first, cross(normal, first)]
}

/// Where a mirror's plane sits along unit `normal` for one span: `at`
/// (0–1) across the span's positions before any fold, `originals`.
pub(crate) fn plane(originals: &[[f64; 3]], normal: [f64; 3], at: f64) -> f64 {
    let (min, max) = originals
        .iter()
        .map(|p| dot(*p, normal))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    if !min.is_finite() {
        return 0.;
    }
    min + at * (max - min)
}

/// Folds `points` (one span) across the plane with unit `normal` at
/// `plane` along it. Returns each point's folded position and whether it
/// was on the low side.
pub(crate) fn fold(points: &[[f64; 3]], normal: [f64; 3], plane: f64) -> Vec<([f64; 3], bool)> {
    points
        .iter()
        .map(|p| {
            let distance = dot(*p, normal) - plane;
            let low = distance.min(0.);
            (
                std::array::from_fn(|a| p[a] - 2. * low * normal[a]),
                distance < -1e-9,
            )
        })
        .collect()
}
