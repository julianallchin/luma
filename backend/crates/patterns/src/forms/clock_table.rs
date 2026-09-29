//! Integrated period tensors, built once during graph preparation.
use crate::*;
use ndarray::Array3;
pub(crate) const CELLS: usize = 256;
pub(crate) const STEPS: usize = 8;
pub(crate) const SAMPLES: usize = CELLS * STEPS + 1;

pub(crate) fn integrate(period: &Signal, frame: Frame<'_>) -> Result<Signal> {
    let (heads, times, channels) = period.values().dim();
    if channels != 1 {
        return Err(Error("clock periods need one value per head".into()));
    }
    if times == 1 {
        return period.map(
            Unit::Number,
            |p| if p == 0. { frame.clip_duration } else { p },
        );
    }
    if times != SAMPLES {
        return Err(Error(
            "clock table must be prepared over the whole clip".into(),
        ));
    }
    if period.values().iter().any(|p| !p.is_finite() || *p <= 0.) {
        return Err(Error("clock periods must stay positive".into()));
    }
    let mut sums = Array3::zeros((heads, 1, CELLS + 3));
    let h = frame.clip_duration / (CELLS * STEPS) as f64;
    for n in 0..heads {
        for k in 0..CELLS {
            let i = k * STEPS;
            let mut total = 1. / period.at(n, i, 0) + 1. / period.at(n, i + STEPS, 0);
            for j in 1..STEPS {
                total += (if j % 2 == 1 { 4. } else { 2. }) / period.at(n, i + j, 0);
            }
            sums[[n, 0, k + 1]] = sums[[n, 0, k]] + total * h / 3.;
        }
        sums[[n, 0, CELLS + 1]] = period.at(n, 0, 0);
        sums[[n, 0, CELLS + 2]] = period.at(n, SAMPLES - 1, 0);
    }
    Signal::new(
        sums,
        Unit::Number,
        Channels::components(CELLS + 3)?,
        period.fixtures().map(Into::into),
    )
}

pub(super) struct Clock<'a> {
    pub table: &'a Signal,
    pub head: usize,
    pub start: f64,
    pub span: f64,
}
impl Clock<'_> {
    fn v(&self, i: usize) -> f64 {
        self.table.at(self.head, 0, i)
    }
    pub fn turns(&self, beat: f64) -> f64 {
        if self.table.values().dim().2 == 1 {
            return (beat - self.start) / self.v(0);
        }
        let cell = self.span / CELLS as f64;
        let x = (beat - self.start) / cell;
        if x <= 0. {
            return (beat - self.start) / self.v(CELLS + 1);
        }
        if x >= CELLS as f64 {
            return self.v(CELLS) + (beat - self.start - self.span) / self.v(CELLS + 2);
        }
        let k = (x.floor() as usize).min(CELLS - 1);
        self.v(k) + (self.v(k + 1) - self.v(k)) * (x - k as f64)
    }
    pub fn beat_at(&self, turns: f64) -> f64 {
        if self.table.values().dim().2 == 1 {
            return self.start + turns * self.v(0);
        }
        if turns <= 0. {
            return self.start + turns * self.v(CELLS + 1);
        }
        if turns >= self.v(CELLS) {
            return self.start + self.span + (turns - self.v(CELLS)) * self.v(CELLS + 2);
        }
        let (mut lo, mut hi) = (0, CELLS);
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.v(mid) <= turns {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let fraction = (turns - self.v(lo)) / (self.v(lo + 1) - self.v(lo));
        self.start + (lo as f64 + fraction) * self.span / CELLS as f64
    }
}
