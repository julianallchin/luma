//! A strobe is a short flash once per period, and a frame sees the light the
//! flashes deliver while it is exposed.
//!
//! The flashes run on the frame's free-running clock ([`crate::Frame::time`]),
//! not on transport time, so a paused show keeps flashing the way a fixture on
//! a DMX line does. Every fixture with the same rate flashes in phase.
//!
//! A frame, or one subframe of it, is exposed over a [`Slice`] of that clock.
//! Its strobe intensity is the exact mean of the flash train over the slice,
//! in closed form. Point sampling would miss a 5 ms flash at almost any frame
//! rate, and it would make the brightness depend on the subframe count. The
//! mean over a slice split in two is the mean of the two halves, so 2
//! subframes and 16 give the same brightness.

/// Seconds one flash lasts. LED strobes flash for a few milliseconds, xenon
/// tubes for well under one; 5 ms is the LED end, and long enough that a
/// flash still shows as a band, not a line, under a 10 ms rolling readout.
/// The flash's peak is the dimmer value.
pub const FLASH_S: f64 = 0.005;

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

/// When one (sub)frame is exposed, on the free-running clock.
///
/// Row 0 is exposed over `[open, open + length]`. With a rolling shutter, row
/// `y` (0 at the top, 1 at the bottom) opens `y * readout` seconds later.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    /// Clock seconds at which row 0 opens.
    pub open: f64,
    /// Seconds each row stays open. Positive.
    pub length: f64,
    /// Seconds from the first row opening to the last. Zero for a global
    /// shutter.
    pub readout: f64,
}

/// A strobe's exposure within a [`Slice`], one number per frame plus the
/// shape of its variation down the rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exposure {
    /// The mean fraction of the peak over the slice, 0..=1. With a readout it
    /// is the mean over the whole readout span, rescaled to one row's length:
    /// an upper bound on every row's own mean, so a cone that lights some row
    /// is never culled. [`Rows::ratio`] takes it down to each row's value.
    pub gain: f32,
    /// How each row's exposure relates to [`Self::gain`].
    pub rows: Rows,
}

/// Row `y`'s exposure over [`Exposure::gain`], for a rolling shutter.
///
/// In periods of the flash train: row `y` sees `[phase + y * readout, … +
/// span]`, a flash is the first `duty` of each period, and `norm` divides the
/// light that window receives by the light behind [`Exposure::gain`].
/// `norm == 0` means every row sees the same light (ratio 1). Mirrored in
/// `shaders/fixture_light.wgsl` (`strobe_row_ratio`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rows {
    /// Where row 0's window opens, as a fraction of a period.
    pub phase: f32,
    /// One row's window, in periods.
    pub span: f32,
    /// The whole readout, in periods.
    pub readout: f32,
    /// The lit fraction of a period.
    pub duty: f32,
    /// One over the periods' worth of light behind [`Exposure::gain`].
    pub norm: f32,
}

impl Rows {
    /// Every row sees the same light.
    pub const STEADY: Self = Self {
        phase: 0.0,
        span: 0.0,
        readout: 0.0,
        duty: 0.0,
        norm: 0.0,
    };

    /// Row `y`'s exposure over the slice's [`Exposure::gain`], 0..=1.
    #[must_use]
    pub fn ratio(&self, y: f32) -> f32 {
        if self.norm <= 0.0 {
            return 1.0;
        }
        let a = f64::from(self.phase) + f64::from(y.clamp(0.0, 1.0)) * f64::from(self.readout);
        let b = a + f64::from(self.span);
        let duty = f64::from(self.duty);
        ((on_time(b, duty) - on_time(a, duty)) * f64::from(self.norm)).clamp(0.0, 1.0) as f32
    }
}

/// On-time of a flash train from 0 to `x` periods, in periods: each period
/// is lit for its first `duty`.
fn on_time(x: f64, duty: f64) -> f64 {
    let whole = x.floor();
    whole * duty + (x - whole).min(duty)
}

impl Exposure {
    /// The steady light: full peak on every row.
    pub const STEADY: Self = Self {
        gain: 1.0,
        rows: Rows::STEADY,
    };

    /// A strobe at `strobe` exposed over `slice`.
    #[must_use]
    pub fn of(strobe: f32, slice: Slice) -> Self {
        let hz = hz(strobe);
        let length = slice.length.max(1e-6);
        let duty = FLASH_S * hz;
        if hz <= 0.0 || duty >= 1.0 {
            return Self::STEADY;
        }
        let readout = slice.readout.max(0.0);
        // Everything in periods from here: the train repeats every one, so
        // the whole part of the open time only adds whole flashes and the
        // fractional part keeps its precision on a clock hours old.
        let start = slice.open * hz;
        let phase = start - start.floor();
        let span = length * hz;
        let reach = (length + readout) * hz;
        let light = on_time(phase + reach, duty) - on_time(phase, duty);
        let gain = (light / span).min(1.0);
        let rows = if readout > 0.0 && light > 0.0 {
            Rows {
                phase: phase as f32,
                span: span as f32,
                readout: (readout * hz) as f32,
                duty: duty as f32,
                norm: (1.0 / (gain * span)) as f32,
            }
        } else {
            Rows::STEADY
        };
        Self {
            gain: gain as f32,
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(open: f64, length: f64) -> Slice {
        Slice {
            open,
            length,
            readout: 0.0,
        }
    }

    #[test]
    fn a_steady_light_is_full_on_every_row() {
        assert_eq!(Exposure::of(0.0, slice(3.0, 0.01)), Exposure::STEADY);
    }

    #[test]
    fn a_frame_that_holds_one_whole_flash_gets_its_share_of_the_peak() {
        // 10 Hz flashes at 0.1 s; the frame [0.095, 0.115] holds one.
        let exposure = Exposure::of(0.5, slice(0.095, 0.02));
        assert!((f64::from(exposure.gain) - FLASH_S / 0.02).abs() < 1e-6);
        // And the frame after it holds none.
        assert_eq!(Exposure::of(0.5, slice(0.115, 0.02)).gain, 0.0);
    }

    #[test]
    fn the_mean_does_not_depend_on_how_the_frame_is_split() {
        for strobe in [0.13_f32, 0.5, 0.77, 1.0] {
            for open in [0.0, 0.0973, 1.37, 3600.001] {
                let whole = f64::from(Exposure::of(strobe, slice(open, 1.0 / 60.0)).gain);
                for parts in [2_u32, 16] {
                    let length = 1.0 / 60.0 / f64::from(parts);
                    let split = (0..parts)
                        .map(|k| {
                            f64::from(
                                Exposure::of(strobe, slice(open + f64::from(k) * length, length))
                                    .gain,
                            )
                        })
                        .sum::<f64>()
                        / f64::from(parts);
                    assert!(
                        (split - whole).abs() < 1e-4,
                        "{strobe} at {open}: whole {whole}, {parts} parts {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn over_a_long_window_the_mean_is_the_duty_cycle() {
        let exposure = Exposure::of(0.5, slice(12.345, 10.0));
        assert!((f64::from(exposure.gain) - FLASH_S * 10.0).abs() < 1e-3);
    }

    #[test]
    fn rolling_rows_bound_by_the_gain_and_band_the_flash() {
        // 10 Hz, one flash at 0.1 s. Rows open 0 to 10 ms after 0.09 and stay
        // open 8 ms, so the top rows close before the flash and the bottom
        // rows hold all of it.
        let exposure = Exposure::of(
            0.5,
            Slice {
                open: 0.09,
                length: 0.008,
                readout: 0.01,
            },
        );
        let lit = |y: f32| exposure.gain * exposure.rows.ratio(y);
        assert_eq!(lit(0.0), 0.0);
        let bottom = f64::from(lit(1.0));
        assert!((bottom - FLASH_S / 0.008).abs() < 1e-4, "{bottom}");
        for step in 0..=20 {
            let ratio = exposure.rows.ratio(step as f32 / 20.0);
            assert!((0.0..=1.0).contains(&ratio));
        }
    }

    #[test]
    fn the_mean_over_the_rows_matches_a_global_shutter_of_the_same_span() {
        // Each row's window is the global window shifted; averaged over the
        // rows it is the mean over the readout span, whatever the phase.
        let rolling = Slice {
            open: 5.0123,
            length: 1.0 / 120.0,
            readout: 0.01,
        };
        let exposure = Exposure::of(0.9, rolling);
        let rows = 4000;
        let mean = (0..rows)
            .map(|r| {
                let y = (r as f32 + 0.5) / rows as f32;
                f64::from(exposure.gain * exposure.rows.ratio(y))
            })
            .sum::<f64>()
            / f64::from(rows);
        let reference = (0..rows)
            .map(|r| {
                let y = (f64::from(r) + 0.5) / f64::from(rows);
                f64::from(Exposure::of(0.9, slice(rolling.open + y * 0.01, rolling.length)).gain)
            })
            .sum::<f64>()
            / f64::from(rows);
        assert!((mean - reference).abs() < 1e-4, "{mean} vs {reference}");
    }
}
