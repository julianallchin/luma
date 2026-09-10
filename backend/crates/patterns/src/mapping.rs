use crate::{circle_fit, Error, Mapping, Result};
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
    Circle {
        origin: f64,
    },
    /// Direction in stage coordinates: U right, V downstage, Z up.
    Vector {
        direction: [f64; 3],
    },
}
impl MappingSource {
    pub const OPTIONS: [(&'static str, &'static str); 7] = [
        ("z", "Up (Z+)"),
        ("u", "Stage right (U+)"),
        ("v", "Downstage (V+)"),
        ("major_axis", "Major axis"),
        ("circle", "Solved circle"),
        ("order", "Selection order"),
        ("vector", "Custom vector"),
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Self::U => "u",
            Self::V => "v",
            Self::Z => "z",
            Self::Order => "order",
            Self::MajorAxis { .. } => "major_axis",
            Self::Circle { .. } => "circle",
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
            "circle" => Self::Circle { origin: 0. },
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
    pub per_group: bool,
    pub reverse: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<MirrorPlane>,
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
        if let Some(mirror) = &self.mirror {
            mirror.validate()?;
            if matches!(self.source, MappingSource::Order) {
                return Err(Error("a mirror plane requires a spatial mapping; selection order has no spatial axis".into()));
            }
        }
        match &self.source {
            MappingSource::Vector { direction } => unit_direction(*direction).map(|_| ()),
            MappingSource::Circle { origin } if !origin.is_finite() => {
                Err(Error("circle origin must be finite".into()))
            }
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
                .entry(if self.per_group { &c.group } else { "" })
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
                MappingSource::Circle { origin } => {
                    let positions: Vec<_> = group
                        .iter()
                        .map(|c| (c.world[0] as f32, c.world[1] as f32, c.world[2] as f32))
                        .collect();
                    let fit = circle_fit::fit_circle_3d(&positions)
                        .ok_or_else(|| Error("selection does not define a solved circle".into()))?;
                    if fit.is_inlier.iter().any(|v| !*v) {
                        return Err(Error(
                            "circle mapping contains outliers; refine the selection".into(),
                        ));
                    }
                    Mapping::circle(
                        group.iter().enumerate().map(|(index, c)| {
                            let position =
                                folded
                                    .as_ref()
                                    .map_or(fit.angular_positions[index], |folded| {
                                        fit.angular_position(
                                            Cell::stage_coordinates(folded[index])
                                                .map(|v| v as f32),
                                        )
                                    });
                            (c.id.clone(), f64::from(position))
                        }),
                        *origin,
                        self.reverse,
                    )?
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
