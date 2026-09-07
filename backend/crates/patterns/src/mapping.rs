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
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingSpec {
    pub source: MappingSource,
    pub per_group: bool,
    pub reverse: bool,
}
impl MappingSpec {
    pub fn validate(&self) -> Result<()> {
        match &self.source {
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
                        group
                            .iter()
                            .zip(fit.angular_positions)
                            .map(|(c, p)| (c.id.clone(), f64::from(p))),
                        *origin,
                        self.reverse,
                    )?
                }
                source => {
                    let axis = if let MappingSource::MajorAxis { toward } = source {
                        Some(major_axis(group, toward)?)
                    } else {
                        None
                    };
                    Mapping::linear(
                        group.iter().enumerate().map(|(index, c)| {
                            let p = match source {
                                MappingSource::U => c.uvz[0],
                                MappingSource::V => c.uvz[1],
                                MappingSource::Z => c.uvz[2],
                                MappingSource::Order => index as f64,
                                MappingSource::MajorAxis { .. } => dot(c.world, axis.unwrap()),
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
