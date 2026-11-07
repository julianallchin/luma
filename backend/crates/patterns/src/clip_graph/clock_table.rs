//! Clock tables: the turns a clock has counted, as knots of a line, each a
//! beat and the turns there, read straight between them and on past either
//! end at the clock's period there.
//!
//! A clock whose period is fixed for the clip is built once, during graph
//! preparation, by integrating 1/period over the clip ([`integrate`]). One
//! a live control moves is a [`Line`]: it runs at the period it has and
//! takes a new one at a point that keeps it on the bar, so a stepped speed
//! stays on the grid. A live set's prepared graph keeps each line from
//! frame to frame; anything else reads the line a fixed clock of the period
//! now would be ([`live`]).
use crate::*;
use ndarray::Array3;
use std::collections::VecDeque;
pub(crate) const CELLS: usize = 256;
pub(crate) const STEPS: usize = 8;
pub(crate) const SAMPLES: usize = CELLS * STEPS + 1;
/// How many knots a line keeps: an event born before the oldest is read
/// on the oldest's period.
const KEPT: usize = 16;
/// A step in the beat larger than this, either way, is a jump: a seek or
/// a deck's loop. A frame is a small share of a beat.
const JUMP: f64 = 0.5;
/// Beats or turns closer than this are the same.
const EPSILON: f64 = 1e-6;

fn check(period: &Signal) -> Result<()> {
    if period.values().dim().2 != 1 {
        return Err(Error("clock periods need one value per head".into()));
    }
    if period.values().iter().any(|p| !p.is_finite() || *p <= 0.) {
        return Err(Error("clock periods must stay above 0 beats".into()));
    }
    Ok(())
}

/// A table of `knots` knots per head and time: channel 0 is the period
/// before the first knot, 1 the period after the last, then each knot's
/// beat, then each knot's turns.
fn table(heads: usize, times: usize, knots: usize) -> Array3<f64> {
    Array3::zeros((heads, times, 2 + 2 * knots))
}

/// Write one head's line at one time into `values`, the last knot repeated
/// to fill its width.
fn write(
    values: &mut Array3<f64>,
    (n, t): (usize, usize),
    (before, after): (f64, f64),
    knots: impl Iterator<Item = (f64, f64)> + Clone,
) {
    let width = (values.dim().2 - 2) / 2;
    let last = knots.clone().last().expect("a line has a knot");
    values[[n, t, 0]] = before;
    values[[n, t, 1]] = after;
    for (i, (beat, turns)) in knots.chain(std::iter::repeat(last)).take(width).enumerate() {
        values[[n, t, 2 + i]] = beat;
        values[[n, t, 2 + width + i]] = turns;
    }
}

fn signal(values: Array3<f64>, period: &Signal) -> Result<Signal> {
    let channels = Channels::components(values.dim().2)?;
    Signal::new(
        values,
        Unit::Number,
        channels,
        period.fixtures().map(Into::into),
    )
}

/// A fixed clock's table: one knot at the clip start for a period that
/// holds, else a knot per cell of the period integrated over the clip.
pub(crate) fn integrate(period: &Signal, frame: Frame<'_>) -> Result<Signal> {
    check(period)?;
    let (heads, times, _) = period.values().dim();
    let start = frame.clip_start;
    if times == 1 {
        let mut values = table(heads, 1, 1);
        for n in 0..heads {
            let p = period.at(n, 0, 0);
            write(&mut values, (n, 0), (p, p), [(start, 0.)].into_iter());
        }
        return signal(values, period);
    }
    if times != SAMPLES {
        return Err(Error(
            "clock table must be prepared over the whole clip".into(),
        ));
    }
    let mut values = table(heads, 1, CELLS + 1);
    let h = frame.clip_duration / (CELLS * STEPS) as f64;
    let cell = frame.clip_duration / CELLS as f64;
    for n in 0..heads {
        let mut sums = vec![0.; CELLS + 1];
        for k in 0..CELLS {
            let i = k * STEPS;
            let mut total = 1. / period.at(n, i, 0) + 1. / period.at(n, i + STEPS, 0);
            for j in 1..STEPS {
                total += (if j % 2 == 1 { 4. } else { 2. }) / period.at(n, i + j, 0);
            }
            sums[k + 1] = sums[k] + total * h / 3.;
        }
        let knots = sums
            .iter()
            .enumerate()
            .map(|(k, turns)| (start + k as f64 * cell, *turns));
        let ends = (period.at(n, 0, 0), period.at(n, SAMPLES - 1, 0));
        write(&mut values, (n, 0), ends, knots);
    }
    signal(values, period)
}

/// A live clock's table at each of `beats`: each head's line of `lines`
/// played on to each beat in turn, on bars `bar` beats long; with no
/// `lines`, the line a fixed clock of the period at that beat is.
pub(crate) fn live(
    period: &Signal,
    beats: &[f64],
    lines: Option<&mut Vec<Line>>,
    start: f64,
    bar: f64,
) -> Result<Signal> {
    check(period)?;
    let heads = period.values().dim().0;
    let mut values = table(heads, beats.len(), KEPT);
    let fixed = |n: usize, t: usize| Line::fixed(start, period.at(n, t, 0), beats[t]);
    match lines {
        Some(lines) => {
            if lines.len() != heads && !beats.is_empty() {
                *lines = (0..heads).map(|n| fixed(n, 0)).collect();
            }
            for (n, line) in lines.iter_mut().enumerate() {
                for (t, beat) in beats.iter().enumerate() {
                    line.follow(period.at(n, t, 0), *beat, start, bar);
                    line.write(&mut values, (n, t));
                }
            }
        }
        None => {
            for n in 0..heads {
                for t in 0..beats.len() {
                    fixed(n, t).write(&mut values, (n, t));
                }
            }
        }
    }
    signal(values, period)
}

/// A live clock as one player plays it: knots where it took a new period,
/// and the period after the last.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    /// Each knot's beat and the turns there, oldest first. The last is
    /// still to come while the line bridges back to the bar.
    knots: VecDeque<(f64, f64)>,
    /// Beats a turn after the last knot.
    period: f64,
    /// The beat it was last played at.
    played: f64,
}

impl Line {
    /// The line of a fixed clock of `period` from the clip start, played
    /// at `beat`.
    fn fixed(start: f64, period: f64, beat: f64) -> Self {
        Self {
            knots: VecDeque::from([(start, 0.)]),
            period,
            played: beat,
        }
    }

    /// Play the line on to `beat`, its period now being `target`. When time
    /// jumps it keeps its period and lines up with the bar again, as a
    /// fixed clock of that period.
    fn follow(&mut self, target: f64, beat: f64, start: f64, bar: f64) {
        if (beat - self.played).abs() > JUMP {
            *self = Self::fixed(start, self.period, beat);
        }
        if let Some(knots) = self
            .handover(target, bar)
            .filter(|knots| knots[0].0 <= beat)
        {
            for knot in knots {
                // A handover at the last knot takes its place.
                if (knot.0 - self.last().0).abs() < EPSILON {
                    self.knots.pop_back();
                }
                self.knots.push_back(knot);
            }
            while self.knots.len() > KEPT {
                self.knots.pop_front();
            }
            self.period = target;
        }
        self.played = beat;
    }

    fn last(&self) -> (f64, f64) {
        *self.knots.back().expect("a line has a knot")
    }

    /// The beat at `turns`, on from the last knot.
    fn beat_at(&self, turns: f64) -> f64 {
        let (beat, at) = self.last();
        beat + (turns - at) * self.period
    }

    /// The knots by which the line takes `target`, the first being when;
    /// `None` while it has it or bridges to the bar still. On a period that
    /// fits the bar (a whole share of it, or whole bars), it hands over at
    /// its next event on that period's grid, within a bar or an event: its
    /// beats and bars stay where they are. Off the grid, it bridges back
    /// with one event from its next to the grid. On any other period, it
    /// hands over at its next event and the clock leaves the grid.
    fn handover(&self, target: f64, bar: f64) -> Option<Vec<(f64, f64)>> {
        let (last, at) = self.last();
        if (target - self.period).abs() <= EPSILON * target || last > self.played {
            return None;
        }
        let from = self.played.max(last);
        let first = (at + (from - last) / self.period - EPSILON).ceil();
        let event = |k: f64| (self.beat_at(k), k);
        let Some(unit) = grid(target, bar) else {
            return Some(vec![event(first)]);
        };
        let horizon = from + bar.max(self.period) + EPSILON;
        let landing = (0..64)
            .map(|i| event(first + i as f64))
            .take_while(|(beat, _)| *beat <= horizon)
            .find_map(|(beat, k)| {
                let on = (beat / unit).round() * unit;
                ((beat - on).abs() < EPSILON).then_some((on, k))
            });
        Some(match landing {
            Some(knot) => vec![knot],
            None => {
                let (beat, k) = event(first);
                let on = ((beat + target / 2.) / unit).ceil() * unit;
                vec![(beat, k), (on, k + 1.)]
            }
        })
    }

    fn write(&self, values: &mut Array3<f64>, at: (usize, usize)) {
        let before = match (self.knots.front(), self.knots.get(1)) {
            (Some(a), Some(b)) => (b.0 - a.0) / (b.1 - a.1),
            _ => self.period,
        };
        write(
            values,
            at,
            (before, self.period),
            self.knots.iter().copied(),
        );
    }
}

/// The grid a period lands on: itself when it is a whole share of a bar,
/// the bar when it is whole bars; `None` when it is neither.
fn grid(period: f64, bar: f64) -> Option<f64> {
    let whole = |x: f64| x.round() >= 1. && (x - x.round()).abs() < EPSILON * x;
    (whole(bar / period) || whole(period / bar)).then_some(period.min(bar))
}

/// One head's line of a clock table at one time.
pub(crate) struct Clock<'a> {
    pub table: &'a Signal,
    pub head: usize,
    pub time: usize,
}
impl Clock<'_> {
    fn v(&self, i: usize) -> f64 {
        self.table.at(self.head, self.time, i)
    }
    /// Turns counted at `beat`.
    pub fn turns(&self, beat: f64) -> f64 {
        self.read(beat, false)
    }
    /// The beat where the clock has counted `turns`.
    pub fn beat_at(&self, turns: f64) -> f64 {
        self.read(turns, true)
    }
    /// The line at `x`: turns at a beat, or `by_turns`, the beat at turns.
    fn read(&self, x: f64, by_turns: bool) -> f64 {
        let knots = (self.table.values().dim().2 - 2) / 2;
        let (from, to) = if by_turns {
            (2 + knots, 2)
        } else {
            (2, 2 + knots)
        };
        let knot = |i: usize| (self.v(from + i), self.v(to + i));
        let on = |(x0, y0): (f64, f64), period: f64| {
            y0 + (x - x0) * if by_turns { period } else { 1. / period }
        };
        if x <= knot(0).0 {
            return on(knot(0), self.v(0));
        }
        if x >= knot(knots - 1).0 {
            return on(knot(knots - 1), self.v(1));
        }
        let (mut lo, mut hi) = (0, knots - 1);
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if knot(mid).0 <= x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let ((xa, ya), (xb, yb)) = (knot(lo), knot(hi));
        ya + (yb - ya) * (x - xa) / (xb - xa)
    }
}
