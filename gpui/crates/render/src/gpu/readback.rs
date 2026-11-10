//! A submitted frame until its pixels and timestamps come back: the pending
//! handle, completion polling, and the deferred timestamp resolve.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::light_index::LightIndexStats;
use crate::pass_profile::Base;
use crate::viewport::{Presented, PRESENTATION_SLOTS};

use super::stats::{CpuSpans, FrameTimings, ShadowStats};

pub(super) struct ProfilerResources {
    /// One set of timestamp resources per presentation slot.
    ///
    /// Sized by [`PRESENTATION_SLOTS`] and not by a literal: `submit_readback`
    /// asserts a slot is in range against that constant and then indexes this
    /// array with it, so a second, smaller answer to "how many slots are there"
    /// turns the assert into a lie and the index into a panic on the renderer
    /// thread — which presents as the stage silently freezing.
    pub(super) slots: [ProfilerSlot; PRESENTATION_SLOTS],
    pub(super) timestamp_period_ns: f32,
}

pub(super) struct ProfilerSlot {
    pub(super) query_set: wgpu::QuerySet,
    pub(super) resolve: wgpu::Buffer,
    pub(super) readback: wgpu::Buffer,
}

/// How one submission's pixels reach its caller, resolved when the frame is
/// encoded so the rest of encoding never asks again.
pub(super) enum Finish {
    /// Copy the texture into the buffer, then map the buffer.
    Copy(wgpu::Texture, wgpu::Buffer),
    /// Hand back the surface that was written directly.
    Share(crate::share::Surface),
}

/// A submitted frame that has not finished.
///
/// Both destinations wait for the GPU; they differ in what arrives when it is
/// done. A staged frame's bytes arrive with its buffer map; a shared frame's
/// pixels were never anywhere else, so only the completion signal is awaited.
pub(crate) struct PendingFrame {
    pub(super) completion: Completion,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) started: Instant,
    pub(super) profile: Option<PendingProfile>,
    /// What the fixture-shadow passes submitted for this frame.
    ///
    /// Carried with the frame rather than read off the renderer later: by the
    /// time a frame completes, the renderer has moved on and its counter
    /// describes some other frame. Unlike `profile` this is always present —
    /// it is counted while encoding, not measured by the adapter.
    pub(super) shadows: ShadowStats,
    /// The clustered-light index this frame shaded against.
    ///
    /// Carried with the frame for the same reason as `shadows`: the renderer
    /// has moved on by the time a frame completes, and its live counter then
    /// describes a different frame.
    pub(super) clusters: LightIndexStats,
    /// How long the request waited before this frame was started.
    pub(super) queued: Duration,
    /// Where the CPU time from claiming this slot to submitting went. Always
    /// present, unlike `profile` — see [`CpuSpans`].
    pub(super) cpu: CpuSpans,
    /// When the driver said this frame was done, if it has.
    pub(super) signalled: Option<Instant>,
}

pub(super) enum Completion {
    Staged {
        readback: wgpu::Buffer,
        mapped: mpsc::Receiver<(Instant, Result<(), String>)>,
        mapped_result: Option<Result<(), String>>,
        bytes_per_row: u32,
    },
    Shared {
        surface: crate::share::Surface,
        done: mpsc::Receiver<Instant>,
        finished: bool,
    },
}

/// The frame's timestamps, resolved once the frame itself has finished.
///
/// `resolve_query_set` runs on the blit engine concurrently with the render
/// stream, so a resolve encoded into the frame's own command buffer reads the
/// query set before the GPU has finished writing it — dropping the tail
/// samples, deterministically, at some viewport sizes and not others. Nothing
/// orderable *inside* the command buffer fixes that: not a trailing render
/// pass, not sequencing the resolve behind a copy of the frame's own output.
/// So the resolve waits for the frame's completion signal — the image map on
/// the readback path, the queue callback on the shared-surface one — and goes
/// in a command buffer of its own. Both signals already exist, which is what
/// keeps this off the critical path: registering a second one costs a
/// queue-wide drain per frame and halves live throughput.
pub(super) struct PendingProfile {
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    pub(super) query_set: wgpu::QuerySet,
    pub(super) resolve: wgpu::Buffer,
    pub(super) readback: wgpu::Buffer,
    /// `None` until the frame completed and the resolve was submitted.
    pub(super) mapped: Option<mpsc::Receiver<Result<(), String>>>,
    pub(super) mapped_result: Option<Result<(), String>>,
    pub(super) timestamp_period_ns: f32,
    pub(super) cpu_encode_submit: Duration,
    pub(super) cpu_cluster: Duration,
    pub(super) strict_timestamps: bool,
    pub(super) grid_fog: bool,
    pub(super) query_count: u32,
    pub(super) passes: Vec<(&'static str, u32, u32)>,
    /// `PassQueries::moved`.
    pub(super) moved: Vec<(u32, u32)>,
}

impl PendingProfile {
    /// Submit the resolve, once, after the frame it belongs to has finished.
    fn resolve_after_frame(&mut self) {
        if self.mapped.is_some() {
            return;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luma-profile-resolve"),
            });
        encoder.resolve_query_set(&self.query_set, 0..self.query_count, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(
            &self.resolve,
            0,
            &self.readback,
            0,
            u64::from(self.query_count) * 8,
        );
        self.queue.submit([encoder.finish()]);
        let (mapped_tx, mapped) = mpsc::sync_channel(1);
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = mapped_tx.send(result.map_err(|error| error.to_string()));
            });
        self.mapped = Some(mapped);
    }
}

pub(crate) struct CompletedFrame {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) image: Presented,
    pub(crate) draw_time: Duration,
    pub(crate) profile: Option<FrameTimings>,
    pub(crate) shadows: ShadowStats,
    pub(crate) clusters: LightIndexStats,
    pub(crate) queued: Duration,
    /// Where the CPU time before this frame reached Metal went.
    pub(crate) cpu: CpuSpans,
    /// Submit to the driver's completion callback — the GPU's own share of
    /// `draw_time`, including any wait to begin executing.
    pub(crate) until_signalled: Option<Duration>,
    /// Completion callback to this poll observing it — the worker's share.
    /// A large value here means nobody was looking, not that anything was slow.
    pub(crate) until_noticed: Option<Duration>,
}

impl Completion {
    /// Whether the GPU is finished with this frame, and when it said so.
    ///
    /// `signalled` is the instant the driver's callback fired, not the instant
    /// this poll noticed it. The difference between the two is the only thing
    /// that separates "the GPU took that long" from "nobody was looking".
    fn ready(&mut self, signalled: &mut Option<Instant>) -> anyhow::Result<bool> {
        match self {
            Self::Staged {
                mapped,
                mapped_result,
                ..
            } => {
                if mapped_result.is_none() {
                    *mapped_result = match mapped.try_recv() {
                        Ok((at, result)) => {
                            *signalled = Some(at);
                            Some(result)
                        }
                        Err(mpsc::TryRecvError::Empty) => None,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            return Err(anyhow::anyhow!("GPU map callback disconnected"));
                        }
                    };
                }
                Ok(mapped_result.is_some())
            }
            Self::Shared { done, finished, .. } => {
                if !*finished {
                    *finished = match done.try_recv() {
                        Ok(at) => {
                            *signalled = Some(at);
                            true
                        }
                        Err(mpsc::TryRecvError::Empty) => false,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            return Err(anyhow::anyhow!("GPU completion callback disconnected"));
                        }
                    };
                }
                Ok(*finished)
            }
        }
    }

    /// The finished pixels.
    ///
    /// # Panics
    /// Panics unless [`Self::ready`] has returned `true`.
    fn image(&mut self, width: u32, height: u32) -> anyhow::Result<Presented> {
        match self {
            Self::Staged {
                readback,
                mapped_result,
                bytes_per_row,
                ..
            } => {
                mapped_result
                    .take()
                    .expect("mapped result checked above")
                    .map_err(anyhow::Error::msg)?;
                let view = readback
                    .slice(..)
                    .get_mapped_range()
                    .map_err(anyhow::Error::msg)?;
                let row_bytes = (width * 4) as usize;
                let mut pixels = Vec::with_capacity(row_bytes * height as usize);
                if *bytes_per_row as usize == row_bytes {
                    pixels.extend_from_slice(&view);
                } else {
                    for row in 0..height {
                        let start = (row * *bytes_per_row) as usize;
                        pixels.extend_from_slice(&view[start..start + row_bytes]);
                    }
                }
                drop(view);
                readback.unmap();
                Ok(Presented::Pixels(pixels))
            }
            // Already where it belongs; the handle is all that moves.
            Self::Shared { surface, .. } => Ok(Presented::Shared(surface.clone())),
        }
    }
}

impl PendingFrame {
    /// Wait for this frame and hand back its pixels.
    ///
    /// # Why this polls rather than blocking
    ///
    /// One `PollType::Wait` was enough while every renderer owned a private
    /// device: the device went idle exactly when this frame finished, and the
    /// map callback had been serviced by the time the poll returned. Neither
    /// half survives sharing the device. The wait is satisfied by "no
    /// submissions in flight as of the beginning of the call", which another
    /// thread's traffic can satisfy while this buffer's mapping is left to a
    /// maintain pass we then skip — so the wait returns with no pixels.
    ///
    /// Worse, a *blocking* wait here holds the device against everyone else.
    /// A caller rendering a few hundred offline frames in a loop, each with
    /// its own blocking wait, starves the live workers sharing the device:
    /// their map callbacks stop being serviced and their frames never arrive.
    /// That reads as a hang, not as the slowdown it sounds like.
    ///
    /// So this waits on the only thing that actually answers the question —
    /// the readback itself — using the same non-blocking maintain and 1 ms
    /// cadence the live worker uses, and blocks nobody.
    ///
    /// # Errors
    /// Propagates a device poll or mapping failure, and gives up rather than
    /// spinning for ever on a device that has stopped returning frames.
    pub(crate) fn complete_blocking(
        &mut self,
        device: &wgpu::Device,
    ) -> anyhow::Result<CompletedFrame> {
        /// A frame that has not landed in this long is a dead device, not a
        /// slow one. Generous because a debug build under a loaded machine is
        /// genuinely slow, and a false positive here would look like a driver
        /// bug.
        const DEADLINE: Duration = Duration::from_secs(30);
        let deadline = Instant::now() + DEADLINE;
        loop {
            device.poll(wgpu::PollType::Poll)?;
            if let Some(frame) = self.try_complete()? {
                return Ok(frame);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "the GPU did not return a frame within {DEADLINE:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(crate) fn try_complete(&mut self) -> anyhow::Result<Option<CompletedFrame>> {
        let ready = self.completion.ready(&mut self.signalled)?;
        if let Some(profile) = &mut self.profile {
            if ready {
                profile.resolve_after_frame();
            }
            if let (None, Some(mapped)) = (&profile.mapped_result, &profile.mapped) {
                profile.mapped_result = match mapped.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(anyhow::anyhow!("GPU timestamp map callback disconnected"));
                    }
                };
            }
        }
        if !ready
            || self
                .profile
                .as_ref()
                .is_some_and(|profile| profile.mapped_result.is_none())
        {
            return Ok(None);
        }
        let profile = if let Some(profile) = &mut self.profile {
            profile
                .mapped_result
                .take()
                .expect("profile map result checked above")
                .map_err(anyhow::Error::msg)?;
            let view = profile
                .readback
                .slice(..)
                .get_mapped_range()
                .map_err(anyhow::Error::msg)?;
            let mut timestamps = vec![0_u64; profile.query_count as usize];
            for (timestamp, bytes) in timestamps.iter_mut().zip(view.chunks_exact(8)) {
                *timestamp =
                    u64::from_ne_bytes(bytes.try_into().expect("timestamp is eight bytes"));
            }
            drop(view);
            profile.readback.unmap();
            for &(base, sample) in &profile.moved {
                timestamps[base as usize] = timestamps[sample as usize];
            }
            // Consecutive end samples partition the frame, per `QUERY_COUNT`.
            // A zero haze end means haze did not run; a whole-frame zero means
            // the driver dropped the samples.
            let [start, scene_end, haze_end, composite_end, index0, index1, grid0, grid1, prepared, lit] =
                [
                    Base::Start,
                    Base::SceneEnd,
                    Base::HazeEnd,
                    Base::CompositeEnd,
                    Base::LightIndexStart,
                    Base::LightIndexEnd,
                    Base::GridStart,
                    Base::GridEnd,
                    Base::GridPrepared,
                    Base::GridLit,
                ]
                .map(|sample| timestamps[sample as usize]);
            let haze_ran = haze_end > 0;
            // The composite pass samples both of its predecessors' outputs, so
            // it is the frame's sink and its completion is the frame's end.
            // The scene and haze passes have no such dependency on each other
            // and do overlap, either order — hence saturating cuts below.
            let timestamps_valid = start > 0
                && scene_end >= start
                && composite_end >= scene_end
                && (!haze_ran || (haze_end >= start && composite_end >= haze_end))
                && index0 > 0
                && index1 >= index0
                && (!profile.grid_fog
                    || (grid0 >= start && grid1 >= grid0 && composite_end >= grid1));
            if !timestamps_valid {
                anyhow::ensure!(
                    !profile.strict_timestamps,
                    "inconsistent GPU pass timestamps: {timestamps:?}"
                );
                None
            } else {
                let milliseconds = f64::from(profile.timestamp_period_ns) / 1_000_000.0;
                let span = |begin: u64, end: u64| end.saturating_sub(begin) as f64 * milliseconds;
                let origin = timestamps
                    .iter()
                    .copied()
                    .filter(|t| *t > 0)
                    .min()
                    .unwrap_or(start);
                let passes = profile
                    .passes
                    .iter()
                    .map(|&(name, begin, end)| {
                        let (begin, end) = (timestamps[begin as usize], timestamps[end as usize]);
                        anyhow::ensure!(
                            begin > 0 && end >= begin,
                            "invalid {name} timestamps: {begin}..{end}"
                        );
                        Ok(crate::pass_profile::GpuPassTiming {
                            name,
                            start_ms: span(origin, begin),
                            end_ms: span(origin, end),
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Some(FrameTimings {
                    passes,
                    cpu: self.cpu,
                    gpu_total_ms: span(start, composite_end),
                    gpu_volumetric_ms: if haze_ran {
                        span(scene_end, haze_end)
                    } else {
                        0.0
                    },
                    gpu_scene_ms: span(start, scene_end),
                    gpu_composite_ms: span(scene_end.max(haze_end), composite_end),
                    gpu_index_ms: span(index0, index1),
                    gpu_fog_grid_ms: if profile.grid_fog {
                        span(grid0, grid1)
                    } else {
                        0.0
                    },
                    gpu_fog_prepare_ms: if profile.grid_fog {
                        span(grid0, prepared)
                    } else {
                        0.0
                    },
                    gpu_fog_light_ms: if profile.grid_fog {
                        span(prepared, lit)
                    } else {
                        0.0
                    },
                    gpu_fog_integrate_ms: if profile.grid_fog {
                        span(lit, grid1)
                    } else {
                        0.0
                    },
                    cpu_encode_submit_ms: profile.cpu_encode_submit.as_secs_f64() * 1000.0,
                    cpu_cluster_ms: profile.cpu_cluster.as_secs_f64() * 1000.0,
                })
            }
        } else {
            None
        };
        let image = self.completion.image(self.width, self.height)?;
        Ok(Some(CompletedFrame {
            width: self.width,
            height: self.height,
            image,
            draw_time: self.started.elapsed(),
            profile,
            shadows: self.shadows,
            clusters: self.clusters,
            queued: self.queued,
            cpu: self.cpu,
            until_signalled: self
                .signalled
                .map(|at| at.saturating_duration_since(self.started)),
            until_noticed: self.signalled.map(|at| at.elapsed()),
        }))
    }
}

/// Claim the frame's start timestamp for the pass being encoded.
///
/// The first pass to ask gets it and later ones get `None`, so passes that skip
/// themselves cannot leave the frame without a start marker. Conditioning each
/// pass on whether the earlier ones ran is what made this fragile.
pub(super) fn claim_start_timestamp<'a>(
    pending: &mut Option<&'a wgpu::QuerySet>,
) -> Option<wgpu::RenderPassTimestampWrites<'a>> {
    pending
        .take()
        .map(|query_set| wgpu::RenderPassTimestampWrites {
            query_set,
            beginning_of_pass_write_index: Base::Start.index(),
            end_of_pass_write_index: None,
        })
}
