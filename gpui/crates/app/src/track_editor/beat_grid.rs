//! Musical time on the beat grid: the rung ladder, snapping, and moving a time
//! by whole beats.

use super::*;

/// The snap capture radius, in screen pixels, for a cursor or an insertion —
/// `snapToGrid`'s `15`.
pub(super) const SNAP_CAPTURE: f32 = 15.;
/// The tighter radius inside a clip drag.
pub(super) const SNAP_CAPTURE_DRAG: f32 = 12.;

/// One bar, for a track with no beat grid to ask: a default beat of 0.5 s
/// times four beats to the bar.
const DEFAULT_BAR: f64 = 2.;

/// How long a clip a right-click inserts at `after` should be.
///
/// The next downbeat if there is one — so an inserted clip lands on the bar
/// line the eye can see — and the mean bar otherwise.
/// Where `bars` whole bars after `start` end, bar by bar along the grid.
pub(super) fn bars_after(beats: Option<&BeatGrid>, start: f64, bars: usize) -> f64 {
    (0..bars).fold(start, |end, _| end + bar_length(beats, end))
}

pub(super) fn bar_length(beats: Option<&BeatGrid>, after: f64) -> f64 {
    let Some(grid) = beats.filter(|grid| !grid.beats.is_empty()) else {
        return DEFAULT_BAR;
    };
    if let Some(next) = grid
        .downbeats
        .iter()
        .map(|beat| f64::from(*beat))
        .find(|beat| *beat > after)
    {
        return next - after;
    }
    if grid.downbeats.len() > 1 {
        let first = f64::from(grid.downbeats[0]);
        let last = f64::from(grid.downbeats[grid.downbeats.len() - 1]);
        return (last - first) / (grid.downbeats.len() - 1) as f64;
    }
    let average = if grid.beats.len() > 1 {
        f64::from(grid.beats[grid.beats.len() - 1] - grid.beats[0]) / (grid.beats.len() - 1) as f64
    } else {
        0.5
    };
    average
        * if grid.beats_per_bar == 0 {
            4.
        } else {
            f64::from(grid.beats_per_bar)
        }
}

/// `snapToGrid`: quantise `time` to the beat subdivision the zoom asks for, but
/// only when the quantised point is within `capture` screen pixels of it.
///
/// The capture radius is a parameter because one gesture has two radii: 15 px
/// for the selection cursor and 12 px inside a clip drag.
pub(super) fn snap(beats: Option<&BeatGrid>, time: f64, zoom: f32, capture: f32) -> f64 {
    let Some(snapped) = beat_snap(beats, time, zoom) else {
        return time;
    };
    if (snapped - time).abs() * f64::from(zoom) < f64::from(capture) {
        snapped
    } else {
        time
    }
}

/// `time` moved by the musical distance from `from` to `to`.
///
/// The distance is counted in beats, not seconds, because beats on a detected
/// grid differ in length. A clip moved by a fixed number of seconds lands a
/// fraction of a millisecond off the grid, and then overlaps its neighbour.
/// Without a grid there are no beats to count, so seconds are all there is.
pub(super) fn shift_time(
    clock: Option<&luma_patterns::BeatTimeline>,
    time: f64,
    from: f64,
    to: f64,
) -> f64 {
    // The beat round trip is not bit exact, and a clip that did not move
    // must keep its authored beat.
    if from == to {
        return time;
    }
    let musical = clock.and_then(|clock| {
        let beat =
            clock.beat_at(time).ok()? + clock.beat_at(to).ok()? - clock.beat_at(from).ok()?;
        clock.seconds_at(beat).ok()
    });
    musical.unwrap_or(time + (to - from))
}

/// The quantised point, before the capture test, or `None` when there is no
/// grid to quantise against.
///
/// It snaps to the grid lines the eye can see: the finest [`Grid`] rung that
/// has at least [`MIN_SNAP_SPACING`] of room.
fn beat_snap(beats: Option<&BeatGrid>, time: f64, zoom: f32) -> Option<f64> {
    let grid = beats.filter(|grid| !grid.beats.is_empty())?;
    let (beat, bar) = mean_lengths(grid);
    let divisions = match Grid::fit(beat * zoom, bar * zoom, MIN_SNAP_SPACING).0 {
        Grid::Beat(divisions) => f64::from(divisions),
        Grid::Bars(n) if !grid.downbeats.is_empty() => {
            return Some(bar_snap(&grid.downbeats, time, n as usize));
        }
        Grid::Bars(_) => 1.,
    };
    let beats = &grid.beats;
    if beats.len() == 1 {
        return Some(f64::from(beats[0]));
    }
    let average = f64::from(beats[beats.len() - 1] - beats[0]) / (beats.len() - 1) as f64;
    let index = beats
        .partition_point(|beat| f64::from(*beat) <= time)
        .saturating_sub(1);
    let prev = f64::from(beats[index]);
    let next = f64::from(*beats.get(index + 1)?);
    let length = if next - prev > 0. {
        next - prev
    } else {
        average
    };
    if !length.is_finite() || length <= 0. {
        return Some(prev);
    }
    let step = ((time - prev) / length * divisions)
        .round()
        .clamp(0., divisions);
    Some((prev + step / divisions * length).clamp(prev, next))
}

/// The nearest of every `n`-th downbeat, counted from bar 1.
fn bar_snap(downbeats: &[f32], time: f64, n: usize) -> f64 {
    let index = downbeats
        .partition_point(|bar| f64::from(*bar) <= time)
        .saturating_sub(1);
    let before = index / n * n;
    let at = |index: usize| f64::from(downbeats[index]);
    match downbeats.get(before + n) {
        Some(after) if f64::from(*after) - time < time - at(before) => f64::from(*after),
        _ => at(before),
    }
}

/// One rung of the grid ladder, from finest to coarsest.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Grid {
    /// `n` lines to the beat: 4, 2 or 1.
    Beat(u32),
    /// A line every `n` bars: 1, 2, 4 and on.
    Bars(u32),
}

impl Grid {
    /// The ladder, finest first. Each rung has twice the spacing of the one
    /// before it, except beat to bar, which is the time signature.
    const LADDER: [Self; 14] = [
        Self::Beat(4),
        Self::Beat(2),
        Self::Beat(1),
        Self::Bars(1),
        Self::Bars(2),
        Self::Bars(4),
        Self::Bars(8),
        Self::Bars(16),
        Self::Bars(32),
        Self::Bars(64),
        Self::Bars(128),
        Self::Bars(256),
        Self::Bars(512),
        Self::Bars(1024),
    ];

    /// The pixels between two lines of this rung.
    fn spacing(self, beat: f32, bar: f32) -> f32 {
        match self {
            Self::Beat(n) => beat / n as f32,
            Self::Bars(n) => bar * n as f32,
        }
    }

    /// The finest rung whose lines are at least `minimum` pixels apart, and
    /// that spacing.
    fn fit(beat: f32, bar: f32, minimum: f32) -> (Self, f32) {
        Self::LADDER
            .iter()
            .map(|grid| (*grid, grid.spacing(beat, bar)))
            .find(|(_, spacing)| *spacing >= minimum)
            .unwrap_or((Self::Bars(1024), Self::Bars(1024).spacing(beat, bar)))
    }

    /// The rung the grid shows at `zoom`, and how far its lines have faded
    /// in: 0 when they have just enough room, 1 at twice that.
    pub(super) fn at(beats: &BeatGrid, zoom: f32) -> (Self, f32) {
        let (beat, bar) = mean_lengths(beats);
        let (grid, spacing) = Self::fit(beat * zoom, bar * zoom, MIN_GRID_SPACING);
        let fade = ((spacing - MIN_GRID_SPACING) / MIN_GRID_SPACING).clamp(0., 1.);
        // Smoothstep, so a rung eases in and out rather than ramping.
        (grid, fade * fade * (3. - 2. * fade))
    }
}

/// The smallest gap between two drawn grid lines, in pixels. A rung fades in
/// from here and is fully drawn at twice this.
const MIN_GRID_SPACING: f32 = 12.;

/// The smallest gap a snap target needs: a rung snaps once it is half faded
/// in, so a cursor never catches on a line the eye cannot see.
const MIN_SNAP_SPACING: f32 = 18.;

/// The mean beat and bar, in seconds.
///
/// The bar comes from the gap between the first two downbeats, and falls back
/// to the mean beat times the time signature with a single downbeat.
pub(super) fn mean_lengths(beats: &BeatGrid) -> (f32, f32) {
    let beat = if beats.beats.len() > 1 {
        (beats.beats[beats.beats.len() - 1] - beats.beats[0]) / (beats.beats.len() - 1) as f32
    } else {
        0.5
    };
    let bar = if beats.downbeats.len() > 1 {
        beats.downbeats[1] - beats.downbeats[0]
    } else {
        beat * if beats.beats_per_bar == 0 {
            4.
        } else {
            beats.beats_per_bar as f32
        }
    };
    (beat, bar)
}

#[cfg(test)]
mod tests {
    use super::{bar_snap, shift_time, Grid, MIN_GRID_SPACING};

    #[test]
    fn the_grid_climbs_the_ladder_as_the_zoom_drops() {
        // A 0.5 s beat in 4/4, in pixels at each zoom.
        let fit = |zoom: f32| Grid::fit(0.5 * zoom, 2. * zoom, MIN_GRID_SPACING).0;
        assert_eq!(fit(200.), Grid::Beat(4));
        assert_eq!(fit(50.), Grid::Beat(2));
        assert_eq!(fit(25.), Grid::Beat(1));
        assert_eq!(fit(10.), Grid::Bars(1));
        assert_eq!(fit(2.), Grid::Bars(4));
    }

    #[test]
    fn a_bar_snap_counts_its_blocks_from_bar_one() {
        let bars: Vec<f32> = (0..9).map(|bar| 1. + 2. * bar as f32).collect();
        assert_eq!(bar_snap(&bars, 4.9, 1), 5.);
        // Every 4th bar is 1 s and 9 s: 4.9 is nearer 1, 5.1 nearer 9.
        assert_eq!(bar_snap(&bars, 4.9, 4), 1.);
        assert_eq!(bar_snap(&bars, 5.1, 4), 9.);
        // Before bar 1 and past the last block.
        assert_eq!(bar_snap(&bars, 0., 4), 1.);
        assert_eq!(bar_snap(&bars, 30., 4), 17.);
    }

    /// Beats that alternate between 0.4285 s and 0.4286 s, like a detected
    /// grid stored to 0.1 ms.
    fn uneven() -> luma_patterns::BeatTimeline {
        let seconds = (0..40)
            .scan(0., |at, i| {
                let beat = *at;
                *at += if i % 2 == 0 { 0.4285 } else { 0.4286 };
                Some(beat)
            })
            .collect();
        luma_patterns::BeatTimeline::new(seconds, 0.).unwrap()
    }

    #[test]
    fn a_shift_moves_whole_beats_on_an_uneven_grid() {
        let clock = uneven();
        let at = |beat: f64| clock.seconds_at(beat).unwrap();
        // A one-beat clip moved from beat 0 to beat 1 must still end on a beat.
        let end = shift_time(Some(&clock), at(1.), at(0.), at(1.));
        assert!((clock.beat_at(end).unwrap() - 2.).abs() < 1e-9);
        // The seconds arithmetic this replaced lands 0.1 ms off the grid.
        assert!(((at(1.) + at(1.) - at(0.)) - at(2.)).abs() > 5e-5);
    }

    #[test]
    fn a_shift_of_nothing_keeps_the_exact_time() {
        let clock = uneven();
        let time = clock.seconds_at(2.5).unwrap() + 1e-13;
        assert_eq!(shift_time(Some(&clock), time, 3., 3.), time);
    }
}
