//! How many pixels the stage renders: the fixed pixel budget, the
//! environment overrides, the operator's percent and the share of them a
//! quality level's frame rate leaves it.
use super::*;

/// The most pixels the stage renders, whatever the size of its element.
///
/// The live renderer costs per pixel: fullscreen 3600×2260 took 2.3× the GPU
/// time of the 1984×1511 window and fell to 27 fps. Past this budget the stage
/// renders smaller at the same aspect and the compositor upscales it (MetalFX
/// Spatial on macOS — see `Window::paint_upscaled_surface`). The budget is the
/// reference frame the renderer's timings are held to, so an ordinary window
/// stays native.
const RENDER_BUDGET_PIXELS: f64 = 2227.0 * 1391.0;

/// A quality level's dynamic resolution: the share of its pixels the stage
/// renders, driven by how long its frames take against the frame rate the
/// level holds.
///
/// A frame's time is from its submission to its completion, since one frame
/// is in flight at a time and a frame that is not home by the refresh it was
/// meant for is shown a whole refresh late. So the stage keeps clear of the
/// time a frame is allowed: a second of frames that use more than
/// [`Self::LATE`] of it take pixels away, as many as the overrun calls for,
/// and two seconds of frames under [`Self::EARLY`] of it give a tenth back.
/// A second, so that a venue loading or a window coming forward costs
/// nothing. Between the two nothing moves, and a step either way lands there,
/// because a resize discards the haze caches and a size that hunts would
/// never settle. A quarter of the pixels is the floor.
///
/// Pixels are only given up while they are what the frame waits on. A step
/// that buys less than [`Self::WORTH`] of what its pixels would, were the
/// whole frame's time theirs, is taken back, and frames that slow at that
/// size are left alone after it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DynamicBudget {
    factor: f64,
    /// Late frames in a row, and their time together.
    late: u32,
    late_ms: f32,
    /// Early frames in a row.
    early: u32,
    /// Frames still to pass over after a step: those already in flight at
    /// the old size, and the first at the new one, which rebuild caches.
    settling: u32,
    /// The share and the late frames' mean time the last step down started
    /// from, while nothing but late frames has followed it.
    stepped: Option<(f64, f32)>,
    /// The share and mean frame time at which fewer pixels bought nothing.
    futile: Option<(f64, f32)>,
}

impl Default for DynamicBudget {
    fn default() -> Self {
        Self {
            factor: 1.0,
            late: 0,
            late_ms: 0.0,
            early: 0,
            settling: 0,
            stepped: None,
            futile: None,
        }
    }
}

impl DynamicBudget {
    /// The share of a frame's allowed time past which it counts as late,
    /// under which it counts as early, and that a step down aims for.
    const LATE: f32 = 0.9;
    const EARLY: f32 = 0.65;
    const AIM: f32 = 0.8;
    /// The least of a frame's time its pixels may account for, for giving
    /// some up to be worth it.
    const WORTH: f32 = 0.3;
    /// The fewest pixels kept, as a share.
    const FLOOR: f64 = 0.25;
    /// Frames passed over after a step.
    const SETTLE: u32 = 6;

    pub(super) fn factor(&self) -> f64 {
        self.factor
    }

    /// One delivered frame took `frame_ms` from submission to completion, at
    /// a level that holds `rate` frames a second.
    pub(super) fn observe(&mut self, frame_ms: f32, rate: f32) {
        if !frame_ms.is_finite() || rate <= 0.0 {
            return;
        }
        if self.settling > 0 {
            self.settling -= 1;
            return;
        }
        let allowed = 1_000.0 / rate;
        if frame_ms > allowed * Self::LATE {
            self.late += 1;
            self.late_ms += frame_ms;
            self.early = 0;
        } else {
            (self.late, self.late_ms, self.stepped) = (0, 0.0, None);
            self.early = if frame_ms < allowed * Self::EARLY {
                self.early + 1
            } else {
                0
            };
        }
        // Counted in frames at the level's own rate: late frames come
        // slower than that, so this is a second at the least.
        if self.late as f32 >= rate {
            let mean = self.late_ms / self.late as f32;
            (self.late, self.late_ms) = (0, 0.0);
            self.late_run(mean, allowed);
        } else if self.early as f32 >= rate * 2.0 {
            self.early = 0;
            self.resize((self.factor * 1.1).min(1.0));
        }
    }

    /// A run of late frames, `mean` milliseconds each.
    fn late_run(&mut self, mean: f32, allowed: f32) {
        let same =
            |(share, time): (f64, f32)| share == self.factor && (mean / time - 1.0).abs() < 0.2;
        if self.futile.is_some_and(same) {
            return;
        }
        self.futile = None;
        if let Some((share, before)) = self.stepped.take() {
            // What the step bought, against what it would have were the
            // whole frame's time its pixels'.
            let bought = (before - mean) / before;
            let gave = (1.0 - self.factor / share) as f32;
            if bought < Self::WORTH * gave {
                self.futile = Some((share, before));
                self.resize(share);
                return;
            }
        }
        // A frame's time goes with its pixels at most, so this many fewer
        // bring the mean late frame to the aim or short of it.
        let keep = (allowed * Self::AIM / mean).clamp(0.5, 0.8);
        let from = self.factor;
        self.resize((from * f64::from(keep)).max(Self::FLOOR));
        if self.factor != from {
            self.stepped = Some((from, mean));
        }
    }

    fn resize(&mut self, factor: f64) {
        if factor != self.factor {
            self.factor = factor;
            self.settling = Self::SETTLE;
        }
    }
}

/// How the stage's render size follows its element's physical size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RenderScale {
    /// Native up to [`RENDER_BUDGET_PIXELS`], scaled down past it.
    Budget,
    /// Always this fraction of the element — `LUMA_RENDER_SCALE`, 0.25..=1.
    Fixed(f32),
    /// Always the element's own size — `LUMA_RENDER_BUDGET=off`.
    Native,
}

impl RenderScale {
    /// Read once per process; the choice is logged.
    pub(super) fn from_env() -> Self {
        static SCALE: std::sync::OnceLock<RenderScale> = std::sync::OnceLock::new();
        *SCALE.get_or_init(|| {
            let scale = Self::parse(
                std::env::var("LUMA_RENDER_SCALE").ok().as_deref(),
                std::env::var("LUMA_RENDER_BUDGET").ok().as_deref(),
            );
            eprintln!("[stage] render size: {scale:?}");
            scale
        })
    }

    /// `LUMA_RENDER_SCALE` wins over `LUMA_RENDER_BUDGET`. A value that does
    /// not parse is reported and ignored.
    fn parse(scale: Option<&str>, budget: Option<&str>) -> Self {
        if let Some(scale) = scale {
            match scale.trim().parse::<f32>() {
                Ok(value) if value.is_finite() => return Self::Fixed(value.clamp(0.25, 1.0)),
                _ => eprintln!("[stage] ignoring LUMA_RENDER_SCALE={scale:?}; expected 0.25..1"),
            }
        }
        match budget {
            Some("off") => Self::Native,
            Some(other) => {
                eprintln!("[stage] ignoring LUMA_RENDER_BUDGET={other:?}; expected off");
                Self::Budget
            }
            None => Self::Budget,
        }
    }

    /// The size to render an element of `physical` device pixels at: the
    /// same aspect, whole pixels, never larger than the element.
    ///
    /// `percent` is the operator's own `render_scale` setting, and it
    /// **composes** with whatever this variant already decided — it does not
    /// replace it. Two things follow, and both are the point:
    ///
    /// * At 100 the multiplier is exactly 1.0, so the answer is bit-for-bit the
    ///   one this function gave before the setting existed, [`Self::Budget`]'s
    ///   fullscreen protection included.
    /// * Every notch below 100 removes pixels *from whatever is being rendered
    ///   now*. Had the percent replaced the budget, then on a large retina
    ///   window — where the budget already picks ~0.6 — the top of the slider's
    ///   travel would do nothing at all, which reads as a broken control.
    ///
    /// `limit` is a quality level's own pixel budget, when it has one, and it
    /// caps every variant: a reduced level on a laptop must not render a native
    /// 4K window because `LUMA_RENDER_BUDGET=off` was set for a desktop.
    ///
    /// `share` is the part of those pixels the level's frame rate leaves it
    /// ([`DynamicBudget::factor`]), whatever the size of the element: a small
    /// window whose frames are late gives up pixels too. A fixed scale stays
    /// fixed.
    pub(super) fn size(
        self,
        percent: u8,
        limit: Option<f64>,
        share: f64,
        (width, height): (u32, u32),
    ) -> (u32, u32) {
        let area = f64::from(width) * f64::from(height);
        let env = match self {
            Self::Native => 1.0,
            Self::Fixed(scale) => f64::from(scale),
            Self::Budget => (RENDER_BUDGET_PIXELS / area).sqrt().min(1.0),
        };
        let env = limit.map_or(env, |pixels| env.min((pixels / area).sqrt()));
        let env = match self {
            Self::Fixed(_) => env,
            Self::Native | Self::Budget => env * share.sqrt(),
        };
        let scale = env * f64::from(clamp_render_scale(i64::from(percent))) / 100.0;
        let fit = |side: u32| ((f64::from(side) * scale).round() as u32).clamp(1, side.max(1));
        (fit(width), fit(height))
    }
}

#[cfg(test)]
mod render_scale_tests {
    use super::{DynamicBudget, RenderScale};

    /// A reduced level's pixel budget caps every variant, native included, and
    /// composes with the operator's percent.
    #[test]
    fn a_quality_limit_caps_every_variant() {
        let limit = Some(1.0e6);
        let (w, h) = RenderScale::Native.size(FULL, limit, 1.0, (1920, 1080));
        assert!(f64::from(w) * f64::from(h) <= 1.0e6 * 1.01, "{w}x{h}");
        assert_eq!(
            RenderScale::Native.size(FULL, limit, 1.0, (800, 600)),
            (800, 600)
        );
        let (half_w, _) = RenderScale::Native.size(50, limit, 1.0, (1920, 1080));
        assert_eq!(half_w, (f64::from(w) / 2.0).round() as u32);
    }

    /// A stage whose frame takes `fixed` milliseconds and `full` more at
    /// every pixel, run for `frames` frames at a level that holds `rate`:
    /// each frame's time, and how many times the size changed.
    fn run(
        budget: &mut DynamicBudget,
        (fixed, full): (f32, f32),
        rate: f32,
        frames: u32,
    ) -> (Vec<f32>, u32) {
        let (mut times, mut steps) = (Vec::new(), 0);
        for _ in 0..frames {
            let before = budget.factor();
            let time = fixed + full * before as f32;
            budget.observe(time, rate);
            steps += u32::from(budget.factor() != before);
            times.push(time);
        }
        (times, steps)
    }

    /// A level whose frames take longer than its frame rate allows gives up
    /// pixels until they fit, then stays where it is.
    #[test]
    fn a_level_that_misses_its_frame_rate_gives_up_pixels_until_it_holds_it() {
        for (rate, load) in [
            (30.0, (6.0, 32.0)),
            (60.0, (3.0, 40.0)),
            (120.0, (2.5, 9.0)),
        ] {
            let allowed = 1_000.0 / rate;
            let mut budget = DynamicBudget::default();
            assert!(load.0 + load.1 > allowed, "the load must miss the rate");
            let (times, _) = run(&mut budget, load, rate, 2_000);
            assert!(budget.factor() < 1.0);
            assert!(
                *times.last().unwrap() < allowed,
                "{rate} fps: still {} ms",
                times.last().unwrap()
            );
            // Settled: an hour more of the same frames moves nothing.
            let settled = budget.factor();
            let (_, steps) = run(&mut budget, load, rate, 200_000);
            assert_eq!((steps, budget.factor()), (0, settled), "{rate} fps hunts");
        }
    }

    /// Frames that fit are left at every pixel, and one late frame among
    /// them takes nothing away.
    #[test]
    fn a_level_that_holds_its_frame_rate_keeps_every_pixel() {
        let mut budget = DynamicBudget::default();
        let (_, steps) = run(&mut budget, (2.0, 3.0), 120.0, 10_000);
        assert_eq!((steps, budget.factor()), (0, 1.0));
        for _ in 0..50 {
            budget.observe(400.0, 120.0);
            run(&mut budget, (2.0, 3.0), 120.0, 500);
        }
        assert_eq!(budget.factor(), 1.0);
    }

    /// When the load lifts the pixels come back, a step at a time and never
    /// past the whole, and nothing takes the last quarter.
    #[test]
    fn pixels_come_back_when_the_load_lifts_and_a_quarter_always_stays() {
        let mut budget = DynamicBudget::default();
        run(&mut budget, (10.0, 4_000.0), 30.0, 5_000);
        let least = budget.factor();
        assert!((0.25..0.3).contains(&least), "{least}");
        let mut last = least;
        for _ in 0..40 {
            run(&mut budget, (4.0, 8.0), 30.0, 200);
            assert!(budget.factor() >= last && budget.factor() <= 1.0);
            last = budget.factor();
        }
        assert_eq!(last, 1.0);
    }

    /// Frames that are late for something other than their pixels keep
    /// them: the one step that bought nothing is taken back, and frames
    /// that slow are left alone after it.
    #[test]
    fn frames_that_do_not_wait_on_their_pixels_keep_them() {
        let mut budget = DynamicBudget::default();
        let (_, steps) = run(&mut budget, (40.0, 4.0), 30.0, 2_000);
        assert_eq!(budget.factor(), 1.0);
        assert!(steps <= 2, "{steps} changes of size");
        let (_, steps) = run(&mut budget, (40.0, 4.0), 30.0, 200_000);
        assert_eq!((steps, budget.factor()), (0, 1.0));
        // A heavier load that is its pixels' is another matter.
        run(&mut budget, (40.0, 400.0), 30.0, 2_000);
        assert!(budget.factor() < 1.0);
    }

    /// The level's share takes pixels from a window of any size, at its own
    /// aspect; a fixed scale stays fixed.
    #[test]
    fn a_share_of_the_pixels_applies_to_a_small_window_too() {
        for scale in [RenderScale::Budget, RenderScale::Native] {
            let whole = scale.size(FULL, Some(1.0e6), 1.0, (800, 600));
            let half = scale.size(FULL, Some(1.0e6), 0.5, (800, 600));
            let pixels = |(w, h): (u32, u32)| f64::from(w) * f64::from(h);
            assert_eq!(whole, (800, 600));
            assert!(
                (pixels(half) / pixels(whole) - 0.5).abs() < 0.01,
                "{half:?}"
            );
            assert!((f64::from(half.0) / f64::from(half.1) - 800.0 / 600.0).abs() < 0.01);
        }
        assert_eq!(
            RenderScale::Fixed(0.5).size(FULL, None, 0.5, (800, 600)),
            RenderScale::Fixed(0.5).size(FULL, None, 1.0, (800, 600))
        );
    }

    /// The percent the operator has not touched. Every assertion about the
    /// behaviour that predates the setting is taken at this value.
    const FULL: u8 = 100;

    #[test]
    fn an_ordinary_window_renders_native() {
        assert_eq!(
            RenderScale::Budget.size(FULL, None, 1.0, (1984, 1511)),
            (1984, 1511)
        );
        assert_eq!(
            RenderScale::Budget.size(FULL, None, 1.0, (2227, 1391)),
            (2227, 1391)
        );
        assert_eq!(RenderScale::Budget.size(FULL, None, 1.0, (1, 1)), (1, 1));
    }

    #[test]
    fn fullscreen_scales_to_the_budget_at_its_own_aspect() {
        let (width, height) = RenderScale::Budget.size(FULL, None, 1.0, (3600, 2260));
        let scale = f64::from(width) / 3600.0;
        assert!((scale - 0.617).abs() < 0.002, "scale {scale}");
        assert!(
            (f64::from(width) / f64::from(height) - 3600.0 / 2260.0).abs() < 0.002,
            "{width}x{height}"
        );
        let budget = super::RENDER_BUDGET_PIXELS;
        let pixels = f64::from(width) * f64::from(height);
        assert!(
            (pixels / budget - 1.0).abs() < 0.002,
            "{pixels} vs {budget}"
        );
    }

    #[test]
    fn overrides_parse_and_clamp() {
        assert_eq!(RenderScale::parse(None, None), RenderScale::Budget);
        assert_eq!(RenderScale::parse(None, Some("off")), RenderScale::Native);
        assert_eq!(
            RenderScale::parse(Some("0.5"), None),
            RenderScale::Fixed(0.5)
        );
        assert_eq!(
            RenderScale::parse(Some("0.1"), None),
            RenderScale::Fixed(0.25)
        );
        assert_eq!(
            RenderScale::parse(Some("3"), Some("off")),
            RenderScale::Fixed(1.0)
        );
        assert_eq!(
            RenderScale::parse(Some("soft"), Some("off")),
            RenderScale::Native
        );
        assert_eq!(RenderScale::parse(None, Some("on")), RenderScale::Budget);
        assert_eq!(
            RenderScale::Native.size(FULL, None, 1.0, (3600, 2260)),
            (3600, 2260)
        );
        assert_eq!(
            RenderScale::Fixed(0.5).size(FULL, None, 1.0, (3600, 2260)),
            (1800, 1130)
        );
        assert_eq!(
            RenderScale::Fixed(0.25).size(FULL, None, 1.0, (2, 2)),
            (1, 1)
        );
    }

    /// The operator's percent **composes** with the env-var scale; it does not
    /// replace it. Had it replaced the budget, the top of the slider's travel
    /// would be a no-op on exactly the window where the control matters most.
    #[test]
    fn the_percent_composes_with_the_budget_rather_than_replacing_it() {
        let fullscreen = (3600, 2260);
        let budgeted = RenderScale::Budget.size(FULL, None, 1.0, fullscreen);
        // Replacement would render 50% of 3600 = 1800 wide. Composition renders
        // half of what the budget already picked.
        let halved = RenderScale::Budget.size(50, None, 1.0, fullscreen);
        // Half of what the budget picked, to within the rounding each side is
        // fitted to — not half of the element.
        assert!(
            halved.0.abs_diff(budgeted.0 / 2) <= 1 && halved.1.abs_diff(budgeted.1 / 2) <= 1,
            "{halved:?} is not half of {budgeted:?}"
        );
        assert_ne!(halved.0, fullscreen.0 / 2);

        // And the protection survives underneath: the full-percent answer is
        // still capped at the budget, so the percent cannot be used to ask for
        // the 27 fps fullscreen frame back.
        let pixels = f64::from(budgeted.0) * f64::from(budgeted.1);
        assert!(pixels <= super::RENDER_BUDGET_PIXELS * 1.002, "{pixels}");

        // Every notch below 100 removes pixels, which is the whole point of the
        // control — a monotone walk, not one flat stretch at the top.
        let mut previous = budgeted;
        for percent in [99, 90, 75, 60, 45, 30, 25] {
            let size = RenderScale::Budget.size(percent, None, 1.0, fullscreen);
            assert!(size.0 < previous.0, "{percent}% gave {size:?}");
            previous = size;
        }
    }

    /// 100 is bit-for-bit the answer this function gave before the setting
    /// existed: the multiplier is exactly 1.0, so no rounding moves.
    #[test]
    fn a_hundred_percent_is_identical_to_the_old_behaviour() {
        for scale in [
            RenderScale::Budget,
            RenderScale::Native,
            RenderScale::Fixed(0.617),
        ] {
            for size in [(1984, 1511), (2227, 1391), (3600, 2260), (1, 1), (7, 3)] {
                let old = {
                    let env = match scale {
                        RenderScale::Native => 1.0,
                        RenderScale::Fixed(fixed) => f64::from(fixed),
                        RenderScale::Budget => (super::RENDER_BUDGET_PIXELS
                            / (f64::from(size.0) * f64::from(size.1)))
                        .sqrt()
                        .min(1.0),
                    };
                    let fit =
                        |side: u32| ((f64::from(side) * env).round() as u32).clamp(1, side.max(1));
                    (fit(size.0), fit(size.1))
                };
                assert_eq!(
                    scale.size(FULL, None, 1.0, size),
                    old,
                    "{scale:?} at {size:?}"
                );
            }
        }
    }

    /// A stored percent from a hand-edited database or an older build cannot put
    /// the renderer somewhere it has no answer for.
    #[test]
    fn an_out_of_range_percent_is_held_to_the_slider_travel() {
        assert_eq!(
            RenderScale::Native.size(0, None, 1.0, (1000, 1000)),
            RenderScale::Native.size(25, None, 1.0, (1000, 1000))
        );
        assert_eq!(
            RenderScale::Native.size(255, None, 1.0, (1000, 1000)),
            RenderScale::Native.size(100, None, 1.0, (1000, 1000))
        );
    }
}
