//! Speed inputs as an odometer. A speed curve is summed over the clip into
//! a table once per call; every query reads that table, so a frame depends
//! only on its beat. Seeking to a beat gives the same frame as playing to it.
use crate::{Error, Keyframes, Result};

/// Cells in the summed table of a speed curve.
const CELLS: usize = 256;
/// Simpson steps inside one cell.
const STEPS: usize = 8;

#[derive(Clone, Debug)]
pub(crate) enum Pace {
    Constant {
        origin: f64,
        period: f64,
    },
    /// `sums[k]` is the count of periods from the origin to cell edge `k`.
    Table {
        origin: f64,
        cell: f64,
        sums: Vec<f64>,
        first: f64,
        last: f64,
    },
}

impl Pace {
    /// One period lasts `period` beats, or follows `curve` over the clip
    /// progress when a curve is given. The count starts at `origin`.
    pub(crate) fn new(
        origin: f64,
        span: f64,
        period: f64,
        curve: Option<&Keyframes>,
    ) -> Result<Self> {
        let Some(curve) = curve else {
            if !period.is_finite() || period <= 0.0 {
                return Err(Error("a period must be positive beats".into()));
            }
            return Ok(Self::Constant { origin, period });
        };
        curve.validate()?;
        if curve.is_color() || curve.values().any(|v| v <= 0.0) {
            return Err(Error("a speed curve needs positive beat values".into()));
        }
        if !span.is_finite() || span <= 0.0 {
            return Err(Error("clip duration must be positive".into()));
        }
        let period = |u: f64| curve.sample(u)[0];
        let rate = |u: f64| span / period(u);
        let mut sums = Vec::with_capacity(CELLS + 1);
        sums.push(0.0);
        let width = 1.0 / CELLS as f64;
        for k in 0..CELLS {
            let a = k as f64 * width;
            let h = width / STEPS as f64;
            let mut total = rate(a) + rate(a + width);
            for step in 1..STEPS {
                total += rate(a + step as f64 * h) * if step % 2 == 1 { 4.0 } else { 2.0 };
            }
            sums.push(sums[k] + total * h / 3.0);
        }
        Ok(Self::Table {
            origin,
            cell: span / CELLS as f64,
            sums,
            first: period(0.0),
            last: period(1.0),
        })
    }

    /// Periods counted from the origin to `beat`. Negative before it.
    pub(crate) fn turns(&self, beat: f64) -> f64 {
        match self {
            Self::Constant { origin, period } => (beat - origin) / period,
            Self::Table {
                origin,
                cell,
                sums,
                first,
                last,
            } => {
                let x = (beat - origin) / cell;
                if x <= 0.0 {
                    return (beat - origin) / first;
                }
                if x >= CELLS as f64 {
                    return sums[CELLS] + (beat - origin - CELLS as f64 * cell) / last;
                }
                let k = (x.floor() as usize).min(CELLS - 1);
                sums[k] + (sums[k + 1] - sums[k]) * (x - k as f64)
            }
        }
    }

    /// The beat at which the count reaches `turns`: the inverse of `turns`.
    pub(crate) fn beat_at(&self, turns: f64) -> f64 {
        match self {
            Self::Constant { origin, period } => origin + turns * period,
            Self::Table {
                origin,
                cell,
                sums,
                first,
                last,
            } => {
                if turns <= 0.0 {
                    return origin + turns * first;
                }
                if turns >= sums[CELLS] {
                    return origin + CELLS as f64 * cell + (turns - sums[CELLS]) * last;
                }
                let k = sums.partition_point(|sum| *sum <= turns).clamp(1, CELLS) - 1;
                let fraction = (turns - sums[k]) / (sums[k + 1] - sums[k]);
                origin + (k as f64 + fraction) * cell
            }
        }
    }
}
