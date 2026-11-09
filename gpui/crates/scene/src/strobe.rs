//! A strobe the DMX engine makes itself is a gate on a cell's brightness,
//! stepped once per DMX output frame ([`FRAME_S`]): at `hz = strobe * 20` the
//! period is rounded to whole frames, at least two, and the gate is on for
//! the first half of them (rounded up). A flash is therefore whole frames on
//! the wire, never a frame the output tick happened to sample on one side of
//! an edge, and the fastest strobe is one frame on, one off.
//!
//! The gate runs on [`clock`], one free-running clock for the whole process,
//! not on transport time: a paused show keeps flashing the way a fixture on a
//! DMX line does, every cell with the same rate flashes in phase, and the
//! screen flashes in phase with the rig. The output tick fires mid-frame on
//! this clock ([`next_frame_middle`]), so each tick samples one frame.
//!
//! A screen frame is the [`Slice`] of that clock since the frame before it.
//! Its gain is the time the gate is on within the slice, divided by the
//! slice's length: an exact box filter over the gate. Consecutive slices tile
//! the clock, so the light the frames show adds up to the light the rig gives,
//! whatever the frame rate and however the intervals jitter. A rate faster
//! than the screen can show becomes a steady glow at about half brightness.

use std::sync::OnceLock;
use std::time::Instant;

/// Flashes per second per unit of `PrimitiveState::strobe`, for every fixture
/// type.
///
/// Only 189 of the 1031 bundled QLC+ definitions with a strobe capability give
/// its rate in hertz, so the definitions cannot be the source. The DMX engine
/// writes a shutter's rate and gates a strobe of its own with this, so the
/// picture and the output agree on the rate.
pub const HZ_PER_UNIT: f64 = 20.0;

/// Flashes per second at a strobe value; zero when the light is steady.
#[must_use]
pub fn hz(strobe: f32) -> f64 {
    if strobe.is_finite() && strobe > 0.0 {
        f64::from(strobe) * HZ_PER_UNIT
    } else {
        0.0
    }
}

/// Seconds on the process's strobe clock: the time the DMX engine gates its
/// strobes on and the live view draws them on. It starts at the first call.
#[must_use]
pub fn clock() -> f64 {
    epoch().elapsed().as_secs_f64()
}

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// The part of the free-running clock one frame covers: `[open, open +
/// length)`. The next frame's slice opens where this one ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    /// Clock seconds at which the slice opens.
    pub open: f64,
    /// Seconds the slice lasts. Positive.
    pub length: f64,
}

/// Seconds per DMX output frame: the tick the engine renders at and the
/// step its strobe gate moves in. About 43 frames a second, under DMX512's
/// 44 for a full universe.
pub const FRAME_S: f64 = 0.023;

/// Output frames per flash at `hz`: the period rounded to whole frames, at
/// least one on and one off.
fn frames_per_flash(hz: f64) -> i64 {
    ((1.0 / (hz * FRAME_S)).round() as i64).max(2)
}

/// Seconds the gate of an `n`-frame flash has been on between 0 and `t`.
fn on_before(t: f64, n: i64) -> f64 {
    let lit = (n + 1) / 2;
    let frames = t / FRAME_S;
    let whole = frames.floor();
    let period = (whole / n as f64).floor();
    let into = (whole - period * n as f64) as i64;
    let on =
        period * lit as f64 + into.min(lit) as f64 + if into < lit { frames - whole } else { 0.0 };
    on * FRAME_S
}

/// A strobing light's share of its steady brightness in `slice`: the
/// fraction of the slice during which its gate is on. 1 for a steady light.
/// A slice of no length takes the gate at its open, as the output tick does.
#[must_use]
pub fn gain(strobe: f32, slice: Slice) -> f32 {
    let hz = hz(strobe);
    if hz <= 0.0 {
        return 1.0;
    }
    let n = frames_per_flash(hz);
    if !(slice.length > 0.0) || !slice.length.is_finite() {
        let frame = (slice.open / FRAME_S).floor() as i64;
        return if frame.rem_euclid(n) < (n + 1) / 2 {
            1.0
        } else {
            0.0
        };
    }
    let lit = on_before(slice.open + slice.length, n) - on_before(slice.open, n);
    (lit / slice.length).clamp(0.0, 1.0) as f32
}

/// The first moment after `t` on [`clock`] that is the middle of an output
/// frame: where the output tick samples the gate, half a frame from either
/// edge.
#[must_use]
pub fn next_frame_middle(t: f64) -> f64 {
    let middle = ((t / FRAME_S).floor() + 0.5) * FRAME_S;
    if middle > t {
        middle
    } else {
        middle + FRAME_S
    }
}

/// [`clock`]'s zero as an [`Instant`], for scheduling on it.
#[must_use]
pub fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

/// The lowest and highest rate a fixture's own strobe runs at across its
/// strobe range. A generic stand-in until fixture profiles carry the real
/// curve: most budget fixtures span about this, and the DMX engine writes the
/// rate linearly across the same range.
pub const FIXTURE_HZ: (f64, f64) = (1.0, 20.0);

/// Flashes per second a fixture's own strobe runs at for a strobe value;
/// zero when the shutter is open.
#[must_use]
pub fn fixture_hz(strobe: f32) -> f64 {
    if strobe.is_finite() && strobe > 0.0 {
        let (low, high) = FIXTURE_HZ;
        low + f64::from(strobe.min(1.0)) * (high - low)
    } else {
        0.0
    }
}

/// A fixture that strobes on its own clock, as the rig does when the DMX
/// engine writes its strobe channel rather than pulsing its dimmer.
///
/// It behaves like a budget fixture, the worst case: a strobe starts the
/// moment the shutter leaves open, but a new rate only takes effect once the
/// current flash cycle ends. A rate that changes every frame therefore
/// flashes on screen exactly as rarely as it does on the rig. Each cycle is
/// on for its first half, as [`gain`]'s gate is.
#[derive(Debug, Clone, Default)]
pub struct FixtureClock {
    /// The running cycle as `(start, period)`; `None` while the shutter is open.
    cycle: Option<(f64, f64)>,
    /// The last slice answered, and its answer, so a frame that asks for one
    /// head twice does not run the clock twice.
    answered: Option<(Slice, f32)>,
}

impl FixtureClock {
    /// The share of its steady brightness the light gives over `slice`, with
    /// `strobe` the rate asked for during it. Slices are expected in order;
    /// asking for the same slice again repeats the answer.
    pub fn gain(&mut self, strobe: f32, slice: Slice) -> f32 {
        if let Some((seen, answer)) = self.answered {
            if seen == slice {
                return answer;
            }
        }
        let answer = self.advance(fixture_hz(strobe), slice);
        self.answered = Some((slice, answer));
        answer
    }

    fn advance(&mut self, hz: f64, slice: Slice) -> f32 {
        if hz <= 0.0 {
            self.cycle = None;
            return 1.0;
        }
        let period = 1.0 / hz;
        let open = slice.open;
        let length = if slice.length.is_finite() && slice.length > 0.0 {
            slice.length
        } else {
            0.0
        };
        let end = open + length;
        let (mut start, mut current) = *self.cycle.get_or_insert((open, period));
        // Catch up to the slice: the cycle in flight finishes at its own
        // rate, then every later one runs at the rate asked for now.
        if start + current <= open {
            start += current;
            current = period;
            if start + current <= open {
                start += ((open - start) / current).floor() * current;
            }
        }
        let mut lit = 0.0;
        let mut t = open;
        loop {
            let on_until = start + current * 0.5;
            let cycle_end = start + current;
            if length == 0.0 {
                lit = if t < on_until { 1.0 } else { 0.0 };
                break;
            }
            lit += (on_until.min(end) - t.max(start)).max(0.0);
            if cycle_end >= end {
                break;
            }
            t = cycle_end;
            start = cycle_end;
            current = period;
        }
        self.cycle = Some((start, current));
        if length == 0.0 {
            lit as f32
        } else {
            (lit / length).clamp(0.0, 1.0) as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(open: f64, length: f64) -> Slice {
        Slice { open, length }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    /// The gate sampled at `t`: what the output tick sends.
    fn gate(strobe: f32, t: f64) -> bool {
        gain(strobe, slice(t, 0.0)) == 1.0
    }

    #[test]
    fn a_steady_light_is_full_on() {
        assert_eq!(gain(0.0, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(-1.0, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(f32::NAN, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(f32::INFINITY, slice(3.0, 0.01)), 1.0);
    }

    /// Every output frame is wholly on or wholly off: sampling anywhere
    /// inside a frame gives the same answer.
    #[test]
    fn the_gate_steps_once_per_output_frame() {
        for strobe in [0.05_f32, 0.3, 0.5, 0.77, 1.0] {
            for frame in 1000..1100_i32 {
                let start = f64::from(frame) * FRAME_S;
                let at = gate(strobe, start + FRAME_S * 0.01);
                for k in 1..10 {
                    let t = start + FRAME_S * f64::from(k) / 10.0;
                    assert_eq!(gate(strobe, t), at, "{strobe} {frame}");
                }
            }
        }
    }

    /// Flashes repeat exactly: every period is the same run of on frames then
    /// off frames, on for at least half of it, and the fastest strobe is one
    /// frame on and one off.
    #[test]
    fn every_flash_is_the_same_whole_frames() {
        for strobe in [0.05_f32, 0.2, 0.5, 0.9, 1.0] {
            let sent: Vec<bool> = (0..2000)
                .map(|k| gate(strobe, (f64::from(k) + 0.5) * FRAME_S))
                .collect();
            let n = frames_per_flash(hz(strobe)) as usize;
            assert!(n >= 2);
            for k in n..sent.len() {
                assert_eq!(sent[k], sent[k - n], "{strobe} frame {k}");
            }
            let on = sent[..n].iter().filter(|&&v| v).count();
            assert!(on * 2 >= n && on < n, "{strobe}: {on} of {n}");
        }
        assert_eq!(frames_per_flash(hz(1.0)), 2);
    }

    /// The rate is the asked rate to the nearest whole frame.
    #[test]
    fn the_flash_rate_is_the_nearest_whole_frames() {
        for strobe in [0.05_f32, 0.2, 0.5] {
            let period = frames_per_flash(hz(strobe)) as f64 * FRAME_S;
            assert!(
                (period - 1.0 / hz(strobe)).abs() <= FRAME_S / 2.0 + 1e-12,
                "{strobe}"
            );
        }
    }

    #[test]
    fn a_slice_of_no_length_takes_the_gate_at_its_open() {
        for (strobe, t) in [(0.5_f32, 0.12), (0.5, 0.17), (0.13, 3.3)] {
            let expected = gain(strobe, slice(t, 0.0));
            assert_eq!(gain(strobe, slice(t, -1.0)), expected, "{strobe} {t}");
            assert_eq!(gain(strobe, slice(t, f64::NAN)), expected, "{strobe} {t}");
        }
    }

    #[test]
    fn the_gain_is_the_gate_sampled_finely() {
        for strobe in [0.13_f32, 0.5, 0.77, 1.0] {
            for (open, length) in [
                (0.0, 1.0 / 60.0),
                (2.345, 1.0 / 45.0),
                (17.0, 0.0071),
                (3.0, 0.4),
            ] {
                let steps = 200_000_u32;
                let on = (0..steps)
                    .filter(|&n| {
                        gate(
                            strobe,
                            open + length * (f64::from(n) + 0.5) / f64::from(steps),
                        )
                    })
                    .count() as f32
                    / steps as f32;
                let g = gain(strobe, slice(open, length));
                assert!((g - on).abs() < 1e-3, "{strobe} {open}: {g} vs {on}");
            }
        }
    }

    /// Screen frames at any rate, regular or jittered, show the light the
    /// output frames give over the same time.
    #[test]
    fn frames_at_any_frame_rate_show_the_light_the_rig_gives() {
        for strobe in [0.13_f32, 0.25, 0.5, 0.77, 1.0] {
            let n = frames_per_flash(hz(strobe));
            let start = 3600.0;
            let span = 10.0;
            let wire = (on_before(start + span, n) - on_before(start, n)) / span;
            for fps in [45.0, 50.0, 59.94, 60.0, 75.0, 120.0, 144.0] {
                let length = 1.0 / fps;
                let frames = (span * fps) as u32;
                let light: f64 = (0..frames)
                    .map(|k| {
                        f64::from(gain(strobe, slice(start + f64::from(k) * length, length)))
                            * length
                    })
                    .sum();
                let shown = light / (f64::from(frames) * length);
                assert!(
                    (shown - wire).abs() < 0.01,
                    "{strobe} at {fps} fps: {shown} vs {wire}"
                );
            }
            let mut open = start;
            let mut light = 0.0;
            for k in 0..3000_u32 {
                let length = 0.007 + 0.015 * f64::from((k * 7919) % 97) / 97.0;
                let g = gain(strobe, slice(open, length));
                assert!((0.0..=1.0).contains(&g), "{g}");
                light += f64::from(g) * length;
                open += length;
            }
            let jittered = light / (open - start);
            assert!(
                (jittered - wire).abs() < 0.02,
                "{strobe} jittered: {jittered} vs {wire}"
            );
        }
    }

    #[test]
    fn the_output_tick_lands_mid_frame() {
        for t in [0.0, 0.0115, 0.0116, 1.2345, 3600.0] {
            let m = next_frame_middle(t);
            assert!(m > t && m - t <= FRAME_S + 1e-12, "{t}");
            let into = (m / FRAME_S).fract();
            assert!((into - 0.5).abs() < 1e-6, "{t}: {into}");
        }
    }

    #[test]
    fn the_clock_runs_forward() {
        let a = clock();
        let b = clock();
        assert!(b >= a && a >= 0.0);
    }
    #[test]
    fn a_fixture_clock_is_steady_while_the_shutter_is_open() {
        let mut clock = FixtureClock::default();
        assert_eq!(clock.gain(0.0, slice(1.0, 0.02)), 1.0);
        assert_eq!(clock.gain(-1.0, slice(1.02, 0.02)), 1.0);
    }

    #[test]
    fn a_fixture_strobe_starts_with_a_flash_and_is_lit_half_the_time() {
        let mut clock = FixtureClock::default();
        // 0.5 is 10.5 Hz; the first cycle opens with the frame.
        assert_eq!(clock.gain(0.5, slice(2.0, 0.01)), 1.0);
        let frames = 2_000;
        let length = 1.0 / 60.0;
        let mut light = 0.0;
        for n in 0..frames {
            let open = 2.01 + f64::from(n) * length;
            light += f64::from(clock.gain(0.5, slice(open, length))) * length;
        }
        let share = light / (f64::from(frames) * length);
        assert!((share - 0.5).abs() < 0.01, "{share}");
    }

    #[test]
    fn a_new_rate_waits_for_the_cycle_in_flight() {
        let mut clock = FixtureClock::default();
        // The slowest rate: a one-second cycle, on for its first half.
        let slow = 1e-6_f32;
        assert_eq!(clock.gain(slow, slice(0.0, 0.02)), 1.0);
        // Asked to go fast at 0.6 s, inside the slow cycle's dark half, it
        // stays dark until that cycle ends at 1.0 s.
        for n in 0..19 {
            let open = 0.6 + f64::from(n) * 0.02;
            assert_eq!(clock.gain(1.0, slice(open, 0.02)), 0.0, "{open}");
        }
        // Then flashes at 20 Hz: on for [1.0, 1.025).
        assert_eq!(clock.gain(1.0, slice(1.0, 0.02)), 1.0);
        // The shared clock would have flashed long before.
        assert!(gain(1.0, slice(0.6, 0.4)) > 0.4);
    }

    #[test]
    fn asking_for_the_same_slice_twice_does_not_run_the_clock_twice() {
        let mut clock = FixtureClock::default();
        let first = clock.gain(0.5, slice(5.0, 0.03));
        assert_eq!(clock.gain(0.5, slice(5.0, 0.03)), first);
        let mut fresh = FixtureClock::default();
        fresh.gain(0.5, slice(5.0, 0.03));
        assert_eq!(
            clock.gain(0.5, slice(5.03, 0.03)),
            fresh.gain(0.5, slice(5.03, 0.03))
        );
    }

    #[test]
    fn opening_the_shutter_ends_the_strobe_at_once() {
        let mut clock = FixtureClock::default();
        clock.gain(1e-6, slice(0.0, 0.02));
        assert_eq!(clock.gain(1e-6, slice(0.6, 0.02)), 0.0);
        assert_eq!(clock.gain(0.0, slice(0.62, 0.02)), 1.0);
        // And a strobe after it starts afresh, with a flash.
        assert_eq!(clock.gain(1e-6, slice(0.64, 0.02)), 1.0);
    }
}
