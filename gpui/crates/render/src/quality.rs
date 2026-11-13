//! What a [`Quality`] level buys: the one table every pass reads. Nothing
//! else in the renderer matches on a level; it reads its share of this.

use crate::atmosphere::clouds;
use crate::scene_desc::Quality;

/// The per-frame work a [`Quality`] level buys.
///
/// `High` is the reference picture: its shadow walk is the one the renderer
/// had before the levels existed. Its plain-beam piece cap and shadow redraw
/// budget are new, and measured to leave the picture unchanged. `Medium` is
/// a laptop GPU's budget. `Low` is `Medium` without the march, without
/// fixture shadows and with ambient visibility at half resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct QualityBudget {
    /// Whether the haze is marched. Without it no volumetric pass runs —
    /// not the fog grid, the medium cache, the lit-interval cache, the
    /// compaction, the haze kernels, the sun shafts or the haze glow — and
    /// `haze_analytic.wgsl` writes the haze target in closed form, through
    /// uniform haze: the medium has no clouds.
    pub marched: bool,
    /// Whether fixtures cast shadows. Without them no fixture shadow map is
    /// assigned, drawn or reduced, and surfaces take each fixture's light
    /// unshadowed. `Frame::fixture_shadows` can only turn them off.
    pub fixture_shadows: bool,
    /// Gauss-Legendre piece cap for a plain beam (`quadrature_pieces`).
    pub plain_pieces: f32,
    /// Scale on the haze shadow walk's pixel-footprint allowance.
    pub shadow_footprint: f32,
    /// Scale on the haze shadow walk's soft and hard step counts.
    pub shadow_steps: f32,
    /// How much coarser the shadow walk gets while the camera moves, when
    /// nothing settles anyway ([`Self::while_moving`]).
    pub moving_coarsening: f32,
    /// Most moved fixture shadow maps redrawn in one frame.
    pub shadow_redraws: usize,
    /// Far-field fog grid cell size in pixels.
    pub fog_tile: u32,
    /// Scene-pass samples per pixel.
    pub msaa: u32,
    /// Output pixels per ambient-visibility texel along each axis.
    pub ambient_divisor: u32,
    /// What the bounced light (`bounce.rs`) may spend; without it surfaces
    /// take the house's flat fill and the sky probe alone.
    pub bounce: Option<crate::bounce::stage::Budget>,
    /// Sun shafts: output pixels per texel along each axis, and samples per
    /// ray.
    pub sun_shafts: (u32, u32),
    /// The haze glow grid's cells along each axis.
    pub haze_glow: [u32; 3],
    /// The cloud layer's trace.
    pub clouds: clouds::Budget,
    /// Which of the glare FFT pipelines (`post.rs`) runs: 0 full size, 1
    /// half.
    pub glare_fft: usize,
    /// Whether floors load their maps at full size and draw parallax.
    pub floor_detail: bool,
    /// Most of the output resolution the haze target may take, if the level
    /// limits the viewer's own setting.
    pub haze_resolution_cap: Option<f32>,
    /// Most stochastic samples per gobo beam, if the level limits the
    /// viewer's own setting.
    pub haze_steps_cap: Option<u32>,
    /// Most pixels a live viewport renders before the compositor upscales,
    /// when the level sets its own limit.
    pub pixel_budget: Option<f64>,
    /// Frames a second a live viewport holds at this level: it renders
    /// fewer pixels when a frame takes longer than that allows.
    pub frame_rate: f32,
}

impl QualityBudget {
    #[must_use]
    pub(crate) fn of(quality: Quality) -> Self {
        match quality {
            Quality::High => Self {
                marched: true,
                fixture_shadows: true,
                plain_pieces: 8.0,
                shadow_footprint: 1.0,
                shadow_steps: 1.0,
                moving_coarsening: 1.0,
                shadow_redraws: 64,
                fog_tile: crate::fog_grid::TILE_SIZE,
                msaa: crate::gpu::MSAA_SAMPLES,
                ambient_divisor: 1,
                bounce: Some(crate::bounce::stage::Budget {
                    spacing: 0.5,
                    discs: 12_000.0,
                    error: 0.02,
                    step: 4,
                }),
                sun_shafts: (2, 24),
                haze_glow: crate::haze_glow::HIGH,
                clouds: clouds::Budget {
                    divisor: 2,
                    steps: 64,
                    light_steps: 6,
                    panorama: (512, 256),
                    shadow: 1024,
                },
                glare_fft: 0,
                floor_detail: true,
                haze_resolution_cap: None,
                haze_steps_cap: None,
                pixel_budget: None,
                frame_rate: 30.0,
            },
            // A laptop budget: twice the shadow walk's footprint allowance
            // and half its steps, and twice again while the camera moves.
            // The bounce is coarser: discs further apart, fewer of them,
            // and looser links.
            Quality::Medium => Self {
                marched: true,
                fixture_shadows: true,
                plain_pieces: 4.0,
                shadow_footprint: 2.0,
                shadow_steps: 0.5,
                moving_coarsening: 2.0,
                shadow_redraws: 16,
                fog_tile: crate::fog_grid::LOW_TILE_SIZE,
                msaa: 1,
                ambient_divisor: 1,
                bounce: Some(crate::bounce::stage::Budget {
                    spacing: 0.8,
                    discs: 4_000.0,
                    error: 0.04,
                    step: 8,
                }),
                sun_shafts: (4, 12),
                haze_glow: crate::haze_glow::LOW,
                clouds: clouds::Budget {
                    divisor: 4,
                    steps: 32,
                    light_steps: 4,
                    panorama: (256, 128),
                    shadow: 512,
                },
                glare_fft: 1,
                floor_detail: false,
                haze_resolution_cap: Some(0.5),
                haze_steps_cap: Some(4),
                pixel_budget: Some(1.0e6),
                frame_rate: 60.0,
            },
            Quality::Low => Self {
                marched: false,
                fixture_shadows: false,
                ambient_divisor: 2,
                bounce: None,
                haze_resolution_cap: Some(0.25),
                frame_rate: 120.0,
                ..Self::of(Quality::Medium)
            },
        }
    }

    /// This budget while the camera moves.
    #[must_use]
    pub(crate) fn while_moving(self) -> Self {
        Self {
            shadow_footprint: self.shadow_footprint * self.moving_coarsening,
            shadow_steps: self.shadow_steps / self.moving_coarsening,
            ..self
        }
    }

    /// `Haze::quality`.
    #[must_use]
    pub(crate) fn haze(self) -> [f32; 4] {
        [
            self.plain_pieces,
            self.shadow_footprint,
            self.shadow_steps,
            0.0,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::QualityBudget;
    use crate::scene_desc::Quality;

    #[test]
    fn each_level_buys_no_more_than_the_one_above() {
        for pair in Quality::ALL.windows(2) {
            let (above, below) = (QualityBudget::of(pair[0]), QualityBudget::of(pair[1]));
            assert!(below.msaa <= above.msaa);
            let (steps, resolution) = (8, 1.0);
            let (above_steps, above_resolution) = pair[0].limit_haze(steps, resolution);
            let (below_steps, below_resolution) = pair[1].limit_haze(steps, resolution);
            assert!(below_steps <= above_steps && above_steps <= steps);
            assert!(below_resolution <= above_resolution && above_resolution <= resolution);
            assert!(below.ambient_divisor >= above.ambient_divisor);
            assert!(above.marched || !below.marched);
            assert!(above.fixture_shadows || !below.fixture_shadows);
            assert!(below.frame_rate >= above.frame_rate);
        }
        for quality in Quality::ALL {
            let still = QualityBudget::of(quality);
            let moving = still.while_moving();
            assert!(moving.shadow_footprint >= still.shadow_footprint);
            assert!(moving.shadow_steps <= still.shadow_steps);
        }
    }
}
