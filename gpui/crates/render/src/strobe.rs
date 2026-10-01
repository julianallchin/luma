//! A strobe is a train of flashes, and a frame shows a flash whole when one
//! begins within it.
//!
//! The flashes run on the frame's free-running clock ([`crate::Frame::time`]),
//! not on transport time, so a paused show keeps flashing the way a fixture on
//! a DMX line does. Every fixture with the same rate flashes in phase.
//!
//! A frame is the [`Slice`] of that clock since the frame before it. It shows
//! a strobing light at its full peak when a flash begins inside the slice,
//! and dark otherwise. A flash is a few milliseconds, shorter than any frame,
//! so a camera would record a fraction of the peak that depends on the frame
//! interval, and split a flash that straddles two frames between both. A
//! display has no business doing either: the same flash would brighten and
//! dim with frame-time jitter. Consecutive slices tile the clock, so each
//! flash lands in exactly one frame, at full brightness, at any frame rate.
//! At a rate above the frame rate, every frame holds an onset and the light
//! looks steady, as it does to an eye.

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

/// The part of the free-running clock one frame covers: `[open, open +
/// length)`. The next frame's slice opens where this one ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    /// Clock seconds at which the slice opens.
    pub open: f64,
    /// Seconds the slice lasts. Positive.
    pub length: f64,
}

/// A strobing light's share of its peak in `slice`: 1 when a flash begins
/// within it, 0 when none does, and 1 for a steady light.
#[must_use]
pub fn gain(strobe: f32, slice: Slice) -> f32 {
    let hz = hz(strobe);
    if hz <= 0.0 {
        return 1.0;
    }
    // In periods, flashes begin on whole numbers. A slice holds an onset when
    // the count of onsets before its close exceeds the count before its open.
    // Each frame computes its open from its own end and length, so the two
    // sides of one boundary can differ in the last bit; an onset within
    // `SNAP` periods of a boundary is put on it, so both sides agree that it
    // belongs to the later slice and it is neither dropped nor shown twice.
    let before = |t: f64| (t * hz - SNAP).ceil();
    let close = slice.open + slice.length.max(0.0);
    if before(close) > before(slice.open) {
        1.0
    } else {
        0.0
    }
}

/// How near a boundary, in periods, an onset counts as on it: far above the
/// rounding of a clock hours old, far below a flash.
const SNAP: f64 = 1e-6;

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(open: f64, length: f64) -> Slice {
        Slice { open, length }
    }

    #[test]
    fn a_steady_light_is_full_on() {
        assert_eq!(gain(0.0, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(-1.0, slice(3.0, 0.01)), 1.0);
        assert_eq!(gain(f32::NAN, slice(3.0, 0.01)), 1.0);
    }

    #[test]
    fn a_frame_holding_an_onset_shows_the_whole_peak() {
        // 10 Hz: flashes begin at 0.1, 0.2, ...
        assert_eq!(gain(0.5, slice(0.095, 0.02)), 1.0);
        // An onset on the opening edge belongs to the slice.
        assert_eq!(gain(0.5, slice(0.1, 0.01)), 1.0);
    }

    #[test]
    fn a_frame_without_an_onset_is_dark() {
        assert_eq!(gain(0.5, slice(0.115, 0.02)), 0.0);
        // An onset on the closing edge belongs to the next slice.
        assert_eq!(gain(0.5, slice(0.09, 0.01)), 0.0);
    }

    #[test]
    fn a_flash_across_a_frame_boundary_lands_once_in_its_onset_frame() {
        // The flash begins at 0.1 and lasts past the boundary at 0.105: the
        // frame it began in shows it, the next does not.
        let frames = [slice(0.09, 0.015), slice(0.105, 0.015)];
        assert_eq!(gain(0.5, frames[0]), 1.0);
        assert_eq!(gain(0.5, frames[1]), 0.0);
    }

    #[test]
    fn every_flash_lands_in_exactly_one_frame_at_any_frame_rate() {
        for strobe in [0.13_f32, 0.5, 0.77, 1.0] {
            let hz = hz(strobe);
            for fps in [24.0, 50.0, 59.94, 60.0, 75.0, 120.0, 144.0] {
                let length = 1.0 / fps;
                let (start, frames) = (3600.0, 600_u32);
                let lit = (0..frames)
                    .map(|n| gain(strobe, slice(start + f64::from(n) * length, length)))
                    .sum::<f32>();
                let end = start + f64::from(frames) * length;
                let onsets = ((end * hz).ceil() - (start * hz).ceil()) as f32;
                if hz < fps {
                    assert_eq!(lit, onsets, "{strobe} at {fps} fps");
                } else {
                    assert_eq!(lit, frames as f32, "{strobe} at {fps} fps");
                }
            }
        }
    }

    #[test]
    fn jittered_frame_intervals_do_not_change_a_flash_brightness() {
        // Irregular intervals between 10 and 20 ms: each flash still lands
        // once, at gain 1, and nothing between flashes lights.
        let strobe = 0.5;
        let mut open = 12.0;
        let mut lit = 0;
        for n in 0..1000_u32 {
            let length = 0.010 + 0.010 * f64::from((n * 7919) % 97) / 97.0;
            let g = gain(strobe, slice(open, length));
            assert!(g == 0.0 || g == 1.0, "{g}");
            lit += g as u32;
            open += length;
        }
        let onsets = ((open * hz(strobe)).ceil() - (12.0 * hz(strobe)).ceil()) as u32;
        assert_eq!(lit, onsets);
    }

    #[test]
    fn a_frame_rate_that_divides_the_flash_rate_lands_each_flash_once() {
        // 10 Hz at 60 fps: every sixth frame boundary is an onset, and the
        // slices' ends are computed, not exact.
        for (strobe, fps) in [(0.5_f32, 60.0), (1.0, 60.0), (0.25, 50.0), (0.5, 30.0)] {
            let hz = hz(strobe);
            let length = 1.0 / fps;
            let lit = (1..=600_u32)
                .map(|n| {
                    let end = f64::from(n) / fps;
                    gain(strobe, slice(end - length, length))
                })
                .sum::<f32>();
            let onsets = (600.0 / fps * hz).round() as f32;
            assert_eq!(lit, onsets.min(600.0), "{strobe} at {fps} fps");
        }
    }

    #[test]
    fn several_onsets_in_one_frame_are_still_one_peak() {
        assert_eq!(gain(1.0, slice(0.0, 0.5)), 1.0);
    }
}
