//! `luma-render` — the offscreen wgpu light-transport renderer.
//!
//! Implements the renderer core of `docs/specs/wgpu-renderer.md`: Z-up world,
//! a closed set of materials, the analytic volumetric-haze pass, and one merged
//! composite + `AgX` display transform, all rendering into a texture that is read
//! back as bytes. There is no windowing here and no gpui dependency —
//! `luma-viewport` will own presentation, and video export writes the same
//! bytes to a pipe.
//!
//! Its acceptance test is the golden-scene gauntlet: the eight scenes in
//! `src/harness/golden-scenes.ts` are rendered here and compared against the
//! committed three.js captures with `harness/compare-shots.mjs`.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
// Graphics code converts bounded indices, counts and pixel coordinates between
// integers and f32 on nearly every line; the magnitudes here are in the
// thousands, nowhere near an f32 mantissa. Denying these would mean a `try_into`
// per shader constant and would obscure the arithmetic the goldens depend on.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
// `Renderer::new`, `Renderer::render` and `frame::build` are long and linear:
// pipeline construction and scene assembly, in one order, with no branching to
// factor out. Splitting them would produce pass-through helpers, which §4 of the
// design rules calls a smell in its own right.
#![allow(clippy::too_many_lines)]

pub mod assets;
pub mod coords;
mod environment;
pub mod frame;
mod gpu;
mod haze_field;
pub mod light_index;
pub mod luminaire;
pub mod metrics;
pub mod overlay;
pub mod scene_desc;
#[cfg(target_os = "macos")]
mod shadow;
mod share;
pub mod viewport;
pub mod warmup;

pub use frame::{build as build_frame, build_with as build_frame_with, Frame, StateSource};
pub use gpu::{CpuSpans, FrameTimings, Gpu, Renderer, RendererProfile, ShadowStats, UploadStats};
pub use light_index::LightIndexStats;
pub use metrics::MetricSummary;
pub use scene_desc::Catalogue;
#[cfg(target_os = "macos")]
pub use viewport::Surface;
pub use viewport::{
    AsyncPresentation, AsyncViewport, Occupancy, Pacing, Presentation, Presented, SubmitOutcome,
    Viewport, LIVE_HAZE_RESOLUTION, LIVE_SUBFRAMES,
};
pub use warmup::{warm, warming, Warming};

/// Jitter subframes accumulated per **exported** output frame.
///
/// The live path runs an exponential moving average with `alpha = 0.4`, whose
/// residual variance is that of roughly four independent samples. Sixteen is a
/// visibly cleaner image than the goldens converge to, chosen because it is
/// deterministic and cheap; it is a quality dial with no other consequence
/// (spec §6).
///
/// The live path has its own, much smaller, budget: [`LIVE_SUBFRAMES`].
pub const DEFAULT_SUBFRAMES: u32 = 16;
