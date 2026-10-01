//! A strobe is a square-wave gate, and a frame shows the share of its slice
//! of the clock during which the gate is on.
//!
//! The gate is the DMX engine's (`backend/src/fixtures/engine.rs`): at `hz =
//! strobe * 20` it is on for the first half of every period and off for the
//! second, so a strobing light is on half the time at its steady brightness.
//! It runs on [`clock`], one free-running clock for the whole process, not on
//! transport time: a paused show keeps flashing the way a fixture on a DMX
//! line does, every fixture with the same rate flashes in phase, and the
//! screen flashes in phase with the rig.
//!
//! A frame is the [`Slice`] of that clock since the frame before it. Its gain
//! is the time the gate is on within the slice, divided by the slice's length:
//! an exact box filter over the square wave. Consecutive slices tile the
//! clock, so the light the frames show adds up to the light the rig gives,
//! whatever the frame rate and however the intervals jitter. A flash that a
//! frame holds whole is as bright as the steady light; a flash cut by a frame
//! boundary is shared between the two frames; on average a strobe is half as
//! bright as the steady light, as on the rig. A rate faster than the screen
//! can show becomes a steady glow at about half brightness.

use std::sync::OnceLock;
use std::time::Instant;

/// Flashes per second per unit of `PrimitiveState::strobe`, for every fixture
/// type.
///
/// Only 189 of the 1031 bundled QLC+ definitions with a strobe capability give
/// its rate in hertz, so the definitions cannot be the source. This is the
/// DMX engine's `STROBE_HZ_MAX` (`backend/src/fixtures/engine.rs`): the
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
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// The part of the free-running clock one frame covers: `[open, open +
/// length)`. The next frame's slice opens where this one ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    /// Clock seconds at which the slice opens.
    pub open: f64,
    /// Seconds the slice lasts. Positive.
    pub length: f64,
}

/// A strobing light's share of its steady brightness in `slice`: the
/// fraction of the slice during which its gate is on. 1 for a steady light.
///
/// The gate is on while `t mod period <= period / 2`, as on the DMX line. A
/// slice of no length takes the gate at its open.
#[must_use]
pub fn gain(strobe: f32, slice: Slice) -> f32 {
    let hz = hz(strobe);
    if hz <= 0.0 {
        return 1.0;
    }
    // In periods, the gate is on over [n, n + 1/2] for every whole n.
    let open = slice.open * hz;
    let length = slice.length * hz;
    if !(length > 0.0) || !length.is_finite() {
        return if open - open.floor() <= 0.5 { 1.0 } else { 0.0 };
    }
    let lit = on_before(open + length) - on_before(open);
    (lit / length).clamp(0.0, 1.0) as f32
}

/// Periods the gate has been on between 0 and `u` periods.
fn on_before(u: f64) -> f64 {
    let whole = u.floor();
    whole * 0.5 + (u - whole).min(0.5)
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

    /// The engine's gate, as `backend/src/fixtures/engine.rs` writes it.
    fn engine_gate(strobe: f32, t: f64) -> bool {
        let period = 1.0 / (f64::from(strobe) * HZ_PER_UNIT);
        t.rem_euclid(period) <= period * 0.5
    }

    #[test]
    fn a_steady_light_is_full_on() {
        assert_eq!(gain(0.0, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(-1.0, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(f32::NAN, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(f32::INFINITY, slice(3.0, 0.01)), 1.0);
    }

    #[test]
    fn a_frame_inside_the_on_half_is_as_bright_as_the_steady_light() {
        // 10 Hz: the gate is on over [0.1, 0.15], off over (0.15, 0.2).
        assert_eq!(gain(0.5, slice(0.105, 0.04)), 1.0);
    }

    #[test]
    fn a_frame_inside_the_off_half_is_dark() {
        assert_eq!(gain(0.5, slice(0.155, 0.04)), 0.0);
    }

    #[test]
    fn a_frame_across_an_edge_shows_the_share_that_is_on() {
        // On from 0.1 to 0.15: a frame over [0.14, 0.16) is on half of it.
        assert!(near(gain(0.5, slice(0.14, 0.02)), 0.5));
        // Off until 0.2: a frame over [0.19, 0.21) is on for its last half.
        assert!(near(gain(0.5, slice(0.19, 0.02)), 0.5));
        // [0.13, 0.16): on for 0.02 of 0.03.
        assert!(near(gain(0.5, slice(0.13, 0.03)), 2.0 / 3.0));
    }

    #[test]
    fn a_flash_cut_by_a_frame_boundary_is_shared_between_both_frames() {
        // The flash over [0.1, 0.15] crosses the boundary at 0.125.
        let first = gain(0.5, slice(0.09, 0.035));
        let second = gain(0.5, slice(0.125, 0.035));
        let light = f64::from(first) * 0.035 + f64::from(second) * 0.035;
        assert!((light - 0.05).abs() < 1e-6, "{light}");
    }

    #[test]
    fn a_whole_period_is_lit_half_the_time() {
        for strobe in [0.05_f32, 0.5, 1.0] {
            let period = 1.0 / hz(strobe);
            for open in [0.0, 0.0123, 7.77, 3600.25] {
                assert!(
                    near(gain(strobe, slice(open, period)), 0.5),
                    "{strobe} {open}"
                );
                assert!(
                    near(gain(strobe, slice(open, 3.0 * period)), 0.5),
                    "{strobe} {open}"
                );
            }
        }
    }

    #[test]
    fn a_slice_of_no_length_takes_the_gate_at_its_open() {
        for (strobe, t) in [(0.5_f32, 0.12), (0.5, 0.17), (0.5, 0.15), (0.13, 3.3)] {
            let expected = if engine_gate(strobe, t) { 1.0 } else { 0.0 };
            assert_eq!(gain(strobe, slice(t, 0.0)), expected, "{strobe} {t}");
            assert_eq!(gain(strobe, slice(t, -1.0)), expected, "{strobe} {t}");
            assert_eq!(gain(strobe, slice(t, f64::NAN)), expected, "{strobe} {t}");
        }
    }

    #[test]
    fn the_gain_matches_the_engine_gate_sampled_finely() {
        // A numerical integral of the engine's own gate.
        for strobe in [0.13_f32, 0.5, 0.77, 1.0] {
            for (open, length) in [(0.0, 1.0 / 60.0), (2.345, 1.0 / 45.0), (17.0, 0.0071)] {
                let steps = 200_000_u32;
                let on = (0..steps)
                    .filter(|&n| {
                        let t = open + length * (f64::from(n) + 0.5) / f64::from(steps);
                        engine_gate(strobe, t)
                    })
                    .count() as f32
                    / steps as f32;
                let g = gain(strobe, slice(open, length));
                assert!((g - on).abs() < 1e-3, "{strobe} {open}: {g} vs {on}");
            }
        }
    }

    #[test]
    fn frames_at_any_frame_rate_show_the_light_the_rig_gives() {
        // Over 10 s, the light the frames show is the gate's on time, which
        // is half the time, at any frame rate.
        for strobe in [0.13_f32, 0.25, 0.5, 0.77, 1.0] {
            for fps in [45.0, 50.0, 59.94, 60.0, 75.0, 120.0, 144.0] {
                let length = 1.0 / fps;
                let start = 3600.0;
                let frames = (10.0 * fps) as u32;
                let light = (0..frames)
                    .map(|n| {
                        f64::from(gain(strobe, slice(start + f64::from(n) * length, length)))
                            * length
                    })
                    .sum::<f64>();
                let span = f64::from(frames) * length;
                let expected = on_before((start + span) * hz(strobe)) / hz(strobe)
                    - on_before(start * hz(strobe)) / hz(strobe);
                assert!((light - expected).abs() < 1e-4, "{strobe} at {fps} fps");
                // A part period at the end may be lit more or less than half.
                let mean = light / span;
                let part = 0.5 / (hz(strobe) * span);
                assert!((mean - 0.5).abs() <= part, "{strobe} at {fps} fps: {mean}");
            }
        }
    }

    #[test]
    fn a_flash_held_whole_by_one_frame_is_as_bright_at_any_frame_rate() {
        // 2 Hz flashes last 0.25 s, longer than any frame: a frame inside one
        // shows the steady light at 45 fps and at 144 fps alike.
        for fps in [45.0, 59.94, 60.0, 75.0, 120.0, 144.0] {
            assert_eq!(gain(0.1, slice(10.05, 1.0 / fps)), 1.0, "{fps}");
        }
    }

    #[test]
    fn jittered_frame_intervals_keep_the_average_at_half() {
        // Irregular intervals between 7 and 22 ms.
        for strobe in [0.25_f32, 0.5, 1.0] {
            let mut open = 12.0;
            let mut light = 0.0;
            for n in 0..3000_u32 {
                let length = 0.007 + 0.015 * f64::from((n * 7919) % 97) / 97.0;
                let g = gain(strobe, slice(open, length));
                assert!((0.0..=1.0).contains(&g), "{g}");
                light += f64::from(g) * length;
                open += length;
            }
            let mean = light / (open - 12.0);
            assert!((mean - 0.5).abs() < 0.01, "{strobe}: {mean}");
        }
    }

    #[test]
    fn a_rate_above_the_frame_rate_is_a_steady_glow_at_about_half() {
        // 20 Hz at 30 fps: every frame holds 2/3 of a period, so it is lit
        // for at least a quarter and at most three quarters of it.
        for n in 0..300_u32 {
            let g = gain(1.0, slice(5.0 + f64::from(n) / 30.0, 1.0 / 30.0));
            assert!((0.25..=0.75).contains(&g), "{n}: {g}");
        }
        // A slice of many periods is lit half of it.
        assert!(near(gain(1.0, slice(0.0, 0.5)), 0.5));
    }

    #[test]
    fn the_clock_runs_forward() {
        let a = clock();
        let b = clock();
        assert!(b >= a && a >= 0.0);
    }
}
