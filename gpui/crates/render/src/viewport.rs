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
//! [`Presentation`]. Live UI uses [`AsyncViewport`]: three bounded slots around
//! a renderer-owned thread, with nonblocking submit/take operations. The bytes
//! still cross the CPU until the `IOSurface` handoff lands, but device polling
//! and mapping never occur on the UI thread.
//!
//! Deliberately *not* here: frame pacing, resize debouncing and input. Those
//! are the compositor's, because only it knows when a repaint is wanted — this
//! crate has no windowing and gains none.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::frame::Frame;
use crate::gpu::{Channels, FrameTimings, PendingReadback, Renderer};

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
    /// `width * height` BGRA8 texels.
    pub pixels: Arc<[u8]>,
    /// End-to-end time spent on the renderer thread.
    pub draw_time: Duration,
    /// Independent CPU encode/submit and GPU pass timings when the adapter
    /// supports timestamp queries. Unsupported adapters still present frames.
    pub timings: Option<FrameTimings>,
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
/// Exactly three GPU output/readback slots bound in-flight work. The UI only
/// calls [`Self::submit`] and [`Self::take_latest`], both short mutex operations;
/// device creation, queue submission, `map_async` polling and retirement all
/// live on the owned renderer thread. One additional descriptor is coalesced
/// newest-wins while all GPU slots are occupied, rather than back-pressuring
/// the UI thread.
pub struct AsyncViewport {
    shared: Arc<Shared<FrameRequest, anyhow::Result<AsyncPresentation>>>,
    next_serial: u64,
    subframes: u32,
}

struct FrameRequest {
    frame: Frame,
    width: u32,
    height: u32,
    subframes: u32,
}

struct Shared<J, R> {
    slots: Mutex<Slots<J, R>>,
    work: Condvar,
}

struct Slots<J, R> {
    gpu: [GpuSlot<R>; 3],
    queued: Option<Queued<J>>,
    /// Greatest serial handed to the presentation caller. GPU maps may retire
    /// out of order, so no later completion at or below this boundary may
    /// become Ready — including an error from an obsolete frame.
    last_presented: u64,
    stopped: bool,
}

struct Queued<J> {
    serial: u64,
    job: J,
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

    /// Bind the newest descriptor to an idle GPU resource. If the UI has not
    /// consumed completed frames, the oldest ready resource is recyclable.
    fn begin_latest(&mut self) -> Option<(usize, u64, J, Option<R>)> {
        let slot = self
            .gpu
            .iter()
            .position(|slot| matches!(slot, GpuSlot::Idle))
            .or_else(|| {
                self.gpu
                    .iter()
                    .enumerate()
                    .filter_map(|(index, slot)| match slot {
                        GpuSlot::Ready { serial, .. } => Some((index, *serial)),
                        GpuSlot::Idle | GpuSlot::Rendering { .. } => None,
                    })
                    .min_by_key(|(_, serial)| *serial)
                    .map(|(index, _)| index)
            })?;
        let Queued { serial, job } = self.queued.take()?;
        let old = std::mem::replace(&mut self.gpu[slot], GpuSlot::Rendering { serial });
        let displaced = match old {
            GpuSlot::Ready { result, .. } => Some(result),
            GpuSlot::Idle => None,
            GpuSlot::Rendering { .. } => unreachable!("selected a busy GPU slot"),
        };
        Some((slot, serial, job, displaced))
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
        self.last_presented = newest.1;
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
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("luma-render".into())
            .spawn(move || render_worker(&worker))
            .expect("renderer worker thread must start");
        Self {
            shared,
            next_serial: 0,
            subframes: LIVE_SUBFRAMES,
        }
    }

    /// Trade image quality for frame time. Applied to subsequently submitted
    /// frames and clamped to at least one sample.
    pub fn set_subframes(&mut self, subframes: u32) {
        self.subframes = subframes.max(1);
    }

    /// Queue one live frame without waiting for the renderer or GPU.
    pub fn submit(&mut self, frame: Frame, width: u32, height: u32) -> SubmitOutcome {
        self.next_serial = self.next_serial.wrapping_add(1);
        let mut slots = self
            .shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (outcome, displaced) = slots.submit(
            self.next_serial,
            FrameRequest {
                frame,
                width: width.max(1),
                height: height.max(1),
                subframes: self.subframes,
            },
        );
        drop(slots);
        drop(displaced);
        self.shared.work.notify_one();
        outcome
    }

    /// Take the newest completed frame, if one exists, without polling or
    /// waiting for the GPU.
    #[must_use]
    pub fn take_latest(&self) -> Option<anyhow::Result<AsyncPresentation>> {
        self.shared
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take_latest()
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

fn render_worker(shared: &Shared<FrameRequest, anyhow::Result<AsyncPresentation>>) {
    // The lab should expose honest GPU time where the adapter can provide it,
    // without making timestamp support a requirement for opening a viewport.
    let mut renderer = Renderer::new_profiled()
        .or_else(|_| Renderer::new())
        .map_err(|error| error.to_string());
    let mut pending: [Option<(u64, PendingReadback)>; 3] = std::array::from_fn(|_| None);
    // Timestamp queries use a single ordered measurement stream. Ordinary
    // frames continue to occupy all three presentation slots around it.
    let mut profile_slot = None;
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
            match &mut renderer {
                Ok(renderer) => {
                    let readback = renderer.submit_live(
                        &request.frame,
                        request.width,
                        request.height,
                        request.subframes,
                        slot,
                        profile_slot.is_none(),
                    );
                    if profile_slot.is_none() {
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
                    retire(
                        shared,
                        slot,
                        serial,
                        Ok(AsyncPresentation {
                            serial,
                            width: frame.width,
                            height: frame.height,
                            pixels: Arc::from(frame.pixels),
                            draw_time: frame.draw_time,
                            timings: frame.profile,
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
        let has_free_gpu = slots
            .gpu
            .iter()
            .any(|slot| !matches!(slot, GpuSlot::Rendering { .. }));
        if slots.queued.is_some() && has_free_gpu {
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
mod tests {
    use super::{GpuSlot, Slots, SubmitOutcome};

    #[test]
    fn triple_slots_coalesce_queued_work_and_never_backpressure_submitter() {
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

        slots.submit(3, 30);
        let (slot, serial, job, _) = slots.begin_latest().unwrap();
        assert_eq!((slot, serial, job), (1, 3, 30));
        slots.submit(4, 40);
        let (slot, serial, job, _) = slots.begin_latest().unwrap();
        assert_eq!((slot, serial, job), (2, 4, 40));
        slots.submit(5, 50);
        assert!(
            slots.begin_latest().is_none(),
            "all GPU slots are in flight"
        );
        assert_eq!(slots.submit(6, 60).1, Some(50));
        assert_eq!(slots.queued.as_ref().map(|job| job.serial), Some(6));
    }

    #[test]
    fn newest_ready_frame_wins_and_releases_older_results() {
        let mut slots = Slots::<(), &'static str> {
            gpu: [
                GpuSlot::Ready {
                    serial: 7,
                    result: "old",
                },
                GpuSlot::Ready {
                    serial: 9,
                    result: "new",
                },
                GpuSlot::Idle,
            ],
            ..Default::default()
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
