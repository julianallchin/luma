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

/// The fewest pixels a level's frame rate leaves a stage that has more: what
/// the reduced levels render (`QualityBudget::pixel_budget`). Under it beams
/// and edges are visibly stepped once the compositor has scaled them up, so
/// a level already there, or a small window, has none to give.
const LEAST_PIXELS: f64 = 1.0e6;

/// A quality level's dynamic resolution: the share of its pixels the stage
/// renders, driven by how long its frames take against the frame rate the
/// level holds.
///
/// A frame's time is from the start of its build on the UI thread to its
/// completion, since one frame is in flight at a time and a frame that is not
/// home by the refresh it was meant for is shown a whole refresh late. Of
/// that time only the GPU's own scales with the pixels; the rest (the score,
/// the build, the encode, the wait for the GPU) is the same at any size.
///
/// So the stage keeps clear of the time a frame is allowed: a second of
/// frames that use more than [`Self::LATE`] of it take away the pixels that
/// bring the frame to [`Self::AIM`] of it, by the GPU's time, and two
/// seconds of frames that would still be within the aim with a tenth more
/// pixels get that tenth back. A second, so that a venue loading or a window
/// coming forward costs nothing. Between the two nothing moves, because a
/// resize discards the haze caches and a size that hunts would never settle.
/// A quarter of the pixels is the floor.
///
/// Pixels are only given up while they are what the frame waits on. A late
/// frame whose GPU time is under [`Self::WORTH`] of it keeps them all, since
/// no size would bring it home. And the GPU's time is not all its pixels': a
/// rig of hundreds of lights costs much the same at any size. So a step that
/// takes less than [`Self::WORTH`] of the GPU's time for the pixels it gave
/// up is taken back, and frames that slow at that size are left alone after
/// it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DynamicBudget {
    factor: f64,
    /// Late frames in a row, with their time and their GPU time together.
    late: u32,
    late_ms: f32,
    late_gpu_ms: f32,
    /// Frames since the stage last looked at giving pixels back, and how many
    /// of them had room for more.
    seen: u32,
    roomy: u32,
    /// Frames still to pass over after a step: those already in flight at
    /// the old size, and the first at the new one, which rebuild caches.
    settling: u32,
    /// The share and the late frames' mean GPU time the last step down
    /// started from, while nothing but late frames has followed it.
    stepped: Option<(f64, f32)>,
    /// The share and mean GPU time at which fewer pixels bought nothing.
    futile: Option<(f64, f32)>,
}

impl Default for DynamicBudget {
    fn default() -> Self {
        Self {
            factor: 1.0,
            late: 0,
            late_ms: 0.0,
            late_gpu_ms: 0.0,
            seen: 0,
            roomy: 0,
            settling: 0,
            stepped: None,
            futile: None,
        }
    }
}

impl DynamicBudget {
    /// The share of a frame's allowed time past which it counts as late, and
    /// that a step either way aims for.
    const LATE: f32 = 0.9;
    const AIM: f32 = 0.8;
    /// The least of a late frame's time the GPU may account for, and the
    /// least of the GPU's time its pixels may, for giving some up to be worth
    /// it.
    const WORTH: f32 = 0.3;
    /// The fewest pixels kept, as a share.
    const FLOOR: f64 = 0.25;
    /// What a step up gives back, and the share of two seconds' frames that
    /// must have room for it: a stray slow frame does not hold the rest down.
    const BACK: f32 = 0.1;
    const MOST: f32 = 0.9;
    /// Frames passed over after a step.
    const SETTLE: u32 = 6;

    pub(super) fn factor(&self) -> f64 {
        self.factor
    }

    /// One delivered frame took `frame_ms` from the start of its build to
    /// its completion, `gpu_ms` of it on the GPU where that is known, at a
    /// level that holds `rate` frames a second.
    pub(super) fn observe(&mut self, frame_ms: f32, gpu_ms: Option<f32>, rate: f32) {
        if !frame_ms.is_finite() || rate <= 0.0 {
            return;
        }
        if self.settling > 0 {
            self.settling -= 1;
            return;
        }
        let allowed = 1_000.0 / rate;
        // The part of the frame its pixels account for: all of it, for
        // giving pixels back, until the GPU's time is known.
        let pixels_ms = gpu_ms.map_or(frame_ms, |gpu| gpu.clamp(0.0, frame_ms));
        if frame_ms > allowed * Self::LATE {
            self.late += 1;
            self.late_ms += frame_ms;
            self.late_gpu_ms += gpu_ms.map_or(0.0, |gpu| gpu.clamp(0.0, frame_ms));
        } else {
            (self.late, self.late_ms, self.late_gpu_ms) = (0, 0.0, 0.0);
            self.stepped = None;
        }
        self.seen += 1;
        self.roomy += u32::from(frame_ms + pixels_ms * Self::BACK <= allowed * Self::AIM);
        // Counted in frames at the level's own rate: late frames come
        // slower than that, so this is a second at the least.
        if self.late as f32 >= rate {
            let (mean, gpu) = (
                self.late_ms / self.late as f32,
                self.late_gpu_ms / self.late as f32,
            );
            (self.late, self.late_ms, self.late_gpu_ms) = (0, 0.0, 0.0);
            self.late_run(mean, gpu, allowed);
        }
        if self.seen as f32 >= rate * 2.0 {
            let most = self.roomy as f32 >= self.seen as f32 * Self::MOST;
            (self.seen, self.roomy) = (0, 0);
            if most {
                self.resize((self.factor * f64::from(1.0 + Self::BACK)).min(1.0));
            }
        }
    }

    /// A run of late frames, `mean` milliseconds each, `gpu` of them on the
    /// GPU.
    fn late_run(&mut self, mean: f32, gpu: f32, allowed: f32) {
        let same =
            |(share, time): (f64, f32)| share == self.factor && (gpu / time - 1.0).abs() < 0.2;
        if gpu < mean * Self::WORTH || self.futile.is_some_and(same) {
            return;
        }
        self.futile = None;
        if let Some((share, before)) = self.stepped.take() {
            // What the step took off the GPU's time, against what it would
            // have were all of that time its pixels'.
            let bought = (before - gpu) / before;
            let gave = (1.0 - self.factor / share) as f32;
            if bought < Self::WORTH * gave {
                self.futile = Some((share, before));
                self.resize(share);
                return;
            }
        }
        // The GPU's time goes with the pixels and the rest stays, so this
        // share of them brings the mean late frame to the aim. No more than
        // half go at once: the estimate is of the size it was measured at.
        let keep = ((allowed * Self::AIM - (mean - gpu)) / gpu).clamp(0.5, 0.9);
        let from = self.factor;
        self.resize((from * f64::from(keep)).max(Self::FLOOR));
        if self.factor != from {
            self.stepped = Some((from, gpu));
        }
    }

    fn resize(&mut self, factor: f64) {
        if factor != self.factor {
            self.factor = factor;
            self.settling = Self::SETTLE;
            (self.seen, self.roomy) = (0, 0);
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
    /// ([`DynamicBudget::factor`]), down to [`LEAST_PIXELS`]: a stage with no
    /// more than that keeps them all. A fixed scale stays fixed.
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
            Self::Native | Self::Budget => {
                (env * share.sqrt()).max(env.min((LEAST_PIXELS / area).sqrt()))
            }
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
            let gpu = full * before as f32;
            let time = fixed + gpu;
            budget.observe(time, Some(gpu), rate);
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
            budget.observe(400.0, Some(3.0), 120.0);
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
    /// every one of them, however long that goes on.
    #[test]
    fn frames_that_do_not_wait_on_their_pixels_keep_them() {
        let mut budget = DynamicBudget::default();
        let (_, steps) = run(&mut budget, (40.0, 4.0), 30.0, 200_000);
        assert_eq!((steps, budget.factor()), (0, 1.0));
        // Low at 120: a frame the UI thread and the encode hold up.
        let (_, steps) = run(&mut budget, (7.5, 1.7), 120.0, 200_000);
        assert_eq!((steps, budget.factor()), (0, 1.0));
        // A heavier load that is its pixels' is another matter.
        run(&mut budget, (40.0, 400.0), 30.0, 2_000);
        assert!(budget.factor() < 1.0);
    }

    /// A rig whose GPU time is its lights', not its pixels', keeps its
    /// pixels: the one step that bought nothing is taken back and not tried
    /// again.
    #[test]
    fn gpu_time_that_does_not_fall_with_the_pixels_keeps_them() {
        let mut budget = DynamicBudget::default();
        let mut steps = 0;
        for frame in 0..200_000 {
            let before = budget.factor();
            // 5.5 ms of the GPU's time is the same at any size.
            let gpu = 5.5 + 0.5 * before as f32;
            budget.observe(5.0 + gpu, Some(gpu), 120.0);
            steps += u32::from(budget.factor() != before);
            if frame == 2_000 {
                assert!(steps <= 2, "{steps} changes of size");
            }
        }
        assert_eq!((steps <= 2, budget.factor()), (true, 1.0));
    }

    /// Pixels a rough patch took come back once it is over, though a frame
    /// in twenty still runs long.
    #[test]
    fn pixels_come_back_through_the_odd_slow_frame() {
        let mut budget = DynamicBudget::default();
        run(&mut budget, (2.0, 30.0), 120.0, 2_000);
        let cut = budget.factor();
        assert!(cut < 1.0);
        for frame in 0..20_000 {
            let gpu = 2.0 * budget.factor() as f32;
            let stray = if frame % 20 == 0 { 6.0 } else { 0.0 };
            budget.observe(3.0 + gpu + stray, Some(gpu), 120.0);
        }
        assert_eq!(budget.factor(), 1.0, "from {cut}");
    }

    /// Before the GPU's time is known nothing is taken away.
    #[test]
    fn nothing_is_given_up_before_the_gpu_time_is_known() {
        let mut budget = DynamicBudget::default();
        for _ in 0..5_000 {
            budget.observe(50.0, None, 60.0);
        }
        assert_eq!(budget.factor(), 1.0);
    }

    /// The level's share takes pixels from a stage at its own aspect, down
    /// to the fewest worth showing; a stage with no more than that keeps
    /// them, and a fixed scale stays fixed.
    #[test]
    fn a_share_takes_pixels_down_to_the_fewest_worth_showing() {
        let pixels = |(w, h): (u32, u32)| f64::from(w) * f64::from(h);
        for scale in [RenderScale::Budget, RenderScale::Native] {
            let whole = scale.size(FULL, Some(3.0e6), 1.0, (3000, 2000));
            let half = scale.size(FULL, Some(3.0e6), 0.5, (3000, 2000));
            assert!(
                (pixels(half) / pixels(whole) - 0.5).abs() < 0.01,
                "{half:?}"
            );
            assert!((f64::from(half.0) / f64::from(half.1) - 1.5).abs() < 0.01);
            // A quarter would be fewer than the reduced levels render.
            let least = scale.size(FULL, Some(3.0e6), 0.25, (3000, 2000));
            assert!((pixels(least) / 1.0e6 - 1.0).abs() < 0.01, "{least:?}");
            // A level at its own million, and a small window, have none to give.
            assert_eq!(
                scale.size(FULL, Some(1.0e6), 0.5, (3000, 2000)),
                scale.size(FULL, Some(1.0e6), 1.0, (3000, 2000))
            );
            assert_eq!(scale.size(FULL, Some(1.0e6), 0.5, (800, 600)), (800, 600));
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
