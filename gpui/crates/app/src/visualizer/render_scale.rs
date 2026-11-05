//! How many pixels the stage renders: the fixed pixel budget, the
//! environment overrides, the operator's percent and low quality's dynamic share.
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

/// GPU frame time low quality aims under: a 60 Hz frame with room for the UI.
const DYNAMIC_TARGET_GPU_MS: f32 = 14.0;

/// Low quality's dynamic resolution: the share of its pixel budget the stage
/// renders, driven by measured GPU frame time.
///
/// It steps down fast (three slow samples in a row take a fifth of the pixels
/// away) and back up slowly (thirty quick ones give a tenth back), because a
/// resize discards the haze caches and a size that hunts would never settle.
/// A quarter of the budget is the floor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DynamicBudget {
    factor: f64,
    slow: u32,
    quick: u32,
}

impl Default for DynamicBudget {
    fn default() -> Self {
        Self {
            factor: 1.0,
            slow: 0,
            quick: 0,
        }
    }
}

impl DynamicBudget {
    pub(super) fn factor(&self) -> f64 {
        self.factor
    }

    pub(super) fn observe(&mut self, gpu_ms: f32) {
        if !gpu_ms.is_finite() {
            return;
        }
        if gpu_ms > DYNAMIC_TARGET_GPU_MS * 1.15 {
            self.slow += 1;
            self.quick = 0;
        } else if gpu_ms < DYNAMIC_TARGET_GPU_MS * 0.7 {
            self.quick += 1;
            self.slow = 0;
        } else {
            self.slow = 0;
            self.quick = 0;
        }
        if self.slow >= 3 {
            self.factor = (self.factor * 0.8).max(0.25);
            self.slow = 0;
        }
        if self.quick >= 30 {
            self.factor = (self.factor * 1.1).min(1.0);
            self.quick = 0;
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
    /// caps every variant: low quality on a laptop must not render a native
    /// 4K window because `LUMA_RENDER_BUDGET=off` was set for a desktop.
    pub(super) fn size(
        self,
        percent: u8,
        limit: Option<f64>,
        (width, height): (u32, u32),
    ) -> (u32, u32) {
        let area = f64::from(width) * f64::from(height);
        let env = match self {
            Self::Native => 1.0,
            Self::Fixed(scale) => f64::from(scale),
            Self::Budget => (RENDER_BUDGET_PIXELS / area).sqrt().min(1.0),
        };
        let env = limit.map_or(env, |pixels| env.min((pixels / area).sqrt()));
        let scale = env * f64::from(clamp_render_scale(i64::from(percent))) / 100.0;
        let fit = |side: u32| ((f64::from(side) * scale).round() as u32).clamp(1, side.max(1));
        (fit(width), fit(height))
    }
}

#[cfg(test)]
mod render_scale_tests {
    use super::{DynamicBudget, RenderScale};

    /// Low quality's pixel budget caps every variant, native included, and
    /// composes with the operator's percent.
    #[test]
    fn a_quality_limit_caps_every_variant() {
        let limit = Some(1.0e6);
        let (w, h) = RenderScale::Native.size(FULL, limit, (1920, 1080));
        assert!(f64::from(w) * f64::from(h) <= 1.0e6 * 1.01, "{w}x{h}");
        assert_eq!(
            RenderScale::Native.size(FULL, limit, (800, 600)),
            (800, 600)
        );
        let (half_w, _) = RenderScale::Native.size(50, limit, (1920, 1080));
        assert_eq!(half_w, (f64::from(w) / 2.0).round() as u32);
    }

    #[test]
    fn dynamic_budget_steps_down_fast_and_up_slowly() {
        let mut budget = DynamicBudget::default();
        for _ in 0..3 {
            budget.observe(30.0);
        }
        assert!((budget.factor() - 0.8).abs() < 1e-9);
        // A single quick frame, or an in-band one, changes nothing.
        budget.observe(5.0);
        budget.observe(14.0);
        assert!((budget.factor() - 0.8).abs() < 1e-9);
        for _ in 0..30 {
            budget.observe(5.0);
        }
        assert!((budget.factor() - 0.88).abs() < 1e-9);
        for _ in 0..300 {
            budget.observe(100.0);
        }
        assert_eq!(budget.factor(), 0.25);
    }

    /// The percent the operator has not touched. Every assertion about the
    /// behaviour that predates the setting is taken at this value.
    const FULL: u8 = 100;

    #[test]
    fn an_ordinary_window_renders_native() {
        assert_eq!(
            RenderScale::Budget.size(FULL, None, (1984, 1511)),
            (1984, 1511)
        );
        assert_eq!(
            RenderScale::Budget.size(FULL, None, (2227, 1391)),
            (2227, 1391)
        );
        assert_eq!(RenderScale::Budget.size(FULL, None, (1, 1)), (1, 1));
    }

    #[test]
    fn fullscreen_scales_to_the_budget_at_its_own_aspect() {
        let (width, height) = RenderScale::Budget.size(FULL, None, (3600, 2260));
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
            RenderScale::Native.size(FULL, None, (3600, 2260)),
            (3600, 2260)
        );
        assert_eq!(
            RenderScale::Fixed(0.5).size(FULL, None, (3600, 2260)),
            (1800, 1130)
        );
        assert_eq!(RenderScale::Fixed(0.25).size(FULL, None, (2, 2)), (1, 1));
    }

    /// The operator's percent **composes** with the env-var scale; it does not
    /// replace it. Had it replaced the budget, the top of the slider's travel
    /// would be a no-op on exactly the window where the control matters most.
    #[test]
    fn the_percent_composes_with_the_budget_rather_than_replacing_it() {
        let fullscreen = (3600, 2260);
        let budgeted = RenderScale::Budget.size(FULL, None, fullscreen);
        // Replacement would render 50% of 3600 = 1800 wide. Composition renders
        // half of what the budget already picked.
        let halved = RenderScale::Budget.size(50, None, fullscreen);
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
            let size = RenderScale::Budget.size(percent, None, fullscreen);
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
                assert_eq!(scale.size(FULL, None, size), old, "{scale:?} at {size:?}");
            }
        }
    }

    /// A stored percent from a hand-edited database or an older build cannot put
    /// the renderer somewhere it has no answer for.
    #[test]
    fn an_out_of_range_percent_is_held_to_the_slider_travel() {
        assert_eq!(
            RenderScale::Native.size(0, None, (1000, 1000)),
            RenderScale::Native.size(25, None, (1000, 1000))
        );
        assert_eq!(
            RenderScale::Native.size(255, None, (1000, 1000)),
            RenderScale::Native.size(100, None, (1000, 1000))
        );
    }
}
