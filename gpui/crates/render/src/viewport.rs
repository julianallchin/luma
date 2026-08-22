//! Live presentation: the renderer driven at interactive rates, and the one
//! seam a compositor reads its pixels through.
//!
//! # Why this is a module and not a call to [`Renderer::render`]
//!
//! An offline golden and a live viewport want the same passes and different
//! everything else. The golden renders once, at a fixed size, accumulating
//! [`crate::DEFAULT_SUBFRAMES`] jitter samples, and hands back an owned `Vec`
//! that becomes a PNG. A viewport renders sixty times a second, at whatever
//! size the window drag left the element at, can afford a fraction of that
//! jitter budget, and hands its pixels to a compositor that wants them in a
//! different channel order. [`Viewport`] is those differences, held in one
//! place, so neither caller carries the other's shape.
//!
//! # The presentation seam
//!
//! [`Viewport::draw`] hands back a [`Presentation`]: a borrowed, row-major,
//! unpadded BGRA8 view of one frame. That borrow *is* the seam. v1 of
//! `docs/specs/wgpu-renderer.md` §4.2 is a readback, so the bytes are on the
//! CPU and the host copies them into whatever its image type is. The v2
//! zero-copy path — an IOSurface-backed texture handed to the compositor —
//! replaces the body of `draw` and the shape of `Presentation`, and nothing
//! above the seam changes: the host still asks for a frame at a size and gets
//! back something it can present.
//!
//! Deliberately *not* here: frame pacing, resize debouncing and input. Those
//! are the compositor's, because only it knows when a repaint is wanted — this
//! crate has no windowing and gains none.

use std::time::{Duration, Instant};

use crate::frame::Frame;
use crate::gpu::{Channels, Renderer};

/// Jitter subframes a live frame accumulates.
///
/// [`crate::DEFAULT_SUBFRAMES`] is an export dial — sixteen passes over the
/// whole scene, chosen because a golden has all the time in the world. Live has
/// 16 ms for everything, and the residual variance of two jittered samples is
/// what the three.js path's temporal EMA settled to within a few frames anyway.
///
/// If two grains visibly, the fix is the temporal pass of spec §2.5 — a real
/// missing stage — and not this number.
pub const LIVE_SUBFRAMES: u32 = 2;

/// Fraction of the output resolution a live frame's haze pass runs at.
///
/// The haze is a full-screen ray-march and dominates a lit frame; a half-size
/// target is a quarter of those invocations. It is not a corner cut invented
/// here — `hazeResolution` is a dial the web's render settings already carry,
/// and the composite's depth-aware bilateral upsample exists precisely to put
/// a low-res haze back at native resolution without smearing it across
/// silhouettes. Measured against the three.js captures, a live frame at
/// `0.5` scores the same SSIM as the sixteen-subframe export path.
///
/// The goldens pin it at `1.0`, so the export image is untouched.
pub const LIVE_HAZE_RESOLUTION: f32 = 0.5;

/// One rendered frame, as the presentation layer receives it.
///
/// BGRA8, sRGB-encoded, row-major, no row padding: `pixels.len()` is exactly
/// `width * height * 4`. Borrowed from the [`Viewport`] that drew it, so the
/// buffer is reused frame to frame rather than reallocated.
pub struct Presentation<'a> {
    /// Physical pixels across.
    pub width: u32,
    /// Physical pixels down.
    pub height: u32,
    /// `width * height` BGRA8 texels.
    pub pixels: &'a [u8],
    /// End-to-end wall time for this draw, including synchronous readback.
    pub draw_time: Duration,
}

/// The renderer, kept alive across frames and pointed at a resizable surface.
///
/// Owns the device, every pipeline, the render targets and the readback
/// buffer, so a frame costs a submit and a copy rather than a reconstruction.
pub struct Viewport {
    renderer: Renderer,
    pixels: Vec<u8>,
    subframes: u32,
    last_draw_time: Option<Duration>,
}

impl Viewport {
    /// Acquire a GPU and build the pipelines.
    ///
    /// # Errors
    /// Fails when no wgpu adapter or device can be acquired — which is the
    /// honest answer on a machine with no GPU, and is why this is a `Result`
    /// rather than a panic: a host with no viewport is still a host.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            renderer: Renderer::new()?,
            pixels: Vec::new(),
            subframes: LIVE_SUBFRAMES,
            last_draw_time: None,
        })
    }

    /// Trade image quality for frame time. `1` is one jittered sample per
    /// frame; see [`LIVE_SUBFRAMES`].
    pub fn set_subframes(&mut self, subframes: u32) {
        self.subframes = subframes.max(1);
    }

    /// CPU wall time of the previous [`Self::draw`], including queue submit,
    /// GPU completion and readback. This deliberately does not claim to be a
    /// GPU timestamp; it measures the end-to-end cost the current synchronous
    /// presentation seam imposes on its caller.
    #[must_use]
    pub fn last_draw_time(&self) -> Option<Duration> {
        self.last_draw_time
    }

    /// Render `frame` at `width` x `height` physical pixels.
    ///
    /// Reallocates render targets only when the size changed, so a resize drag
    /// costs one reallocation per distinct size rather than one per frame.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub fn draw(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Presentation<'_>> {
        // A zero-sized element is a laid-out element that has not been given
        // room yet, not an error — wgpu would reject the extent, so clamp.
        let width = width.max(1);
        let height = height.max(1);
        let started = Instant::now();
        let result = self.renderer.render_into(
            frame,
            width,
            height,
            self.subframes,
            Channels::Bgra,
            &mut self.pixels,
        );
        self.last_draw_time = Some(started.elapsed());
        result?;
        Ok(Presentation {
            width,
            height,
            pixels: &self.pixels,
            draw_time: self.last_draw_time.unwrap_or_default(),
        })
    }
}
