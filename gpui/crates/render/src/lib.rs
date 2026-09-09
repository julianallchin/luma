//! Offscreen wgpu rendering for the stage and waveform strips.
//!
//! Independent pipelines share a device context and compositor-compatible
//! render targets. Live presentation shares textures on supported platforms;
//! capture and export can explicitly read pixels back. This crate has no GPUI
//! or windowing dependency.

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
pub mod atmosphere;
pub(crate) mod cables;
pub mod catalog;
pub mod coords;
pub mod device;
mod environment;
pub mod face;
mod fog_grid;
pub mod frame;
mod gpu;
mod haze_field;
pub mod house;
pub mod image_out;
pub mod light_index;
pub mod luminaire;
mod medium;
pub mod metrics;
pub mod overlay;
pub mod scene_desc;
mod shadow;
mod share;
pub mod truss;
pub mod venue_tiles;
pub mod viewport;
pub mod warmup;
pub mod waveform;

pub use frame::{build as build_frame, build_with as build_frame_with, Frame, StateSource};
pub use gpu::{CpuSpans, FrameTimings, Gpu, Renderer, RendererProfile, ShadowStats, UploadStats};
pub use light_index::LightIndexStats;
pub use metrics::MetricSummary;
pub use scene_desc::Catalogue;
pub use share::Surface;
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

mod visibility;
