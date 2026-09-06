use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coordinate {
    pub cell: String,
    pub position: f64,
    pub closed: bool,
}

/// A resolved coordinate field, not authored fixture references. The host
/// resolves group expressions and supplies U/V/Z or solved geometry per cell.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub coordinates: Vec<Coordinate>,
}
impl Mapping {
    pub fn validate(&self) -> Result<()> {
        let mut cells = BTreeSet::new();
        for c in &self.coordinates {
            if c.cell.is_empty()
                || !c.position.is_finite()
                || !(0.0..=1.0).contains(&c.position)
                || !cells.insert(&c.cell)
            {
                return Err(Error(
                    "mapping needs unique cell identities and finite positions in 0..1".into(),
                ));
            }
        }
        Ok(())
    }
    /// A solved circle already has turn coordinates. Do not min/max normalize:
    /// the last sampled cell and the first still have a real closing interval.
    pub fn circle(
        cells: impl IntoIterator<Item = (String, f64)>,
        origin: f64,
        reverse: bool,
    ) -> Result<Self> {
        if !origin.is_finite() {
            return Err(Error("circle origin must be finite".into()));
        }
        let m = Self {
            coordinates: cells
                .into_iter()
                .map(|(cell, angle)| Coordinate {
                    cell,
                    position: ((angle - origin) * if reverse { -1.0 } else { 1.0 }).rem_euclid(1.0),
                    closed: true,
                })
                .collect(),
        };
        m.validate()?;
        Ok(m)
    }
    pub fn linear(cells: impl IntoIterator<Item = (String, f64)>, reverse: bool) -> Result<Self> {
        let cells: Vec<_> = cells.into_iter().collect();
        if cells.iter().any(|(_, v)| !v.is_finite()) {
            return Err(Error("mapping coordinates must be finite".into()));
        }
        let min = cells.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min);
        let max = cells
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        let m = Self {
            coordinates: cells
                .into_iter()
                .map(|(cell, v)| {
                    let p = if max > min {
                        (v - min) / (max - min)
                    } else {
                        0.5
                    };
                    Coordinate {
                        cell,
                        position: if reverse { 1.0 - p } else { p },
                        closed: false,
                    }
                })
                .collect(),
        };
        m.validate()?;
        Ok(m)
    }
}

/// Stable hash independent of Rust's Hash implementation, traversal order,
/// process, or frame. Cell identity makes adjacent pixels independent.
pub(crate) fn threshold(cell: &str, seed: u64) -> f64 {
    let mut h = 0xcbf29ce484222325_u64 ^ seed;
    for b in cell.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d049bb133111eb);
    h ^= h >> 31;
    // Strictly interior, leaving exact progress endpoints unambiguous.
    ((h >> 11) as f64 + 0.5) / 9007199254740992.0
}

pub(crate) fn epoch_seed(seed: u64, epoch: i64) -> u64 {
    let mut value = seed ^ (epoch as u64).wrapping_mul(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}
