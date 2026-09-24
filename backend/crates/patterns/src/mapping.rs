use crate::{Error, Mapping, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Host-resolved cell geometry. U/V/Z is supplied by the venue's existing
/// coordinate convention; it is not silently renamed world X/Y/Z here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub id: String,
    pub group: String,
    pub world: [f64; 3],
    pub uvz: [f64; 3],
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MappingSource {
    U,
    V,
    Z,
    Order,
    /// Disambiguate the principal eigenvector's sign by an authored direction.
    MajorAxis {
        toward: [f64; 3],
    },
    /// Distance from the span's centroid within its plane.
    Radial,
    /// Turns around the span's centroid within its plane.
    Angle,
    /// Direction in stage coordinates: U right, V downstage, Z up.
    Vector {
        direction: [f64; 3],
    },
}
impl MappingSource {
    pub const OPTIONS: [(&'static str, &'static str); 8] = [
        ("z", "Up (Z+)"),
        ("u", "Stage right (U+)"),
        ("v", "Downstage (V+)"),
        ("major_axis", "Major axis"),
        ("order", "Selection order"),
        ("radial", "Radial"),
        ("angle", "Angle"),
        ("vector", "Custom vector"),
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Self::U => "u",
            Self::V => "v",
            Self::Z => "z",
            Self::Order => "order",
            Self::MajorAxis { .. } => "major_axis",
            Self::Radial => "radial",
            Self::Angle => "angle",
            Self::Vector { .. } => "vector",
        }
    }

    pub fn from_key(key: &str) -> Result<Self> {
        Ok(match key {
            "u" => Self::U,
            "v" => Self::V,
            "z" => Self::Z,
            "order" => Self::Order,
            "major_axis" => Self::MajorAxis {
                toward: [0., 0., 1.],
            },
            "radial" => Self::Radial,
            "angle" => Self::Angle,
            "vector" => Self::Vector {
                direction: [1., 0., 1.],
            },
            _ => return Err(Error(format!("Unknown mapping {key}"))),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingSpec {
    pub source: MappingSource,
    /// The old graphs' per-group axis; the same as [`Span::Group`]. Forms
    /// use `span`.
    pub per_group: bool,
    pub reverse: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<MirrorPlane>,
    /// What one axis covers: the selection, each fixture or each group.
    #[serde(default, skip_serializing_if = "Span::is_selection")]
    pub span: Span,
    /// The plane radial and angle read in, around the centroid of the span.
    /// Radial and angle require it; other sources have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<AxisPlane>,
}

/// What one axis covers. Path, mirror, width and shape apply within each
/// span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Span {
    /// One axis across the whole selection.
    #[default]
    Selection,
    /// Each fixture (the head id before its last `:`) gets its own axis.
    Fixture,
    /// Each group of the selection expression gets its own axis.
    Group,
}
impl Span {
    pub const OPTIONS: [(Span, &'static str); 3] = [
        (Span::Selection, "Selection"),
        (Span::Fixture, "Fixture"),
        (Span::Group, "Group"),
    ];
    fn is_selection(&self) -> bool {
        *self == Span::Selection
    }
}

/// The plane radial and angle read in. Angle 0 is along the plane's first
/// direction and turns toward its second:
/// around up–down: stage right (U+) toward downstage (V+);
/// around front–back: stage right (U+) toward up (Z+);
/// around left–right: downstage (V+) toward up (Z+).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AxisPlane {
    /// The best-fit plane of the heads in the span; see [`AxisPlane::basis`].
    Auto,
    UpDown,
    FrontBack,
    LeftRight,
    /// A plane normal in stage U/V/Z.
    Custom {
        normal: [f64; 3],
    },
}
impl AxisPlane {
    pub const OPTIONS: [&'static str; 5] = [
        "Auto",
        "Around up–down",
        "Around front–back",
        "Around left–right",
        "Custom axis",
    ];
    pub fn index(&self) -> usize {
        match self {
            Self::Auto => 0,
            Self::UpDown => 1,
            Self::FrontBack => 2,
            Self::LeftRight => 3,
            Self::Custom { .. } => 4,
        }
    }

    /// The fixed planes as (normal, first direction). The second direction
    /// is normal × first.
    const FIXED: [([f64; 3], [f64; 3]); 3] = [
        ([0., 0., 1.], [1., 0., 0.]),
        ([0., -1., 0.], [1., 0., 0.]),
        ([1., 0., 0.], [0., 1., 0.]),
    ];

    /// The two in-plane directions for `points` (stage U/V/Z) around
    /// `center`.
    ///
    /// Auto takes the normal as the direction of least spread of the
    /// points (the two largest spread directions span the plane). Points on
    /// one line or one spot give around up–down. The normal then takes its
    /// sign and first direction from the fixed plane it is closest to
    /// (ties go up–down, front–back, left–right), so a ring facing the
    /// audience reads exactly like around front–back and similar rigs do
    /// not flip. A custom normal keeps its own sign and takes its first
    /// direction the same way.
    fn basis(&self, points: &[[f64; 3]], center: [f64; 3]) -> Result<[[f64; 3]; 2]> {
        let fixed = |index: usize| {
            let (normal, first) = Self::FIXED[index];
            [first, cross(normal, first)]
        };
        let normal = match self {
            Self::UpDown => return Ok(fixed(0)),
            Self::FrontBack => return Ok(fixed(1)),
            Self::LeftRight => return Ok(fixed(2)),
            Self::Custom { normal } => unit_direction(*normal)?,
            Self::Auto => match least_spread(points, center) {
                Some(normal) => normal,
                None => return Ok(fixed(0)),
            },
        };
        let nearest = (0..3)
            .max_by(|a, b| {
                dot(normal, Self::FIXED[*a].0)
                    .abs()
                    .total_cmp(&dot(normal, Self::FIXED[*b].0).abs())
                    .then_with(|| b.cmp(a))
            })
            .unwrap();
        let (reference, first) = Self::FIXED[nearest];
        let normal = if matches!(self, Self::Auto) && dot(normal, reference) < 0. {
            normal.map(|v| -v)
        } else {
            normal
        };
        let along = dot(first, normal);
        let first = unit_direction(std::array::from_fn(|a| first[a] - along * normal[a]))?;
        Ok([first, cross(normal, first)])
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorPlane {
    /// Plane normal in stage U/V/Z coordinates; independent of mapping direction.
    pub normal: [f64; 3],
    /// Metres along the normal from the selected domain's projected midpoint.
    pub offset: f64,
}

impl MirrorPlane {
    pub fn validate(&self) -> Result<()> {
        unit_direction(self.normal)?;
        if !self.offset.is_finite() {
            return Err(Error("mirror plane offset must be finite".into()));
        }
        Ok(())
    }

    fn fold(&self, cells: &[&Cell]) -> Result<Vec<[f64; 3]>> {
        let normal = unit_direction(self.normal)?;
        let (min, max) = cells
            .iter()
            .map(|cell| dot(cell.uvz, normal))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
                (min.min(value), max.max(value))
            });
        // Use the extent, not the centroid: fixture density on one side must
        // not move the symmetry plane of an otherwise unchanged selection.
        let center = min * 0.5 + max * 0.5 + self.offset;
        Ok(cells
            .iter()
            .map(|cell| {
                let distance = (dot(cell.uvz, normal) - center).min(0.);
                std::array::from_fn(|axis| cell.uvz[axis] - 2. * distance * normal[axis])
            })
            .collect())
    }
}
impl MappingSpec {
    pub fn validate(&self) -> Result<()> {
        if self.per_group && self.span != Span::Selection {
            return Err(Error("per_group is the group span; set only one".into()));
        }
        let planar = matches!(self.source, MappingSource::Radial | MappingSource::Angle);
        if planar != self.plane.is_some() {
            return Err(Error(if planar {
                "radial and angle need a plane".into()
            } else {
                "only radial and angle take a plane".into()
            }));
        }
        if let Some(AxisPlane::Custom { normal }) = &self.plane {
            unit_direction(*normal)?;
        }
        if let Some(mirror) = &self.mirror {
            mirror.validate()?;
            if matches!(self.source, MappingSource::Order) {
                return Err(Error("a mirror plane requires a spatial mapping; selection order has no spatial axis".into()));
            }
        }
        match &self.source {
            MappingSource::Vector { direction } => unit_direction(*direction).map(|_| ()),
            MappingSource::MajorAxis { toward }
                if toward.iter().any(|v| !v.is_finite()) || dot(*toward, *toward) < 1e-12 =>
            {
                Err(Error(
                    "major axis needs a finite nonzero orientation vector".into(),
                ))
            }
            _ => Ok(()),
        }
    }
    pub fn resolve(&self, cells: &[Cell]) -> Result<Mapping> {
        self.validate()?;
        if cells
            .iter()
            .flat_map(|c| c.world.iter().chain(&c.uvz))
            .any(|v| !v.is_finite())
        {
            return Err(Error("cell geometry must be finite".into()));
        }
        let mut groups: BTreeMap<&str, Vec<&Cell>> = BTreeMap::new();
        for c in cells {
            groups
                .entry(match (self.per_group, self.span) {
                    (true, _) | (_, Span::Group) => c.group.as_str(),
                    (_, Span::Fixture) => fixture_of(&c.id),
                    (_, Span::Selection) => "",
                })
                .or_default()
                .push(c);
        }
        let mut result = Mapping::default();
        for group in groups.values() {
            let folded = self
                .mirror
                .as_ref()
                .map(|plane| plane.fold(group))
                .transpose()?;
            let mapped = match &self.source {
                MappingSource::Angle | MappingSource::Radial => {
                    let plane = self.plane.as_ref().expect("validated");
                    let points: Vec<[f64; 3]> = group
                        .iter()
                        .enumerate()
                        .map(|(index, c)| folded.as_ref().map_or(c.uvz, |folded| folded[index]))
                        .collect();
                    let center = centroid(&points);
                    let [first, second] = plane.basis(&points, center)?;
                    let flat = points.iter().map(|p| {
                        let d: [f64; 3] = std::array::from_fn(|a| p[a] - center[a]);
                        (dot(d, first), dot(d, second))
                    });
                    let cells = group.iter().zip(flat);
                    if matches!(self.source, MappingSource::Angle) {
                        Mapping::circle(
                            cells.map(|(c, (x, y))| {
                                let turns = y.atan2(x) / std::f64::consts::TAU;
                                (c.id.clone(), turns.rem_euclid(1.0))
                            }),
                            0.0,
                            self.reverse,
                        )?
                    } else {
                        Mapping::linear(
                            cells.map(|(c, (x, y))| (c.id.clone(), x.hypot(y))),
                            self.reverse,
                        )?
                    }
                }
                source => {
                    let axis = match source {
                        MappingSource::MajorAxis { toward } => major_axis(group, toward)?,
                        MappingSource::Vector { direction } => unit_direction(*direction)?,
                        _ => [0.; 3],
                    };
                    Mapping::linear(
                        group.iter().enumerate().map(|(index, c)| {
                            let uvz = folded.as_ref().map_or(c.uvz, |folded| folded[index]);
                            let p = match source {
                                MappingSource::U => uvz[0],
                                MappingSource::V => uvz[1],
                                MappingSource::Z => uvz[2],
                                MappingSource::Order => index as f64,
                                MappingSource::MajorAxis { .. } => dot(
                                    if folded.is_some() {
                                        Cell::stage_coordinates(uvz)
                                    } else {
                                        c.world
                                    },
                                    axis,
                                ),
                                MappingSource::Vector { .. } => dot(uvz, axis),
                                _ => unreachable!(),
                            };
                            (c.id.clone(), p)
                        }),
                        self.reverse,
                    )?
                }
            };
            result.coordinates.extend(mapped.coordinates);
        }
        result.validate()?;
        Ok(result)
    }
}
/// The fixture part of a head identity (`fixture:head`).
pub(crate) fn fixture_of(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(fixture, _)| fixture)
}
/// The mean position.
fn centroid(points: &[[f64; 3]]) -> [f64; 3] {
    let n = points.len().max(1) as f64;
    std::array::from_fn(|a| points.iter().map(|p| p[a]).sum::<f64>() / n)
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
/// The unit direction of least spread of `points` around `center`, or
/// `None` when the points lie on one line or one spot.
fn least_spread(points: &[[f64; 3]], center: [f64; 3]) -> Option<[f64; 3]> {
    let mut m = [[0.0; 3]; 3];
    for p in points {
        for a in 0..3 {
            for b in 0..3 {
                m[a][b] += (p[a] - center[a]) * (p[b] - center[b]);
            }
        }
    }
    // Cyclic Jacobi rotations: `m` becomes diagonal, `v` holds the
    // eigenvectors as columns.
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
    let mut order = [0, 1, 2];
    order.sort_by(|a, b| m[*a][*a].total_cmp(&m[*b][*b]));
    let [_, middle, most] = order.map(|i| m[i][i]);
    if most <= 1e-12 || middle <= most * 1e-9 {
        return None;
    }
    unit_direction(std::array::from_fn(|a| v[a][order[0]])).ok()
}
fn unit_direction(direction: [f64; 3]) -> Result<[f64; 3]> {
    if direction.iter().any(|value| !value.is_finite()) {
        return Err(Error("direction needs finite U, V and Z components".into()));
    }
    // Rescale before squaring so every finite nonzero vector has the same
    // orientation regardless of magnitude, without overflow or underflow.
    let scale = direction.iter().map(|value| value.abs()).fold(0., f64::max);
    if scale == 0. {
        return Err(Error("direction must not be zero".into()));
    }
    let direction = direction.map(|value| value / scale);
    let length = dot(direction, direction).sqrt();
    Ok(direction.map(|value| value / length))
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}
fn major_axis(cells: &[&Cell], toward: &[f64; 3]) -> Result<[f64; 3]> {
    if toward.iter().any(|v| !v.is_finite()) || dot(*toward, *toward) < 1e-12 {
        return Err(Error(
            "major axis needs a nonzero orientation vector".into(),
        ));
    }
    let mut center = [0.0; 3];
    for c in cells {
        for (a, v) in center.iter_mut().enumerate() {
            *v += c.world[a] / cells.len() as f64;
        }
    }
    let mut covariance = [[0.0; 3]; 3];
    for c in cells {
        for a in 0..3 {
            for b in 0..3 {
                covariance[a][b] += (c.world[a] - center[a]) * (c.world[b] - center[b]);
            }
        }
    }
    let multiply = |v: [f64; 3]| covariance.map(|row| dot(row, v));
    // Starting from every basis vector avoids a seed orthogonal to the dominant
    // eigenspace. Select the largest Rayleigh quotient deterministically.
    let mut best = ([0.0; 3], 0.0);
    for seed in 0..3 {
        let mut v = [0.0; 3];
        v[seed] = 1.0;
        for _ in 0..96 {
            let next = multiply(v);
            let length = dot(next, next).sqrt();
            if length < 1e-12 {
                break;
            }
            v = next.map(|x| x / length);
        }
        let eigenvalue = dot(v, multiply(v));
        if eigenvalue > best.1 {
            best = (v, eigenvalue);
        }
    }
    if best.1 < 1e-12 {
        return Err(Error("coincident cells do not define a major axis".into()));
    }
    let mut alignment = dot(best.0, *toward);
    if alignment.abs() < 1e-9 {
        // A perpendicular hint has no preference between the two signs.
        // Orient the largest component positively, with X/Y/Z breaking ties.
        let dominant = (0..3)
            .max_by(|a, b| {
                best.0[*a]
                    .abs()
                    .total_cmp(&best.0[*b].abs())
                    .then_with(|| b.cmp(a))
            })
            .unwrap();
        alignment = best.0[dominant];
    }
    Ok(best.0.map(|v| if alignment < 0.0 { -v } else { v }))
}

impl Cell {
    /// Venue coordinates are X stage right, Y upstage, Z up. The authored
    /// stage convention is U right, V downstage, Z up, independent of rig fits.
    pub fn stage_coordinates(world: [f64; 3]) -> [f64; 3] {
        [world[0], -world[1], world[2]]
    }
}
