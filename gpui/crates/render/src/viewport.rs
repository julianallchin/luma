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
//! Deterministic capture uses [`Viewport::draw`] and its borrowed
//! [`Presentation`]. Live UI uses [`AsyncViewport`]: a bounded ring of slots
//! around a renderer-owned thread, with nonblocking submit/take operations.
//! Where the platform can share memory with the compositor the bytes never
//! cross the CPU at all; where it cannot they are read back, and either way the
//! polling and mapping happen off the UI thread.
//!
//! Deliberately *not* here: frame pacing, resize debouncing and input. Those
//! are the compositor's, because only it knows when a repaint is wanted — this
//! crate has no windowing and gains none.
//!
//! *Measuring* the pacing is here, though, and is not the same thing. Deciding
//! when to repaint needs a window; observing how far apart the repaints landed
//! needs only the seam they cross, which is [`AsyncViewport::take_latest`]. See
//! [`Pacing`] for why that measurement has to exist beside the GPU timings
//! rather than instead of them.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::frame::Frame;
use crate::gpu::{Channels, FrameTimings, Gpu, PendingFrame, Renderer};
use crate::gpu::{CpuSpans, ShadowStats};
use crate::light_index::LightIndexStats;
use crate::metrics::MetricSummary;

/// In-flight presentation resources.
///
/// [`RESERVED`] of these are always withheld — a shared surface stays readable
/// by the compositor after the UI has moved on from it — so the renderer's
/// usable depth is this minus two.
///
/// # Why four, and why not six
///
/// Six was tried, shipped, measured on the operator's machine, and reverted.
/// It is worth recording why, because the arithmetic for raising it is
/// seductive and wrong.
///
/// There is no async compute in this wgpu/Metal path: one queue, serial
/// dispatch. Frames in flight therefore do not execute in parallel, they
/// execute in turn, so depth cannot raise throughput when the GPU is the
/// bottleneck — it only decides how many frames sit in the queue ahead of
/// yours. Measured live, `until_signalled` against frames already rendering at
/// submit: 5.6 ms at none, 11.9 at one, 20.0 at two, 47.0 at three. Going four
/// to six moved p95 per-frame latency from 12.8 ms to 52.7 ms and left the
/// fraction of wall clock lost to late frames at 49%, exactly where four slots
/// had it.
///
/// A harness measurement did say six was better (−41% interval), and it was
/// honest — but it was taken in a latency-bound regime, where `until_signalled`
/// was 14.5 ms against 0.72 ms of GPU pass time, and latency is precisely what
/// a pipeline hides. The machine that matters is throughput-bound at its window
/// size, and nothing hides that. See `docs/design/volumetrics-v2.md` §26–§29.
///
/// Five, not four, because [`RESERVED`] is three: usable depth is the
/// difference, and it is **two** either way. The count moved to keep that
/// difference while widening the reservation — it does not re-open the finding
/// above, which was about usable depth going from two to four.
pub const PRESENTATION_SLOTS: usize = 5;

/// How many presented frames stay reserved against reuse.
///
/// # The rule is `RESERVED >= drawables + 1`
///
/// Not `>=`-the-drawable-count, and the extra one is not slack — it comes from
/// *where* the two operations sit in a frame.
///
/// The window's `CAMetalLayer` bounds how many of its command buffers may be
/// executing at once (`set_maximum_drawable_count`, `D`). Each samples the
/// `IOSurface` that was current when it was encoded, on a different Metal queue,
/// with nothing fencing it against this renderer's writes. `next_drawable` is
/// the only thing that blocks, and it runs in *paint*; [`AsyncViewport::
/// take_latest`], which releases `S(n - RESERVED)` back to the worker, runs in
/// *prepaint*. So the most recent guarantee in force when a surface is released
/// is frame `n-1`'s acquire, not frame `n`'s: `CB(n-1-D)` has been displayed,
/// and `{CB(n-D) ..= CB(n-1)}` may still be reading `S(n-D) ..= S(n-1)`.
/// Safety needs `n - RESERVED < n - D`.
///
/// Getting this wrong is an unfenced write-after-read that appears only when the
/// GPU backs up past a frame period — during exactly the hitch someone would be
/// debugging.
///
/// With `D = 2` in the vendored `gpui_apple/src/metal_renderer.rs`, three is the
/// smallest safe value. Change either number and you must recheck the other; the
/// comment there says the same from its side.
///
/// # The standing invitation to delete this coupling
///
/// A counting argument that two files must agree on is exactly the kind of
/// invariant a vendored-dependency refresh can silently falsify. Retaining the
/// `CVPixelBuffer` in the window command buffer's completed handler and
/// releasing the slot from that callback would replace it with a real fence, and
/// then neither number needs to know about the other.
const RESERVED: usize = 3;

// Reserving every slot would leave the worker nowhere to draw and
// `Slots::announce` nowhere to report a dead renderer. Checked here rather than
// asserted at runtime because it is a property of two constants.
const _: () = assert!(
    RESERVED < PRESENTATION_SLOTS,
    "at least one presentation slot must stay unreserved"
);

/// Jitter subframes a live frame accumulates.
///
/// [`crate::DEFAULT_SUBFRAMES`] is an export dial — sixteen passes over the
/// whole scene, chosen because a golden has all the time in the world. Live has
/// 16 ms for everything. Two blue-noise samples feed the depth-rejecting live
/// history resolve; history resets on resize, camera/FOV changes, medium or
/// cone-topology changes, and track-time discontinuities. Capture bypasses
/// history entirely and fixes its sample seeds.
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

/// A frame's memory, addressable by the window compositor.
///
/// `core-video`'s `CVPixelBuffer` is not `Send` — its binding is a raw pointer
/// — but the object behind it is a CoreFoundation type backed by an
/// `IOSurface`, which Apple documents as shareable across threads *and*
/// processes. Sending one from the renderer thread to the UI thread is the
/// modest end of what it is for. This newtype is where that reasoning lives, so
/// no call site has to repeat it.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct Surface(core_video::pixel_buffer::CVPixelBuffer);

// SAFETY: retain, release and the `IOSurface` backing are all thread-safe; the
// pointer is the only reason the binding is not `Send` already.
#[cfg(target_os = "macos")]
unsafe impl Send for Surface {}

#[cfg(target_os = "macos")]
impl Surface {
    pub(crate) fn new(buffer: core_video::pixel_buffer::CVPixelBuffer) -> Self {
        Self(buffer)
    }

    /// A retained handle for the compositor to paint.
    #[must_use]
    pub fn pixel_buffer(&self) -> core_video::pixel_buffer::CVPixelBuffer {
        self.0.clone()
    }

    /// Copy the texels out, unpadded.
    ///
    /// The surface's rows are aligned for the display hardware, so this is a
    /// per-row copy rather than one memcpy.
    fn to_bytes(&self) -> Vec<u8> {
        use core_foundation::base::TCFType;
        use core_video::pixel_buffer::{
            CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
            CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferUnlockBaseAddress,
        };
        /// `kCVPixelBufferLock_ReadOnly`.
        const READ_ONLY: u64 = 1;

        // SAFETY: the buffer is alive for this borrow; lock and unlock are
        // balanced and the base address is valid between them, which is the
        // whole contract of the lock.
        unsafe {
            let raw = self.0.as_concrete_TypeRef();
            let width = CVPixelBufferGetWidth(raw);
            let height = CVPixelBufferGetHeight(raw);
            let row_bytes = width * 4;
            if CVPixelBufferLockBaseAddress(raw, READ_ONLY) != 0 {
                return Vec::new();
            }
            let stride = CVPixelBufferGetBytesPerRow(raw);
            let base: *const u8 = CVPixelBufferGetBaseAddress(raw).cast();
            let mut pixels = Vec::with_capacity(row_bytes * height);
            for row in 0..height {
                pixels.extend_from_slice(std::slice::from_raw_parts(
                    base.add(row * stride),
                    row_bytes,
                ));
            }
            CVPixelBufferUnlockBaseAddress(raw, READ_ONLY);
            pixels
        }
    }
}

/// Where a finished frame's pixels are.
///
/// The two are the same picture, and a caller that only paints it need not care
/// which it has — but a caller that *can* hand a surface to its compositor
/// should, because that is the variant that never crossed the memory bus.
pub enum Presented {
    /// Owned bytes: `width * height` texels in the requested channel order,
    /// row-major, no row padding.
    Pixels(Vec<u8>),
    /// Memory the window compositor can address directly, holding the same
    /// BGRA8 texels. Retained, so it outlives the viewport that drew it.
    #[cfg(target_os = "macos")]
    Shared(Surface),
}

impl Presented {
    /// The frame's texels as bytes, copying them out if they are not already in
    /// CPU memory.
    ///
    /// That copy is the cost this whole type exists to avoid, so this is for
    /// callers that genuinely need bytes — a test comparing two frames, an
    /// encoder — and never for putting a frame on screen.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Pixels(pixels) => pixels.clone(),
            #[cfg(target_os = "macos")]
            Self::Shared(surface) => surface.to_bytes(),
        }
    }

    /// Take ownership of the bytes, when this frame has any of its own.
    #[must_use]
    pub fn into_pixels(self) -> Option<Vec<u8>> {
        match self {
            Self::Pixels(pixels) => Some(pixels),
            #[cfg(target_os = "macos")]
            Self::Shared(_) => None,
        }
    }
}

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

/// An owned live frame completed by the renderer worker.
///
/// Unlike [`Presentation`], this can cross the worker/UI boundary. Pixels are
/// retained by an `Arc`, so taking the latest frame never waits for or borrows
/// the renderer.
pub struct AsyncPresentation {
    /// Monotonic submission number.
    pub serial: u64,
    /// Physical pixels across.
    pub width: u32,
    /// Physical pixels down.
    pub height: u32,
    /// `width * height` BGRA8 texels, wherever they live.
    pub image: Presented,
    /// End-to-end time spent on the renderer thread.
    pub draw_time: Duration,
    /// Independent CPU encode/submit and GPU pass timings from the most
    /// recently profiled frame, which is usually an earlier one than this.
    ///
    /// A profiled frame carries a second command submission and so retires
    /// later than the unprofiled frames around it — late enough that on a
    /// heavy scene it is always the stale one `complete` drops. Handing its
    /// timings to the next frame out costs an attribution the consumers never
    /// made anyway (they trend these) and is the difference between a number
    /// and no number at all on exactly the frames worth measuring. `None` until
    /// the first profiled frame lands, and on adapters without timestamp
    /// queries, which still present frames.
    pub timings: Option<FrameTimings>,
    /// What the fixture-shadow passes submitted for this frame. Always present:
    /// counted while encoding rather than measured by the adapter.
    pub shadows: ShadowStats,
    /// The clustered-light index this frame shaded against. `mean` is the
    /// number that says whether culling works at all: near the cone count,
    /// every fragment is shading every light and the grid is pure overhead.
    pub clusters: LightIndexStats,
    /// How long this frame waited between the UI thread submitting it and the
    /// worker starting it. Non-zero means the renderer was busy, not slow.
    pub queued: Duration,
    /// Where the CPU time between claiming this frame's slot and handing it to
    /// the driver went. Always present, and the only view of the span in which
    /// a slot already reads `Rendering` but nothing has reached the GPU.
    pub cpu: CpuSpans,
    /// `draw_time`, split at the driver's completion callback: how long the
    /// GPU took to say it was done, and how long after that the worker
    /// noticed. A big second half is nobody looking, not anything being slow.
    pub until_signalled: Option<Duration>,
    /// The second half of that split: how long after the driver signalled
    /// before the worker looked.
    pub until_noticed: Option<Duration>,
    /// Wall time since the previous frame was handed over, or `None` for the
    /// first. Read it against `timings` to attribute a hitch — see [`Pacing`].
    pub since_previous: Option<Duration>,
}

/// What a nonblocking live-frame submission did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Used a vacant presentation slot.
    Queued,
    /// Replaced an older frame which had not reached the screen.
    Replaced {
        /// Serial of the obsolete frame.
        dropped_serial: u64,
    },
}

/// How many recent presentation intervals [`Pacing`] keeps.
///
/// Four seconds at 60 Hz. Long enough that a p95 means something, short enough
/// that it still describes what the stage is doing *now* rather than averaging
/// away a hitch that happened when the window was first opened.
const PACING_WINDOW: usize = 240;

/// Wall-clock spacing of frame deliveries at the presentation seam.
///
/// [`FrameTimings`] says how long the GPU took. This says how long the screen
/// waited. They answer different halves of one question and neither can be
/// derived from the other, which is the whole reason both exist: a 33 ms
/// interval behind a 5 ms `gpu_total_ms` is nobody asking for frames, and the
/// same interval behind a 30 ms one is the renderer unable to make them. A
/// profile that reports only GPU time cannot tell those apart, and they have
/// opposite fixes.
///
/// The cost is one `Instant::now` and one array store per presented frame, so
/// it is always on — a measurement that has to be switched on is a measurement
/// nobody has when the hitch happens.
#[derive(Debug)]
pub struct Pacing {
    previous: Option<Instant>,
    /// Most recent intervals in milliseconds, oldest overwritten first.
    recent: [f64; PACING_WINDOW],
    next: usize,
    /// Live entries in `recent`, saturating at its length.
    len: usize,
    delivered: u64,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            previous: None,
            recent: [0.0; PACING_WINDOW],
            next: 0,
            len: 0,
            delivered: 0,
        }
    }
}

impl Pacing {
    /// Note that a frame has just been handed over, returning the interval
    /// since the previous one.
    ///
    /// `None` for the first frame of a session, which has no predecessor to be
    /// spaced from — deliberately not zero, which would be a real interval and
    /// would drag every percentile down by one sample.
    fn record(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let interval = self.previous.replace(now).map(|previous| now - previous);
        self.delivered += 1;
        if let Some(interval) = interval {
            self.recent[self.next] = interval.as_secs_f64() * 1e3;
            self.next = (self.next + 1) % PACING_WINDOW;
            self.len = (self.len + 1).min(PACING_WINDOW);
        }
        interval
    }

    /// Distribution of the last few seconds of intervals, or `None` before two
    /// frames have been presented.
    #[must_use]
    pub fn summary(&self) -> Option<MetricSummary> {
        MetricSummary::of(self.recent[..self.len].iter().copied())
    }

    /// Frames handed to the presentation caller since this viewport opened.
    ///
    /// The denominator the intervals are silent about: a stage that presented
    /// four frames in ten seconds has excellent percentiles over three samples.
    #[must_use]
    pub fn delivered(&self) -> u64 {
        self.delivered
    }
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
        let result = self.renderer.render_live_into(
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

/// Live-only asynchronous presentation.
///
/// A bounded set of GPU output slots ([`PRESENTATION_SLOTS`]) binds in-flight
/// work. The UI only
/// calls [`Self::submit`] and [`Self::take_latest`], both short mutex operations;
/// device creation, queue submission, `map_async` polling and retirement all
/// live on the owned renderer thread. One additional descriptor is coalesced
/// newest-wins while all GPU slots are occupied, rather than back-pressuring
/// the UI thread.
pub struct AsyncViewport {
    shared: Arc<Shared<FrameRequest, anyhow::Result<AsyncPresentation>>>,
    next_serial: u64,
    subframes: u32,
    /// Behind a lock because [`Self::take_latest`] takes `&self`: the caller
    /// holding a shared viewport is the one presenting frames, and making it
    /// take `&mut` to be measured would push this measurement's cost into
    /// every call site's borrow shape.
    pacing: Mutex<Pacing>,
}

struct FrameRequest {
    frame: Frame,
    width: u32,
    height: u32,
    subframes: u32,
    /// When the UI thread handed this frame over.
    ///
    /// The worker takes the newest request whenever it is free, so a frame can
    /// sit here while an older one finishes. That wait is invisible in
    /// `draw_time`, which starts at *submit to the GPU* — without it, a
    /// renderer that is simply backed up and one that is slow look identical.
    submitted: Instant,
}

struct Shared<J, R> {
    slots: Mutex<Slots<J, R>>,
    work: Condvar,
    /// What the worker has finished, counted where frames retire rather than
    /// where they are presented.
    ///
    /// `complete` drops any result at or below `last_presented` — so the frames
    /// that stalled are exactly the ones whose timings are discarded, and no
    /// `FrameSample` ever carries them. Without a count taken on the retire
    /// path, a stall in which the worker completes nothing and one in which it
    /// completes frames the UI then throws away look identical from a report.
    finished: Finished,
}

/// Worker completions, readable from the UI thread without taking the slot lock.
#[derive(Default)]
struct Finished {
    /// Frames retired since the viewport opened, delivered or discarded.
    count: std::sync::atomic::AtomicU64,
    /// The most recently retired frame's submit-to-completion span, in
    /// microseconds. Microseconds because this is a lock-free scalar and a
    /// `Duration` is two words.
    last_signalled_us: std::sync::atomic::AtomicU64,
}

struct Slots<J, R> {
    gpu: [GpuSlot<R>; PRESENTATION_SLOTS],
    queued: Option<Queued<J>>,
    /// Greatest serial handed to the presentation caller. GPU maps may retire
    /// out of order, so no later completion at or below this boundary may
    /// become Ready — including an error from an obsolete frame.
    last_presented: u64,
    /// Slots handed to the presentation caller, most recent first, that no new
    /// frame may be rendered into.
    ///
    /// A frame used to stop belonging to its slot the moment its pixels were
    /// copied out. A shared frame is never copied out, so the slot *is* the
    /// picture for as long as the picture is on screen, and drawing into it
    /// would tear the frame being displayed. Reserving here rather than in the
    /// renderer is deliberate: this is the only place that knows which frame
    /// reached the screen.
    reserved: [Option<usize>; RESERVED],
    stopped: bool,
}

struct Queued<J> {
    serial: u64,
    job: J,
}

/// What the presentation slots were doing at the moment a frame was submitted.
///
/// The counts, not just a "full" flag: `RESERVED` slots are held because their
/// shared surface is still on screen, and a pipeline stalled on *that* is a
/// different fault from one stalled on slow rendering. With
/// [`PRESENTATION_SLOTS`] and [`RESERVED`], only the difference between them may
/// be startable at once, which is a ceiling worth being able to see rather than
/// infer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Occupancy {
    /// Slots with nothing in them.
    pub idle: u8,
    /// Slots with a frame on the GPU.
    pub rendering: u8,
    /// Slots holding a finished frame nobody has taken yet.
    pub ready: u8,
    /// Slots withheld because the frame in them is still being displayed.
    pub reserved: u8,
    /// Whether this submission could have been started immediately. False is
    /// the state that makes a frame wait, and so the state `queued_ms`
    /// measures.
    pub startable: bool,
}

enum GpuSlot<R> {
    Idle,
    Rendering { serial: u64 },
    Ready { serial: u64, result: R },
}

impl<J, R> Default for Slots<J, R> {
    fn default() -> Self {
        Self {
            gpu: std::array::from_fn(|_| GpuSlot::Idle),
            queued: None,
            last_presented: 0,
            reserved: [None; RESERVED],
            stopped: false,
        }
    }
}

impl<J, R> Slots<J, R> {
    fn submit(&mut self, serial: u64, job: J) -> (SubmitOutcome, Option<J>) {
        let replaced = self.queued.replace(Queued { serial, job });
        replaced.map_or((SubmitOutcome::Queued, None), |old| {
            (
                SubmitOutcome::Replaced {
                    dropped_serial: old.serial,
                },
                Some(old.job),
            )
        })
    }

    /// A census of the slots, for telemetry. Cheap: four enum discriminants.
    fn occupancy(&self) -> Occupancy {
        let mut census = Occupancy {
            startable: self.startable_slot().is_some(),
            ..Occupancy::default()
        };
        for (index, slot) in self.gpu.iter().enumerate() {
            if self.reserved.contains(&Some(index)) {
                census.reserved += 1;
            }
            match slot {
                GpuSlot::Idle => census.idle += 1,
                GpuSlot::Rendering { .. } => census.rendering += 1,
                GpuSlot::Ready { .. } => census.ready += 1,
            }
        }
        census
    }

    /// The slot a new frame would be rendered into. Idle first; failing that,
    /// a completed frame the UI has not consumed and no longer needs to.
    /// Reserved slots are neither, whatever state they are in.
    ///
    /// # Why the newest completed frame is never recyclable
    ///
    /// Recycling exists so a ring full of stale completions cannot deadlock the
    /// worker. But the *newest* completed frame is not stale — it is precisely
    /// what the next `take_latest` will present. Overwriting it renders a frame
    /// nobody ever sees, and worse, hides the completion: the slot goes back to
    /// `Rendering`, so the census the UI reads never shows a `Ready` frame and
    /// the stall is invisible from every field that describes slots.
    ///
    /// At [`PRESENTATION_SLOTS`] minus [`RESERVED`] — two usable slots — that
    /// is not a rare race but the steady state. The UI queues a descriptor
    /// every frame, so the worker almost always has work waiting; when one of
    /// the two usable slots is `Rendering` and the other has just completed,
    /// the only recyclable slot *is* the newest, and the worker takes it. The
    /// UI then finds nothing ready, submits again, and the cycle repeats for as
    /// long as the phasing holds — measured at 15 completed frames destroyed
    /// across one 163 ms freeze, with the GPU signalling healthily at 19–21 ms
    /// throughout.
    fn startable_slot(&self) -> Option<usize> {
        let free = |index: &usize| !self.reserved.contains(&Some(*index));
        let ready = |slot: &GpuSlot<R>| match slot {
            GpuSlot::Ready { serial, .. } => Some(*serial),
            GpuSlot::Idle | GpuSlot::Rendering { .. } => None,
        };
        // Read across every slot, not just the free ones: a reserved slot's
        // frame is still on screen and still the newest thing completed, and
        // the answer must not change with the reservation rotation.
        let newest_completed = self
            .gpu
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| ready(slot).map(|serial| (index, serial)))
            .max_by_key(|(_, serial)| *serial)
            .map(|(index, _)| index);
        self.gpu
            .iter()
            .enumerate()
            .filter(|(index, _)| free(index))
            .find(|(_, slot)| matches!(slot, GpuSlot::Idle))
            .map(|(index, _)| index)
            .or_else(|| {
                self.gpu
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| free(index) && Some(*index) != newest_completed)
                    .filter_map(|(index, slot)| ready(slot).map(|serial| (index, serial)))
                    .min_by_key(|(_, serial)| *serial)
                    .map(|(index, _)| index)
            })
    }

    /// Whether the worker has something to start and somewhere to start it.
    ///
    /// Asks [`Self::startable_slot`] rather than restating its rule. A second
    /// opinion here does not produce a wrong frame — it produces a worker that
    /// spins on a queue it will not be allowed to begin, which is a live-lock
    /// with no stack trace and no failing assertion.
    fn can_begin(&self) -> bool {
        self.queued.is_some() && self.startable_slot().is_some()
    }

    /// Bind the newest descriptor to a startable GPU resource.
    fn begin_latest(&mut self) -> Option<(usize, u64, J, Option<R>)> {
        let slot = self.startable_slot()?;
        let Queued { serial, job } = self.queued.take()?;
        let old = std::mem::replace(&mut self.gpu[slot], GpuSlot::Rendering { serial });
        let displaced = match old {
            GpuSlot::Ready { result, .. } => Some(result),
            GpuSlot::Idle => None,
            GpuSlot::Rendering { .. } => unreachable!("selected a busy GPU slot"),
        };
        Some((slot, serial, job, displaced))
    }

    /// Install a result nobody rendered, outranking everything present.
    ///
    /// [`Self::force_ready`] speaks for a frame that was in flight. This speaks
    /// for the renderer itself, which is why it needs no slot to have been
    /// rendering and no serial to have been issued: it mints one above every
    /// serial in play so [`Self::take_latest`] cannot prefer a stale picture
    /// over it.
    ///
    /// Returns whatever the chosen slot was holding, for the caller to release
    /// once the state lock is gone.
    fn announce(&mut self, result: R) -> Option<R> {
        // `last_presented` alone is not the ceiling, and the case that proves
        // it is the ordinary one: `fail_in_flight` runs first and turns every
        // in-flight slot into a `Ready` failure, so `last_presented + 1` loses
        // to a message installed one line earlier — no surviving picture
        // required.
        //
        // The `Rendering` arm is unreachable from the only caller today, which
        // drains in-flight slots before announcing. It is here so this method
        // does not inherit that as an unwritten precondition: without it,
        // announcing from a path that has not drained mints a *losing* serial,
        // which fails silently rather than loudly. Not dead code — one token of
        // fold buying a total function.
        let ceiling = self
            .gpu
            .iter()
            .filter_map(|slot| match slot {
                GpuSlot::Ready { serial, .. } | GpuSlot::Rendering { serial } => Some(*serial),
                GpuSlot::Idle => None,
            })
            .fold(self.last_presented, u64::max);
        let free = |index: &usize| !self.reserved.contains(&Some(*index));
        let slot = (0..PRESENTATION_SLOTS)
            .filter(free)
            .find(|index| matches!(self.gpu[*index], GpuSlot::Idle))
            .or_else(|| (0..PRESENTATION_SLOTS).find(free))
            .expect("RESERVED is below PRESENTATION_SLOTS, so a slot is free");
        let displaced = std::mem::replace(
            &mut self.gpu[slot],
            GpuSlot::Ready {
                serial: ceiling + 1,
                result,
            },
        );
        match displaced {
            GpuSlot::Ready { result, .. } => Some(result),
            GpuSlot::Idle | GpuSlot::Rendering { .. } => None,
        }
    }

    /// Hand a result to the caller whatever the presentation boundary says.
    ///
    /// [`Self::complete`] drops a result at or below `last_presented`, because
    /// presenting a stale *picture* would run time backwards. A failure is not
    /// about its frame — it is about the renderer, and the renderer is gone —
    /// so that rule must not apply to it.
    ///
    /// The stale case is ordinary rather than exotic: frames complete out of
    /// order, so begin 21, begin 22, complete 22 and present it leaves slot 21
    /// rendering with the boundary already past it. A worker that dies there had
    /// its only in-flight result dropped, and dropping it is precisely the
    /// silent freeze [`supervised_worker`] exists to prevent — `take_latest`
    /// returns `None` for ever while the stage goes on painting its last good
    /// frame and nothing anywhere says why.
    ///
    /// Kept separate from `complete` rather than teaching `complete` to tell a
    /// picture from a failure: `Slots` is generic over what a frame *is*, and
    /// the only caller that knows a result is an error is [`fail_in_flight`].
    fn force_ready(&mut self, slot: usize, serial: u64, result: R) {
        assert!(
            matches!(&self.gpu[slot], GpuSlot::Rendering { serial: active } if *active == serial),
            "failed frame owns its GPU slot"
        );
        self.gpu[slot] = GpuSlot::Ready { serial, result };
    }

    fn complete(&mut self, slot: usize, serial: u64, result: R) -> Option<R> {
        assert!(
            matches!(&self.gpu[slot], GpuSlot::Rendering { serial: active } if *active == serial),
            "completed frame owns its GPU slot"
        );
        if serial <= self.last_presented {
            self.gpu[slot] = GpuSlot::Idle;
            return Some(result);
        }
        self.gpu[slot] = GpuSlot::Ready { serial, result };
        None
    }

    /// Return only the freshest completed frame. Older completed frames can no
    /// longer be shown without presenting time backwards, so release them.
    fn take_latest(&mut self) -> Option<R> {
        let newest = self
            .gpu
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match slot {
                GpuSlot::Ready { serial, .. } => Some((index, *serial)),
                GpuSlot::Idle | GpuSlot::Rendering { .. } => None,
            })
            .max_by_key(|(_, serial)| *serial)?;
        // Only ever forward. `force_ready` admits a failure below this boundary
        // on purpose, and taking one must not walk the mark backwards —
        // `complete` reads it to reject stale completions, so lowering it would
        // re-admit pictures this viewport has already moved past.
        self.last_presented = self.last_presented.max(newest.1);
        self.reserved.rotate_right(1);
        self.reserved[0] = Some(newest.0);
        let mut result = None;
        for (index, slot) in self.gpu.iter_mut().enumerate() {
            if matches!(slot, GpuSlot::Ready { .. }) {
                let old = std::mem::replace(slot, GpuSlot::Idle);
                if index == newest.0 {
                    let GpuSlot::Ready { result: value, .. } = old else {
                        unreachable!()
                    };
                    result = Some(value);
                }
            }
        }
        result
    }
}

impl AsyncViewport {
    /// Start the renderer worker without acquiring a GPU on the calling thread.
    ///
    /// # Panics
    /// Panics if the operating system cannot create the renderer thread.
    #[must_use]
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            slots: Mutex::new(Slots::default()),
            work: Condvar::new(),
            finished: Finished::default(),
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("luma-render".into())
            .spawn(move || supervised_worker(&worker))
            .expect("renderer worker thread must start");
        Self {
            shared,
            next_serial: 0,
            subframes: LIVE_SUBFRAMES,
            pacing: Mutex::new(Pacing::default()),
        }
    }

    /// Trade image quality for frame time. Applied to subsequently submitted
    /// frames and clamped to at least one sample.
    pub fn set_subframes(&mut self, subframes: u32) {
        self.subframes = subframes.max(1);
    }

    /// Queue one live frame without waiting for the renderer or GPU.
    pub fn submit(&mut self, frame: Frame, width: u32, height: u32) -> SubmitOutcome {
        self.submit_numbered(frame, width, height).1
    }

    /// Queue one live frame and return the serial that will accompany its
    /// presentation. Editor hit-test snapshots use this to stay paired with
    /// the exact pixels that eventually reach the screen.
    pub fn submit_numbered(
        &mut self,
        frame: Frame,
        width: u32,
        height: u32,
    ) -> (u64, SubmitOutcome, Occupancy) {
        self.next_serial = self.next_serial.wrapping_add(1);
        let serial = self.next_serial;
        let mut slots = self
            .shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (outcome, displaced) = slots.submit(
            serial,
            FrameRequest {
                frame,
                width: width.max(1),
                height: height.max(1),
                subframes: self.subframes,
                submitted: Instant::now(),
            },
        );
        // Read under the same lock as the submission, so the census describes
        // the pipeline this frame actually joined rather than one a microsecond
        // of worker progress later.
        let occupancy = slots.occupancy();
        drop(slots);
        drop(displaced);
        self.shared.work.notify_one();
        (serial, outcome, occupancy)
    }

    /// Take the newest completed frame, if one exists, without polling or
    /// waiting for the GPU.
    ///
    /// This is the presentation seam, so it is also where the frame is timed
    /// against its predecessor — see [`Pacing`]. A failed frame is not paced:
    /// an error is not a picture, and spacing one against the last good frame
    /// would report an interval no viewer experienced.
    #[must_use]
    pub fn take_latest(&self) -> Option<anyhow::Result<AsyncPresentation>> {
        let taken = self
            .shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take_latest();
        match taken {
            Some(Ok(mut presentation)) => {
                presentation.since_previous = self
                    .pacing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .record();
                Some(Ok(presentation))
            }
            other => other,
        }
    }

    /// Frames the worker has retired, and the last one's submit-to-completion
    /// span.
    ///
    /// Read this against the slot census. A stall in which the count does not
    /// move is the GPU not signalling; a stall in which it climbs is the worker
    /// completing frames the UI is discarding as stale. The two need opposite
    /// fixes and no other field tells them apart, because `complete` drops a
    /// stale result before any sample can carry its timings.
    #[must_use]
    pub fn finished(&self) -> (u64, Duration) {
        use std::sync::atomic::Ordering;
        (
            self.shared.finished.count.load(Ordering::Relaxed),
            Duration::from_micros(
                self.shared
                    .finished
                    .last_signalled_us
                    .load(Ordering::Relaxed),
            ),
        )
    }

    /// The compositor declares a deliberate pause in presentation: the next
    /// presented frame starts a new stream instead of being spaced against
    /// the frame before the pause.
    ///
    /// A chosen gap is not an interval any viewer experienced — pacing it
    /// would report a stall, and fire everything downstream that watches for
    /// one, for a stage that was resting on purpose. Deciding *when* to pause
    /// stays the compositor's job (see the module doc); this only keeps the
    /// measurement honest about it.
    pub fn rest(&self) {
        self.pacing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .previous = None;
    }

    /// Spacing of the frames this viewport has actually put on screen.
    ///
    /// Always available and independent of adapter timestamp support, which is
    /// why it is the metric to look at first: it says whether there is a
    /// problem, and [`AsyncPresentation::timings`] says whose.
    #[must_use]
    pub fn pacing(&self) -> Option<MetricSummary> {
        self.pacing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .summary()
    }

    /// Frames handed to the presenter since this viewport opened.
    #[must_use]
    pub fn delivered(&self) -> u64 {
        self.pacing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .delivered()
    }
}

impl Default for AsyncViewport {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AsyncViewport {
    fn drop(&mut self) {
        self.shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped = true;
        // Deliberately do not join: a GPU frame already in flight must not
        // turn closing a tab into a UI-thread wait.
        self.shared.work.notify_one();
    }
}

fn retire<J, R>(shared: &Shared<J, R>, slot: usize, serial: u64, result: R) {
    let displaced = shared
        .slots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .complete(slot, serial, result);
    // Pixel storage can be large. Release a stale result after the state lock
    // is gone so UI submission/take cannot wait on its deallocation.
    drop(displaced);
}

/// Run the renderer, and survive it dying.
///
/// The worker owns a GPU device, and the ways it can die are not all in this
/// crate's control: a wgpu validation error, a lost device, a driver fault and
/// an ordinary bug all arrive as a panic on this thread. Without supervision
/// that panic is *silent* — the thread disappears, `take_latest` returns `None`
/// for ever, and the stage goes on painting its last good frame while the UI
/// stays responsive. The picture freezes and nothing anywhere says why.
///
/// So a panic here is turned into two things it can be acted on as: an error
/// delivered to whoever is waiting for a frame, so the screen can say the
/// renderer failed instead of lying with a stale image, and a fresh renderer.
///
/// What a restart deliberately does *not* rebuild is the device: [`Gpu`] is
/// process-wide and lives outside this thread's blast radius, so re-entering
/// `render_worker` costs one set of shadow atlases and no shader compilation.
/// A device that is genuinely gone is a different event with a different
/// remedy — see [`Gpu::shared`].
fn supervised_worker(shared: &Shared<FrameRequest, anyhow::Result<AsyncPresentation>>) {
    supervise(shared, render_worker);
}

/// [`supervised_worker`]'s policy, with the worker as a parameter.
///
/// Split out only so the give-up branch can be driven from a test. It is the
/// least reachable branch in this file and the most expensive one to get wrong
/// — it runs exactly when the renderer is permanently broken, which is when the
/// user most needs to be told — and a GPU-owning worker gives a test no seam to
/// reach it through.
fn supervise<J>(
    shared: &Shared<J, anyhow::Result<AsyncPresentation>>,
    worker: impl Fn(&Shared<J, anyhow::Result<AsyncPresentation>>),
) {
    // Bounded: a renderer that dies immediately and for ever would otherwise
    // spin here rebuilding devices. After this many consecutive failures the
    // stage keeps the last error rather than flickering between it and a
    // doomed restart.
    const RESTARTS: usize = 3;
    for attempt in 0..=RESTARTS {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker(shared);
        }));
        let Err(payload) = outcome else {
            return;
        };
        let reason = panic_reason(&payload);
        // Deliberately stderr and not a logging facade: this is the line that
        // has to reach a terminal a user is looking at, and it is the whole
        // difference between "it froze" and a diagnosis.
        eprintln!(
            "luma-render worker panicked ({}/{RESTARTS}): {reason}",
            attempt + 1
        );
        let restarting = attempt < RESTARTS;
        if fail_in_flight(shared, &reason) {
            return;
        }
        if !restarting {
            // `fail_in_flight` reports through the frames that were in flight,
            // and correctly says nothing when there were none — on a restarting
            // attempt that silence is right, because the worker usually comes
            // back and a fabricated error over a good frame would be a lie.
            //
            // On the last attempt it is exactly wrong. Nothing is coming back,
            // so silence here is the thread disappearing while the stage paints
            // its last good frame for ever — the failure this whole function
            // exists to prevent. And the two conditions correlate the wrong way:
            // a worker that dies on entry dies before it starts anything, so the
            // permanently-broken renderer is the one least likely to have had a
            // frame in flight to carry the news.
            //
            // So terminal failure is announced in its own right. Deliberately
            // not folded into `fail_in_flight`: that function's contract is
            // "fail what was in flight", and teaching it which attempt this is
            // would put a policy it has no business knowing next to the guard
            // that keeps it quiet when nothing was in flight.
            announce_terminal_failure(shared, &reason);
            return;
        }
    }
}

/// Tell whoever is waiting that the renderer is gone for good.
fn announce_terminal_failure<J>(
    shared: &Shared<J, anyhow::Result<AsyncPresentation>>,
    reason: &str,
) {
    let mut slots = shared
        .slots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slots.stopped {
        return;
    }
    let displaced = slots.announce(Err(anyhow::anyhow!(
        "the renderer stopped and this view will not resume \
         until it is reopened: {reason}"
    )));
    drop(slots);
    drop(displaced);
}

/// Turn a panic payload into something printable.
fn panic_reason(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|reason| (*reason).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked with a non-string payload".to_string())
}

/// Fail every slot the dead worker left mid-flight so the UI stops waiting.
///
/// Returns whether the viewport has been stopped, in which case there is
/// nothing left to restart for.
fn fail_in_flight<J>(shared: &Shared<J, anyhow::Result<AsyncPresentation>>, reason: &str) -> bool {
    let mut slots = shared
        .slots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slots.stopped {
        return true;
    }
    // The queued descriptor is gone with the thread that would have drawn it.
    slots.queued = None;
    let mut failed = false;
    for slot in 0..PRESENTATION_SLOTS {
        if let GpuSlot::Rendering { serial } = slots.gpu[slot] {
            // No slot goes straight to `Idle`. `Idle` means "nothing has ever
            // been drawn here", which makes the slot immediately startable —
            // and the renderer that just died may still have writes outstanding
            // to that surface, so handing it to the restarted worker is an
            // unfenced write-after-write.
            //
            // `force_ready` rather than `complete` because `complete` drops a
            // result at or below `last_presented`, and out-of-order completion
            // makes that state ordinary: a slot rendering an older serial than
            // the one already presented is where a worker most plausibly dies.
            // Every such failure would be dropped and the caller would never
            // learn the renderer was gone.
            //
            // Deliberately not "restarting": the worker does restart, but the
            // stage holds this failure until the pane is reopened, and a message
            // promising recovery the UI does not deliver is worse than one that
            // just says what happened.
            //
            // All of them carry the full message rather than one elected slot,
            // so whichever `take_latest` surfaces says something useful.
            let result = Err(anyhow::anyhow!(
                "the renderer stopped and this view will not resume \
                 until it is reopened: {reason}"
            ));
            slots.force_ready(slot, serial, result);
            failed = true;
        }
    }
    // A frame that finished before the worker died is still a valid picture, and
    // `take_latest` returns the highest serial — so one surviving success above
    // the failing serials wins, the errors are cleared away with it, and the
    // caller is never told the renderer is gone. That is the plain case of a
    // worker dying with a frame already in hand, not a corner of the staleness
    // rule; it needs no help from `last_presented` to happen.
    //
    // Dropping those pictures is the deliberate exception to "no slot goes
    // straight to `Idle`" above, and it is safe on both counts that make direct
    // `Idle` dangerous there: their GPU work has completed, so nothing is
    // writing the surface, and they were never presented, so nothing is
    // displaying it.
    //
    // Only once a failure is actually installed. With nothing in flight there is
    // no serial to carry a message on, and clearing the last good frame would
    // trade a misleading picture for a blank one.
    let mut stale = Vec::new();
    if failed {
        for slot in &mut slots.gpu {
            if matches!(slot, GpuSlot::Ready { result: Ok(_), .. }) {
                stale.push(std::mem::replace(slot, GpuSlot::Idle));
            }
        }
    }
    // Pixel storage can be large; release it once the state lock is gone so UI
    // submission and take cannot wait on the deallocation.
    drop(slots);
    drop(stale);
    false
}

/// How often the worker profiles a frame.
///
/// A profiled frame carries a second command submission — the query resolve,
/// which cannot ride in the frame's own buffer and stay honest (see
/// `PendingProfile`). On the shared-surface path that extra work lands inside
/// the queue-wide drain every frame already waits on, so profiling *every*
/// frame roughly halves delivery. One in sixteen is enough to watch a trend
/// and cheap enough to leave on.
const PROFILE_EVERY: u64 = 16;

fn render_worker(shared: &Shared<FrameRequest, anyhow::Result<AsyncPresentation>>) {
    // The lab should expose honest GPU time where the adapter can provide it,
    // without making timestamp support a requirement for opening a viewport.
    // Both arms borrow the same process-wide device, so the fallback costs a
    // second set of query buffers rather than a second device.
    let mut renderer = Gpu::shared()
        .map(|gpu| Renderer::profiling_on(Arc::clone(&gpu)).unwrap_or_else(|_| Renderer::on(gpu)))
        .map_err(|error| error.to_string());
    let mut pending: [Option<(u64, PendingFrame)>; PRESENTATION_SLOTS] =
        std::array::from_fn(|_| None);
    // Timestamp queries use a single ordered measurement stream. Ordinary
    // frames continue to occupy the remaining presentation slots around it.
    let mut profile_slot = None;
    let mut submitted = 0_u64;
    // Outlives the frame it was measured on, per `AsyncPresentation::timings`.
    let mut latest_timings = None;
    loop {
        if shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped
        {
            return;
        }

        let next = {
            let mut slots = shared
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slots.begin_latest()
        };
        if let Some((slot, serial, request, displaced)) = next {
            drop(displaced);
            submitted += 1;
            let measure = profile_slot.is_none() && submitted.is_multiple_of(PROFILE_EVERY);
            match &mut renderer {
                Ok(renderer) => {
                    let readback = renderer.submit_live(
                        &request.frame,
                        request.width,
                        request.height,
                        request.subframes,
                        slot,
                        measure,
                        request.submitted.elapsed(),
                    );
                    if measure {
                        profile_slot = Some(slot);
                    }
                    pending[slot] = Some((serial, readback));
                }
                Err(error) => retire(shared, slot, serial, Err(anyhow::anyhow!(error.clone()))),
            }
        }

        if let Ok(renderer) = &renderer {
            if let Err(error) = renderer.poll_live() {
                let message = error.to_string();
                for (slot, in_flight) in pending.iter_mut().enumerate() {
                    if let Some((serial, _)) = in_flight.take() {
                        retire(shared, slot, serial, Err(anyhow::anyhow!(message.clone())));
                    }
                }
                profile_slot = None;
            }
        }

        for (slot, in_flight) in pending.iter_mut().enumerate() {
            let completed = match in_flight.as_mut() {
                Some((_, readback)) => readback.try_complete(),
                None => continue,
            };
            match completed {
                Ok(None) => {}
                Ok(Some(frame)) => {
                    let (serial, _) = in_flight.take().expect("completed readback was in flight");
                    if profile_slot == Some(slot) {
                        profile_slot = None;
                    }
                    latest_timings = frame.profile.or(latest_timings);
                    // Counted here, before `retire` decides whether anyone gets
                    // to see this frame: a frame dropped for being stale is
                    // still a frame the GPU finished, and it is the only
                    // evidence that the GPU was finishing anything at all.
                    {
                        use std::sync::atomic::Ordering;
                        shared.finished.count.fetch_add(1, Ordering::Relaxed);
                        if let Some(signalled) = frame.until_signalled {
                            shared.finished.last_signalled_us.store(
                                signalled.as_micros().min(u128::from(u64::MAX)) as u64,
                                Ordering::Relaxed,
                            );
                        }
                    }
                    retire(
                        shared,
                        slot,
                        serial,
                        Ok(AsyncPresentation {
                            serial,
                            width: frame.width,
                            height: frame.height,
                            image: frame.image,
                            draw_time: frame.draw_time,
                            timings: latest_timings,
                            shadows: frame.shadows,
                            clusters: frame.clusters,
                            queued: frame.queued,
                            cpu: frame.cpu,
                            until_signalled: frame.until_signalled,
                            until_noticed: frame.until_noticed,
                            // Stamped where the frame is handed over, not
                            // where it finished: a frame that completes and
                            // then waits for the UI to ask for it has not been
                            // presented, and counting from here would hide
                            // exactly the stall this measures.
                            since_previous: None,
                        }),
                    );
                }
                Err(error) => {
                    let (serial, _) = in_flight.take().expect("failed readback was in flight");
                    if profile_slot == Some(slot) {
                        profile_slot = None;
                    }
                    retire(shared, slot, serial, Err(error));
                }
            }
        }

        let slots = shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slots.stopped {
            return;
        }
        if slots.can_begin() {
            continue;
        }
        if pending.iter().any(Option::is_some) {
            let (guard, _) = shared
                .work
                .wait_timeout(slots, Duration::from_millis(1))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(guard);
        } else {
            drop(
                shared
                    .work
                    .wait(slots)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }
}

#[cfg(test)]
mod supervision {
    use std::mem::discriminant;

    use super::*;

    fn shared() -> Shared<(), anyhow::Result<AsyncPresentation>> {
        Shared {
            slots: Mutex::new(Slots::default()),
            work: Condvar::new(),
            finished: Finished::default(),
        }
    }

    /// A frame that completed before the worker died, for the census search
    /// below. Only its serial is read.
    fn picture(serial: u64) -> AsyncPresentation {
        AsyncPresentation {
            serial,
            width: 1,
            height: 1,
            image: Presented::Pixels(Vec::new()),
            draw_time: Duration::ZERO,
            timings: None,
            shadows: crate::gpu::ShadowStats::default(),
            clusters: LightIndexStats::default(),
            queued: Duration::ZERO,
            cpu: crate::gpu::CpuSpans::default(),
            until_signalled: None,
            until_noticed: None,
            since_previous: None,
        }
    }

    /// Slot contents the searches below draw from, before serials are assigned.
    #[derive(Clone, Copy, Debug)]
    enum Shape {
        Idle,
        Rendering,
        Finished,
        Failed,
    }

    const SHAPES: [Shape; 4] = [
        Shape::Idle,
        Shape::Rendering,
        Shape::Finished,
        Shape::Failed,
    ];

    /// Every ordering of `1..=PRESENTATION_SLOTS` across the slots, so "which
    /// slot holds the newest serial" is covered rather than assumed. Both
    /// searches need it: the holes found here have all been orderings.
    fn orderings() -> Vec<Vec<u64>> {
        let mut out = Vec::new();
        let mut current: Vec<u64> = (1..=PRESENTATION_SLOTS as u64).collect();
        let mut counters = vec![0usize; PRESENTATION_SLOTS];
        out.push(current.clone());
        let mut index = 0;
        while index < PRESENTATION_SLOTS {
            if counters[index] < index {
                current.swap(if index % 2 == 0 { 0 } else { counters[index] }, index);
                out.push(current.clone());
                counters[index] += 1;
                index = 0;
            } else {
                counters[index] = 0;
                index += 1;
            }
        }
        out
    }

    /// Every assignment of `SHAPES` across the slots, as index codes.
    fn shape_codes() -> std::ops::Range<usize> {
        0..SHAPES.len().pow(PRESENTATION_SLOTS as u32)
    }

    /// Decode one shape code into per-slot contents.
    fn shapes_of(code: usize) -> [Shape; PRESENTATION_SLOTS] {
        let mut rest = code;
        std::array::from_fn(|_| {
            let shape = SHAPES[rest % SHAPES.len()];
            rest /= SHAPES.len();
            shape
        })
    }

    /// Build the census into a fresh set of slots.
    fn install(
        slots: &mut Slots<(), anyhow::Result<AsyncPresentation>>,
        shapes: &[Shape; PRESENTATION_SLOTS],
        serials: &[u64],
        presented: u64,
    ) {
        for (index, shape) in shapes.iter().enumerate() {
            let serial = serials[index];
            slots.gpu[index] = match shape {
                Shape::Idle => GpuSlot::Idle,
                Shape::Rendering => GpuSlot::Rendering { serial },
                Shape::Finished => GpuSlot::Ready {
                    serial,
                    result: Ok(picture(serial)),
                },
                Shape::Failed => GpuSlot::Ready {
                    serial,
                    result: Err(anyhow::anyhow!("an earlier frame failed")),
                },
            };
        }
        slots.last_presented = presented;
    }

    /// Whatever the slots held when the worker died, the caller must find out.
    ///
    /// The rule has no exceptions worth carving: if any slot was `Rendering`,
    /// the thread that would have finished it is gone, and a caller that is not
    /// told keeps painting its last good frame for ever. So this enumerates
    /// every census — each slot idle, rendering, holding a finished picture or
    /// holding an earlier failure, across every serial ordering and every
    /// position of the presented high-water mark — and asks the one question
    /// the panic path exists to answer.
    ///
    /// A search rather than cases because the two known holes here were both
    /// *orderings*, not shapes: an elected slot below the high-water mark, and
    /// a surviving picture above it. Neither is visible from a census drawn on
    /// a whiteboard, and there is no reason to think a third would be.
    #[test]
    fn a_dead_worker_is_reported_whatever_the_slots_were_holding() {
        let orderings = orderings();
        assert_eq!(orderings.len(), 120, "5! orderings of the serials");
        let mut censuses = 0usize;
        let mut with_nothing_in_flight = 0usize;

        for code in shape_codes() {
            let shapes = shapes_of(code);
            let in_flight = shapes.iter().any(|s| matches!(s, Shape::Rendering));
            for serials in &orderings {
                for presented in 0..=PRESENTATION_SLOTS as u64 + 1 {
                    let shared = shared();
                    install(
                        &mut shared.slots.lock().unwrap(),
                        &shapes,
                        serials,
                        presented,
                    );
                    censuses += 1;

                    // What the caller would have been handed had the worker
                    // not died: the highest-serial slot holding anything.
                    let untouched = shapes
                        .iter()
                        .enumerate()
                        .filter_map(|(index, shape)| match shape {
                            Shape::Finished => Some((serials[index], true)),
                            Shape::Failed => Some((serials[index], false)),
                            Shape::Idle | Shape::Rendering => None,
                        })
                        .max_by_key(|(serial, _)| *serial)
                        .map(|(_, ok)| ok);

                    assert!(!fail_in_flight(&shared, "device lost"));

                    let taken = shared.slots.lock().unwrap().take_latest();

                    // With nothing in flight there is no serial to carry a
                    // message on, so the panic path has nothing to report and
                    // must not destroy what is there instead. Discarding the
                    // last good picture would trade a misleading frame for a
                    // blank one and tell the caller no more than it already
                    // knew.
                    if !in_flight {
                        with_nothing_in_flight += 1;
                        let handed = taken.map(|result| result.is_ok());
                        assert_eq!(
                            handed, untouched,
                            "a panic with nothing in flight changed what the \
                             caller is handed: shapes {shapes:?}, serials \
                             {serials:?}, last_presented {presented}"
                        );
                        continue;
                    }

                    match taken {
                        Some(Err(_)) => {}
                        Some(Ok(_)) => panic!(
                            "the renderer died and the caller was handed a picture \
                             instead: shapes {shapes:?}, serials {serials:?}, \
                             last_presented {presented}"
                        ),
                        None => panic!(
                            "the renderer died silently: shapes {shapes:?}, \
                             serials {serials:?}, last_presented {presented}"
                        ),
                    }
                }
            }
        }

        assert!(censuses > 800_000, "the search was trimmed: {censuses}");
        assert!(
            with_nothing_in_flight > 100_000,
            "the zero-in-flight half of the space was not reached: \
             {with_nothing_in_flight}"
        );
    }

    /// The terminal announcement must outrank everything that survived the
    /// worker, in every census it could be made from.
    ///
    /// `Slots::announce` mints its serial by folding over the slots, and the
    /// question that fold answers is not obvious: `last_presented` alone is not
    /// the ceiling, because a finished picture can sit above it — `fail_in_flight`
    /// installs nothing when nothing was in flight, so it clears nothing either,
    /// and that picture is exactly what would swallow the announcement. So this
    /// enumerates the ceiling rather than arguing it: every census, every serial
    /// ordering, every high-water position, and every reservation pattern, run
    /// through the give-up path with a distinguishable reason.
    ///
    /// The reservation axis is here because `announce` writes into a slot, and
    /// the one thing no part of this module may ever write into is a slot whose
    /// surface is on screen.
    #[test]
    fn giving_up_outranks_whatever_survived_the_worker() {
        /// Reservation patterns, including the ones that leave `announce` the
        /// fewest slots to choose from.
        const RESERVATIONS: [[Option<usize>; RESERVED]; 4] = [
            [None, None, None],
            [Some(0), Some(1), Some(2)],
            [Some(4), Some(3), None],
            [Some(1), None, None],
        ];

        let orderings = orderings();
        let mut censuses = 0usize;
        for code in shape_codes() {
            let shapes = shapes_of(code);
            for serials in &orderings {
                for presented in [0, 3, PRESENTATION_SLOTS as u64 + 1] {
                    for reserved in RESERVATIONS {
                        // A reserved slot is always `Idle` in the real machine:
                        // `take_latest` clears every `Ready` slot as it
                        // reserves one, and `startable_slot` never hands a
                        // reserved slot out, so nothing can put it back into
                        // flight. `every_reachable_census_is_safe_live_and_self_consistent`
                        // is what pins that. Censuses that contradict it are
                        // unreachable, and asserting over them would be testing
                        // a machine this is not.
                        if reserved
                            .iter()
                            .flatten()
                            .any(|index| !matches!(shapes[*index], Shape::Idle))
                        {
                            continue;
                        }
                        let shared = shared();
                        let untouched = {
                            let mut slots = shared.slots.lock().unwrap();
                            install(&mut slots, &shapes, serials, presented);
                            slots.reserved = reserved;
                            // What the reserved slots hold before the panic
                            // path runs, to prove `announce` did not take one.
                            reserved
                                .iter()
                                .flatten()
                                .map(|index| discriminant(&slots.gpu[*index]))
                                .collect::<Vec<_>>()
                        };
                        censuses += 1;

                        assert!(!fail_in_flight(&shared, "in flight when it died"));
                        announce_terminal_failure(&shared, "gave up for good");

                        {
                            let slots = shared.slots.lock().unwrap();
                            let after = reserved
                                .iter()
                                .flatten()
                                .map(|index| discriminant(&slots.gpu[*index]))
                                .collect::<Vec<_>>();
                            assert_eq!(
                                after, untouched,
                                "the panic path wrote into a displayed slot: \
                                 shapes {shapes:?}, reserved {reserved:?}"
                            );
                        }

                        let taken = shared.slots.lock().unwrap().take_latest();
                        let message = match taken {
                            Some(Err(error)) => error.to_string(),
                            Some(Ok(_)) => panic!(
                                "a surviving picture outranked the terminal \
                                 failure: shapes {shapes:?}, serials {serials:?}, \
                                 last_presented {presented}, reserved {reserved:?}"
                            ),
                            None => panic!(
                                "giving up said nothing: shapes {shapes:?}, \
                                 serials {serials:?}, last_presented {presented}, \
                                 reserved {reserved:?}"
                            ),
                        };
                        assert!(
                            message.contains("gave up for good"),
                            "an older failure outranked the terminal one: \
                             {message} — shapes {shapes:?}, serials {serials:?}, \
                             last_presented {presented}, reserved {reserved:?}"
                        );
                    }
                }
            }
        }
        assert!(censuses > 400_000, "the search was trimmed: {censuses}");
    }

    #[test]
    fn a_panic_reason_survives_both_payload_shapes() {
        let str_payload: Box<dyn std::any::Any + Send> = Box::new("index out of bounds");
        let string_payload: Box<dyn std::any::Any + Send> = Box::new(String::from("device lost"));
        let odd_payload: Box<dyn std::any::Any + Send> = Box::new(7_u32);
        assert_eq!(panic_reason(&str_payload), "index out of bounds");
        assert_eq!(panic_reason(&string_payload), "device lost");
        assert!(panic_reason(&odd_payload).contains("non-string"));
    }

    /// A dead worker must hand its in-flight work back as an error. Leaving the
    /// slots `Rendering` is precisely the silent freeze: the caller waits on a
    /// frame that no thread is drawing any more.
    #[test]
    fn in_flight_work_is_failed_rather_than_abandoned() {
        let shared = shared();
        {
            let mut slots = shared.slots.lock().unwrap();
            slots.gpu[0] = GpuSlot::Rendering { serial: 7 };
            slots.gpu[2] = GpuSlot::Rendering { serial: 9 };
            slots.queued = Some(Queued {
                serial: 11,
                job: (),
            });
        }

        assert!(!fail_in_flight(&shared, "device lost"));

        let mut slots = shared.slots.lock().unwrap();
        assert!(
            slots.queued.is_none(),
            "the queued descriptor died with the thread"
        );
        let taken = slots.take_latest().expect("an error reached the caller");
        let message = match taken {
            Ok(_) => panic!("the presentation should be an error"),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("device lost"), "{message}");
        assert!(message.contains("reopened"), "{message}");
        assert!(
            slots.gpu.iter().all(|slot| matches!(slot, GpuSlot::Idle)),
            "no slot is left mid-flight"
        );
    }

    /// A dead worker whose in-flight serials are all older than the last
    /// presented frame still has to say so.
    ///
    /// `complete` drops a result at or below `last_presented`, which is right
    /// for a stale *picture* — presenting it would run time backwards — and
    /// wrong for a stale *error*, because the error is not about that frame. It
    /// is about the renderer, and the renderer is gone. Out-of-order completion
    /// makes this ordinary rather than exotic: begin two frames, complete and
    /// present the newer, and the older one is in flight with a serial below
    /// the boundary.
    ///
    /// Dropping it is the silent freeze `supervised_worker` exists to prevent —
    /// `take_latest` returns `None` for ever and the stage keeps painting its
    /// last good frame with nothing anywhere saying why.
    #[test]
    fn a_failure_older_than_the_presented_high_water_still_reaches_the_caller() {
        let shared = shared();
        {
            let mut slots = shared.slots.lock().unwrap();
            // The state two frames leave behind when the newer one completes
            // first and is presented: the older is still rendering, and the
            // boundary has already moved past it.
            slots.gpu[0] = GpuSlot::Rendering { serial: 21 };
            slots.last_presented = 22;
        }

        assert!(!fail_in_flight(&shared, "device lost"));

        let taken = shared.slots.lock().unwrap().take_latest();
        let message = match taken {
            Some(Err(error)) => error.to_string(),
            Some(Ok(_)) => panic!("the renderer died; there is no picture to show"),
            None => panic!("the renderer died silently: nothing reached the caller"),
        };
        assert!(message.contains("device lost"), "{message}");
    }

    /// `force_ready` admits a failure below the presentation boundary on
    /// purpose. Taking one must not drag the boundary down with it: `complete`
    /// reads that mark to reject stale completions, so lowering it would
    /// re-admit pictures this viewport has already moved past.
    #[test]
    fn taking_a_failure_below_the_boundary_does_not_lower_it() {
        let shared = shared();
        {
            let mut slots = shared.slots.lock().unwrap();
            slots.gpu[0] = GpuSlot::Rendering { serial: 21 };
            slots.last_presented = 22;
        }

        assert!(!fail_in_flight(&shared, "device lost"));

        let mut slots = shared.slots.lock().unwrap();
        assert!(
            matches!(slots.take_latest(), Some(Err(_))),
            "the failure still reaches the caller"
        );
        assert_eq!(
            slots.last_presented, 22,
            "the high-water mark only ever advances"
        );
    }

    /// The branch that runs exactly when the renderer is permanently broken,
    /// and so the one where silence costs most.
    ///
    /// A worker that dies on entry dies before it starts anything, so the
    /// permanently-broken renderer is the one least likely to leave a frame in
    /// flight for `fail_in_flight` to hang the news on. Nothing was in flight
    /// here at all, which is the honest shape of that case.
    #[test]
    fn giving_up_reports_the_failure_even_with_nothing_in_flight() {
        let shared = shared();
        let attempts = std::cell::Cell::new(0_usize);

        supervise(&shared, |_| {
            attempts.set(attempts.get() + 1);
            panic!("device lost");
        });

        assert_eq!(
            attempts.get(),
            4,
            "one initial run plus RESTARTS retries, then it gives up"
        );
        let taken = shared.slots.lock().unwrap().take_latest();
        let message = match taken {
            Some(Err(error)) => error.to_string(),
            Some(Ok(_)) => panic!("the renderer is gone; there is no picture to show"),
            None => panic!("the renderer gave up silently — the freeze with no explanation"),
        };
        assert!(message.contains("device lost"), "{message}");
        assert!(message.contains("reopened"), "{message}");
    }

    /// A worker that recovers must not have a failure announced over it: the
    /// terminal report belongs to giving up, not to any panic.
    #[test]
    fn a_worker_that_recovers_leaves_no_failure_behind() {
        let shared = shared();
        let attempts = std::cell::Cell::new(0_usize);

        supervise(&shared, |_| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                panic!("a transient fault");
            }
        });

        assert_eq!(attempts.get(), 2, "it panicked once and then ran clean");
        assert!(
            shared.slots.lock().unwrap().take_latest().is_none(),
            "a recovered renderer owes the caller nothing"
        );
    }

    /// Giving up must not resurrect a viewport the UI has already torn down.
    #[test]
    fn a_stopped_viewport_is_told_nothing_when_the_worker_gives_up() {
        let shared = shared();
        shared.slots.lock().unwrap().stopped = true;

        supervise(&shared, |_| panic!("device lost"));

        assert!(
            shared.slots.lock().unwrap().take_latest().is_none(),
            "nothing is reported into a stopped viewport"
        );
    }

    #[test]
    fn a_stopped_viewport_is_not_restarted() {
        let shared = shared();
        shared.slots.lock().unwrap().stopped = true;
        assert!(fail_in_flight(&shared, "device lost"));
    }
}

#[cfg(test)]
mod tests {
    use super::{GpuSlot, Slots, SubmitOutcome, PRESENTATION_SLOTS, RESERVED};

    /// The regression that cost this investigation five sections.
    ///
    /// A worker with work queued and no idle slot may recycle a completed
    /// frame, but never the newest one — that is the frame the next
    /// `take_latest` presents. Recycling it destroys a frame the GPU finished
    /// and leaves the slot reading `Rendering`, so the loss is invisible to
    /// every census field.
    ///
    /// At two usable slots this is the ordinary case, not a corner: one slot
    /// rendering, one just completed, a descriptor always waiting.
    #[test]
    fn the_newest_completed_frame_is_never_recycled_out_from_under_the_ui() {
        let mut slots = Slots::<u32, u32>::default();
        // Two reserved, exactly as a live viewport runs.
        slots.reserved = [Some(2), Some(3), Some(4)];

        // Slot 0 renders; slot 1 completes and is the newest thing there is.
        slots.submit(1, 10);
        let (rendering, _, _, _) = slots.begin_latest().expect("a free slot");
        slots.submit(2, 20);
        let (completing, serial, _, _) = slots.begin_latest().expect("the second free slot");
        assert!(
            slots.complete(completing, serial, 20).is_none(),
            "it is ready"
        );

        // The UI has not looked yet, and the worker has more work waiting.
        slots.submit(3, 30);
        assert_eq!(
            slots.startable_slot(),
            None,
            "the only recyclable slot holds the frame the UI is about to take"
        );

        // And the frame survives to be presented, which is the point.
        assert_eq!(slots.take_latest(), Some(20));
        assert!(
            slots.startable_slot().is_some(),
            "once taken, the slot is reusable and the worker is not wedged"
        );
        let _ = rendering;
    }

    /// The deadlock the recycling fallback exists to prevent must still not
    /// happen: an *older* completed frame is still fair game.
    #[test]
    fn an_older_completed_frame_is_still_recyclable() {
        let mut slots = Slots::<u32, u32>::default();
        slots.reserved = [None; RESERVED];
        // Every slot completed, so there is no idle one to prefer and the
        // fallback is the only thing under test.
        let mut ready = Vec::new();
        for serial in 1..=PRESENTATION_SLOTS as u64 {
            slots.submit(serial, serial as u32 * 10);
            let (slot, began, _, _) = slots.begin_latest().expect("a free slot");
            slots.complete(slot, began, began as u32 * 10);
            ready.push(slot);
        }
        slots.submit(99, 990);
        let startable = slots
            .startable_slot()
            .expect("an older frame may be recycled");
        assert_ne!(
            startable,
            *ready.last().expect("slots were filled"),
            "the newest completed frame is still protected"
        );
        assert_eq!(startable, ready[0], "the oldest is the one recycled");
    }

    #[test]
    fn bounded_slots_coalesce_queued_work_and_never_backpressure_submitter() {
        let mut slots = Slots::<u32, u32>::default();
        assert_eq!(slots.submit(1, 10).0, SubmitOutcome::Queued);
        assert_eq!(
            slots.submit(2, 20).0,
            SubmitOutcome::Replaced { dropped_serial: 1 }
        );
        let (slot, serial, job, displaced) = slots.begin_latest().unwrap();
        assert_eq!((slot, serial, job, displaced), (0, 2, 20, None));
        assert_eq!(
            slots
                .gpu
                .iter()
                .filter(|slot| matches!(slot, GpuSlot::Rendering { .. }))
                .count(),
            1
        );
        assert!(slots.queued.is_none());

        // Fill the rest, whatever the slot count is: one slot is already in
        // flight, so this leaves every slot rendering.
        for step in 1..PRESENTATION_SLOTS {
            let serial = step as u64 + 2;
            let job = serial as u32 * 10;
            slots.submit(serial, job);
            let (slot, began, queued, _) = slots
                .begin_latest()
                .expect("a free slot while the pipeline is filling");
            assert_eq!((slot, began, queued), (step, serial, job));
        }

        let last = PRESENTATION_SLOTS as u64 + 2;
        slots.submit(last, last as u32 * 10);
        assert!(
            slots.begin_latest().is_none(),
            "all GPU slots are in flight"
        );
        // The submitter is never blocked: a further submission displaces the
        // one still queued rather than waiting for a slot.
        assert_eq!(
            slots.submit(last + 1, 0).1,
            Some(last as u32 * 10),
            "the queued job is displaced, not the submitter blocked"
        );
        assert_eq!(slots.queued.as_ref().map(|job| job.serial), Some(last + 1));
    }

    /// The invariant a shared surface depends on: a frame that reached the
    /// screen, and the [`RESERVED`] - 1 before it, are not drawn over — because
    /// the window's command buffers may still be sampling them.
    #[test]
    fn presented_slots_stay_reserved_for_three_generations() {
        let mut slots = Slots::<u32, u32>::default();
        let present = |slots: &mut Slots<u32, u32>, serial: u32| {
            slots.submit(u64::from(serial), serial);
            let (slot, gpu_serial, job, _) = slots.begin_latest().expect("a slot was free");
            slots.complete(slot, gpu_serial, job);
            assert_eq!(slots.take_latest(), Some(serial));
            slot
        };

        let first = present(&mut slots, 1);
        assert_eq!(slots.reserved, [Some(first), None, None]);
        let second = present(&mut slots, 2);
        assert_ne!(second, first, "the displayed frame's slot was reused");
        assert_eq!(slots.reserved, [Some(second), Some(first), None]);

        let third = present(&mut slots, 3);
        assert!(third != first && third != second);
        assert_eq!(slots.reserved, [Some(third), Some(second), Some(first)]);

        let fourth = present(&mut slots, 4);
        assert!(
            fourth != first,
            "the oldest reserved slot was reused too soon"
        );
        // Now the first presented slot ages out: nothing on screen and nothing
        // already encoded still refers to it.
        assert_eq!(slots.reserved, [Some(fourth), Some(third), Some(second)]);
        let fifth = present(&mut slots, 5);
        assert_eq!(fifth, first);
    }

    /// The worker parks when it cannot start work and spins when it can, so
    /// the two questions must have one answer. They did not once, and the
    /// symptom was a wedged UI rather than a wrong picture.
    #[test]
    fn the_startable_predicate_agrees_with_actually_starting() {
        let mut slots = Slots::<u32, u32>::default();
        assert!(!slots.can_begin(), "nothing is queued");

        for serial in 1..=PRESENTATION_SLOTS as u32 {
            slots.submit(u64::from(serial), serial);
            assert!(slots.can_begin());
            assert!(slots.begin_latest().is_some());
        }
        slots.submit(99, 99);
        assert!(!slots.can_begin(), "every slot is rendering");
        assert!(slots.begin_latest().is_none());

        // One completion does NOT free a slot while it is the newest frame the
        // UI has not taken — recycling it is what destroyed 15 finished frames
        // across a 163 ms freeze. Both questions must still agree, which is
        // what this test is actually for.
        slots.complete(0, 1, 1);
        assert!(
            !slots.can_begin(),
            "the only completed frame is the one the UI is about to present"
        );
        assert!(slots.begin_latest().is_none());

        // Taking it does not immediately reopen the worker either: the slot it
        // freed becomes the reservation, and every other slot is still
        // rendering. Whatever the answer, the predicate and the act must agree
        // — which is the invariant this test exists for.
        assert_eq!(slots.take_latest(), Some(1));
        assert_eq!(slots.can_begin(), slots.startable_slot().is_some());
        assert!(!slots.can_begin(), "the freed slot is now the reservation");
        assert!(slots.begin_latest().is_none());

        // Reserving the only recyclable slot must close the predicate too.
        let mut reserved = Slots::<u32, u32>::default();
        reserved.submit(1, 1);
        let (slot, serial, job, _) = reserved.begin_latest().unwrap();
        reserved.complete(slot, serial, job);
        reserved.take_latest();
        for other in 0..PRESENTATION_SLOTS {
            if other != slot {
                reserved.gpu[other] = GpuSlot::Rendering { serial: 50 };
            }
        }
        reserved.submit(2, 2);
        assert_eq!(reserved.reserved[0], Some(slot));
        assert!(
            !reserved.can_begin(),
            "the presented slot is not somewhere to start work"
        );
        assert!(reserved.begin_latest().is_none());
    }

    /// Every reachable slot census, checked against the three properties the
    /// recycling rule has to hold simultaneously.
    ///
    /// The recycling bug was a *phasing* bug: every individual step was
    /// defensible and the loop they formed was not. Hand-written interleavings
    /// pin the ones someone thought of, so this enumerates the rest — a
    /// breadth-first search over every order of submit / begin / complete /
    /// take, with serials collapsed to their rank so the space is finite.
    ///
    /// The properties, in the order they would bite:
    ///
    /// - **Safety.** A slot the UI is displaying is never chosen to draw into.
    ///   This is the invariant a shared `IOSurface` has instead of a copy.
    /// - **Progress.** Whenever work is queued and nothing is startable, some
    ///   slot must be `Rendering` — that completion is the only event that can
    ///   reopen the worker, and without one the worker parks for ever holding a
    ///   descriptor. Protecting the newest frame narrows the startable set, so
    ///   this is precisely the property the fix could have broken.
    /// - **Agreement.** `can_begin` and `begin_latest` answer the same question,
    ///   or the worker spins on a queue it will not be allowed to start.
    #[test]
    fn every_reachable_census_is_safe_live_and_self_consistent() {
        use std::collections::{HashSet, VecDeque};

        /// Slot states with serials replaced by their rank, which is all the
        /// rule reads them for.
        type Key = (
            [(u8, usize); PRESENTATION_SLOTS],
            usize,
            usize,
            [Option<usize>; RESERVED],
        );

        fn key(slots: &Slots<u64, u64>) -> Key {
            let mut serials: Vec<u64> = slots
                .gpu
                .iter()
                .filter_map(|slot| match slot {
                    GpuSlot::Idle => None,
                    GpuSlot::Rendering { serial } | GpuSlot::Ready { serial, .. } => Some(*serial),
                })
                .chain(slots.queued.iter().map(|queued| queued.serial))
                .chain(std::iter::once(slots.last_presented))
                .collect();
            serials.sort_unstable();
            serials.dedup();
            let rank = |serial: u64| {
                serials
                    .binary_search(&serial)
                    .expect("serial was collected")
            };
            let gpu = std::array::from_fn(|index| match &slots.gpu[index] {
                GpuSlot::Idle => (0, 0),
                GpuSlot::Rendering { serial } => (1, rank(*serial)),
                GpuSlot::Ready { serial, .. } => (2, rank(*serial)),
            });
            let queued = slots
                .queued
                .as_ref()
                .map_or(usize::MAX, |queued| rank(queued.serial));
            (gpu, queued, rank(slots.last_presented), slots.reserved)
        }

        /// A state is carried as the action list that reaches it, so each node
        /// can be replayed into a fresh `Slots` — cheaper than making `Slots`
        /// clonable for a test.
        #[derive(Clone, Copy, Debug)]
        enum Step {
            Submit,
            Begin,
            Complete(usize),
            Take,
        }

        fn replay(history: &[Step]) -> Slots<u64, u64> {
            let mut slots = Slots::<u64, u64>::default();
            let mut next = 0u64;
            for step in history {
                match step {
                    Step::Submit => {
                        next += 1;
                        slots.submit(next, next);
                    }
                    Step::Begin => {
                        slots.begin_latest();
                    }
                    Step::Complete(slot) => {
                        if let GpuSlot::Rendering { serial } = slots.gpu[*slot] {
                            slots.complete(*slot, serial, serial);
                        }
                    }
                    Step::Take => {
                        slots.take_latest();
                    }
                }
            }
            slots
        }

        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(Vec::<Step>::new());
        seen.insert(key(&replay(&[])));
        // The two censuses the rule is actually about. Asserting they were
        // reached is what stops this from passing vacuously if the frontier
        // ever stops generating the interesting half of the space.
        let mut saw_blocked_on_newest = false;
        let mut saw_older_recycled = false;

        while let Some(history) = queue.pop_front() {
            let slots = replay(&history);

            // Safety: the frame on screen is never the frame being drawn.
            if let Some(startable) = slots.startable_slot() {
                assert!(
                    !slots.reserved.contains(&Some(startable)),
                    "a displayed slot was offered for drawing: {history:?}"
                );
            }
            for (index, slot) in slots.gpu.iter().enumerate() {
                assert!(
                    !(slots.reserved.contains(&Some(index))
                        && matches!(slot, GpuSlot::Rendering { .. })),
                    "slot {index} is displayed and rendering at once: {history:?}"
                );
            }

            // Progress: if nothing can start, something must be able to finish.
            if slots.queued.is_some() && slots.startable_slot().is_none() {
                assert!(
                    slots
                        .gpu
                        .iter()
                        .any(|slot| matches!(slot, GpuSlot::Rendering { .. })),
                    "the worker is wedged with work queued and no completion \
                     outstanding: {history:?}"
                );
            }

            let ready = |slot: &GpuSlot<u64>| match slot {
                GpuSlot::Ready { serial, .. } => Some(*serial),
                GpuSlot::Idle | GpuSlot::Rendering { .. } => None,
            };
            let completed = slots.gpu.iter().filter_map(ready).count();
            match slots.startable_slot() {
                None if completed == 1 && slots.queued.is_some() => {
                    saw_blocked_on_newest = true;
                }
                Some(startable) if completed > 1 => {
                    let newest = slots.gpu.iter().filter_map(ready).max();
                    assert_ne!(
                        ready(&slots.gpu[startable]),
                        newest,
                        "the newest completed frame was offered for recycling: {history:?}"
                    );
                    saw_older_recycled |= ready(&slots.gpu[startable]).is_some();
                }
                _ => {}
            }

            // Agreement: the predicate the worker parks on and the act.
            assert_eq!(
                slots.can_begin(),
                slots.queued.is_some() && slots.startable_slot().is_some(),
                "can_begin disagrees with startable_slot: {history:?}"
            );

            // Depth is what makes this terminate at all; the frontier stops
            // producing new keys well before it.
            if history.len() >= 22 {
                continue;
            }
            let mut steps = vec![Step::Submit, Step::Begin, Step::Take];
            steps.extend((0..PRESENTATION_SLOTS).map(Step::Complete));
            for step in steps {
                let mut next = history.clone();
                next.push(step);
                if seen.insert(key(&replay(&next))) {
                    queue.push_back(next);
                }
            }
        }

        assert!(
            saw_blocked_on_newest,
            "the search never reached the census the fix is about"
        );
        assert!(
            saw_older_recycled,
            "the search never reached the census the anti-deadlock fallback is \
             about"
        );
    }

    #[test]
    fn newest_ready_frame_wins_and_releases_older_results() {
        // Filled by assignment rather than by an array literal so the slot
        // count can change without editing this test — which is exactly what
        // it did.
        let mut slots = Slots::<(), &'static str>::default();
        slots.gpu[0] = GpuSlot::Ready {
            serial: 7,
            result: "old",
        };
        slots.gpu[1] = GpuSlot::Ready {
            serial: 9,
            result: "new",
        };
        assert_eq!(slots.take_latest(), Some("new"));
        assert!(slots.gpu.iter().all(|slot| matches!(slot, GpuSlot::Idle)));
    }

    #[test]
    fn completion_older_than_the_presented_high_water_is_discarded() {
        let mut slots = Slots::<(), Result<&'static str, &'static str>>::default();
        slots.gpu[0] = GpuSlot::Rendering { serial: 8 };
        slots.gpu[1] = GpuSlot::Ready {
            serial: 9,
            result: Ok("new"),
        };

        assert_eq!(slots.take_latest(), Some(Ok("new")));
        assert_eq!(slots.last_presented, 9);
        assert_eq!(slots.complete(0, 8, Ok("stale")), Some(Ok("stale")));
        assert_eq!(slots.take_latest(), None);

        slots.gpu[0] = GpuSlot::Rendering { serial: 7 };
        assert_eq!(
            slots.complete(0, 7, Err("stale error")),
            Some(Err("stale error"))
        );
        assert_eq!(slots.take_latest(), None);
        assert!(matches!(slots.gpu[0], GpuSlot::Idle));
    }
}

#[cfg(test)]
// The window holds intervals verbatim, so the assertions below compare exactly
// the values that were stored. A tolerance would only blur the boundary the
// test is checking.
#[allow(clippy::float_cmp)]
mod pacing {
    use super::{Pacing, PACING_WINDOW};

    /// The first delivery has nothing to be spaced from, and saying "0 ms"
    /// there is not a harmless default — it is a perfect frame in the
    /// distribution that no one rendered.
    #[test]
    fn the_first_frame_has_no_interval_and_does_not_enter_the_distribution() {
        let mut pacing = Pacing::default();
        assert!(pacing.record().is_none());
        assert_eq!(pacing.delivered(), 1);
        assert!(
            pacing.summary().is_none(),
            "one frame is no interval at all"
        );
        assert!(pacing.record().is_some());
        assert!(pacing.summary().is_some());
    }

    /// The window is what makes this describe now rather than since-launch.
    #[test]
    fn the_window_forgets_intervals_older_than_its_length() {
        // A single catastrophic frame, then a long clean run that displaces it.
        let mut recent = [0.0; PACING_WINDOW];
        recent[0] = 900.0;
        let mut pacing = Pacing {
            previous: Some(std::time::Instant::now()),
            recent,
            next: 1,
            len: 1,
            ..Pacing::default()
        };
        assert_eq!(pacing.summary().expect("one interval").max_ms, 900.0);
        for _ in 0..PACING_WINDOW {
            pacing.record();
        }
        let summary = pacing.summary().expect("a full window");
        assert!(
            summary.max_ms < 900.0,
            "the stale hitch is still being reported: {summary:?}"
        );
        assert_eq!(pacing.delivered(), PACING_WINDOW as u64);
    }
}
