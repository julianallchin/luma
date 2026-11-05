//! Per-frame cost records: the UI-thread spans, the frame sample, and the
//! always-on hitch ring whose run-up is reported when a frame arrives late.
use super::*;

/// What the UI thread knew about a frame at the moment it submitted it.
///
/// Carried through [`SerialPairing`] with the hit-test snapshot rather than
/// through the renderer, because none of it is the renderer's business — and
/// because the pairing is already the mechanism this codebase uses for
/// "app-side data that must come back with the frame it belongs to".
///
/// Without it these spans described whichever frame happened to be *painting*
/// when an older one was presented, so a gap and the interval it supposedly
/// explained could be a pipeline stage apart.
#[derive(Clone, Copy, Default)]
pub(super) struct UiSpans {
    /// Wall time since the stage's previous prepaint.
    pub(super) frame_gap_ms: f32,
    /// `request_animation_frame` to this frame's prepaint.
    pub(super) request_to_prepaint_ms: f32,
    /// How many times the stage's `render` ran since the previous prepaint.
    ///
    /// One is the healthy pairing. **Zero over a long gap is the whole reason
    /// this field exists**: it separates a UI thread that was busy elsewhere
    /// from one that was never asked for a frame at all, and those have
    /// different fixes and different owners.
    pub(super) renders: u32,
    /// Input the window dispatched during the gap, and what it cost — see
    /// [`gpui::InputLoad`]. Pointer events arrive at the mouse's rate, not
    /// the display's, and their cost is otherwise time nobody accounts for.
    pub(super) input_events: u32,
    pub(super) input_moves: u32,
    pub(super) input_ms: f32,
    pub(super) input_longest_ms: f32,
}

/// One frame's cost, small enough to keep several seconds of them.
///
/// `Copy` and all-scalar on purpose: this is written every frame on the UI
/// thread, so it must not allocate, and it is only ever read when something
/// already went wrong.
#[derive(Clone, Copy, Default, serde::Serialize)]
pub(super) struct FrameSample {
    /// Wall time between this frame reaching the screen and the previous one —
    /// the only number here the operator actually experiences.
    pub(super) interval_ms: f32,
    /// gpui's element walk on the UI thread.
    pub(super) draw_ms: f32,
    /// Score evaluation, frame assembly, hit-test rebuild.
    pub(super) score_ms: f32,
    pub(super) build_ms: f32,
    pub(super) pick_ms: f32,
    /// Renderer-thread halves. `None`, not zero, until the first profiled
    /// frame lands. One frame in sixteen is profiled and its timings are
    /// carried to the next frame delivered, so consecutive rows do repeat a
    /// value — see `AsyncPresentation::timings` for why that beats attributing
    /// it to a frame that was never presented.
    pub(super) cpu_encode_ms: Option<f32>,
    pub(super) gpu_total_ms: Option<f32>,
    /// Of `gpu_total_ms`, the part spent on the haze passes. The volumetric
    /// march is fill-bound, so this is the term that
    /// should grow when beams fill the viewport — and the one to check before
    /// blaming anything else for a heavy frame. Zero also means the haze
    /// passes finished before the scene pass they ran alongside, which puts
    /// their cost in `gpu_scene_ms` — see `FrameTimings`.
    pub(super) gpu_volumetric_ms: Option<f32>,
    /// Of `gpu_total_ms`, the scene pass plus any fixture-shadow passes ahead
    /// of it — separable by `redrawn_shadow_maps` being zero.
    pub(super) gpu_scene_ms: Option<f32>,
    /// Of `gpu_total_ms`, the composite pass. The three regions are cut at
    /// consecutive fragment-stage completions and so exhaust the total
    /// exactly, which is what lets a heavy frame be attributed rather than
    /// guessed at. Where two adjacent passes overlap on the GPU, the overlap
    /// is charged to the earlier one — see `FrameTimings`.
    pub(super) gpu_composite_ms: Option<f32>,
    /// Set only on a frame that rebuilt the clustered-light grid, which makes
    /// it a camera-motion marker as much as a cost.
    pub(super) cluster_ms: Option<f32>,
    /// The renderer worker's CPU span between claiming this frame's
    /// presentation slot and handing the work to the driver, split by phase.
    ///
    /// Unlike every other renderer-thread field here these are never `None`:
    /// they are wall-clock brackets, not adapter timestamps. That is the whole
    /// point of them. A slot reads `Rendering` from the moment it is claimed,
    /// so a worker blocked in this span produces the stall signature — slots
    /// rendering, nothing on the GPU, UI thread healthy — which no other field
    /// in this struct can see.
    pub(super) submit_total_ms: f32,
    pub(super) submit_prepare_ms: f32,
    pub(super) submit_clusters_ms: f32,
    pub(super) submit_upload_ms: f32,
    /// Acquiring the presentation target, including the shared `IOSurface` the
    /// window compositor samples. The suspect that only exists with a real
    /// compositor, which is the harness-versus-window delta.
    pub(super) submit_targets_ms: f32,
    pub(super) submit_encode_ms: f32,
    /// Frames the renderer worker had retired when this prepaint ran, and the
    /// last one's submit-to-completion span.
    ///
    /// The delta between consecutive rows is the question the census cannot
    /// answer: through a stall with the slots pinned `Rendering`, a flat count
    /// means the GPU signalled nothing, and a climbing count means the worker
    /// finished frames the UI discarded as stale. Counted on the retire path,
    /// because a discarded frame's timings never reach a sample any other way.
    pub(super) worker_finished: u64,
    pub(super) worker_last_signalled_ms: f32,
    /// Identity of this row's requested camera, transport time and dimensions.
    pub(super) submitted_serial: u64,
    /// Delivered image, which can belong to an earlier request.
    pub(super) presented_serial: Option<u64>,
    /// Source of the cached GPU timings. Join to `submitted_serial` for its
    /// inputs and count each source once, even when several images reuse it.
    pub(super) profiled_serial: Option<u64>,
    /// Air animation uses a wall clock independently of transport time.
    pub(super) haze_time_s: f32,
    /// Camera distance, so a report says how zoomed in the stage was.
    pub(super) camera_radius: f32,
    /// Exact camera handed to the renderer, in internal Z-up world space.
    pub(super) camera_eye: [f32; 3],
    pub(super) camera_target: [f32; 3],
    pub(super) camera_fov_y_deg: f32,
    /// Physical pixels the renderer was asked for.
    pub(super) width: u32,
    pub(super) height: u32,
    /// Whether the OS considered this window active when the frame was built.
    ///
    /// The stalls in the live captures happen with the transport paused, which
    /// is when an operator is most likely to have switched away — and a
    /// background process is exactly what macOS deprioritises for GPU work. If
    /// `false` correlates with the stalls, the disease is a condition of the
    /// session rather than anything the renderer does.
    ///
    /// Window *occlusion* (visible but covered) is a separate state that gpui
    /// does not surface; this covers the switched-away case only.
    ///
    /// **Always `false` under the headless test platform**, which has no OS
    /// window to be active. That is a property of the harness, not a finding
    /// about it — the field is only meaningful in a real session.
    pub(super) window_active: bool,
    /// Transport time of `submitted_serial`, in track seconds. Delivered
    /// images and GPU timings can belong to older requests; use their serials
    /// to join to the corresponding input row before comparing content.
    pub(super) track_time_s: f32,
    /// Cones the score had lit — the content axis every other number scales on.
    pub(super) lit_cones: u32,
    /// Mean broad-phase candidates per 8 px light-index tile. **The number
    /// that says whether culling works at all**: near the lit-cone count,
    /// every fragment is shading every light and the index is pure overhead.
    pub(super) mean_lights_per_tile: f32,
    /// Cones that survived the screen cull, and the total (tile, light) mask
    /// bits the broad phase set this frame.
    pub(super) lights_on_screen: u32,
    pub(super) tile_references: u32,
    /// Fixture shadow maps redrawn this frame. Zero is the healthy steady
    /// state: a map is only redrawn when its slot's projection or caster set
    /// changed, so a non-zero run means tenancy is churning.
    pub(super) redrawn_shadow_maps: u32,
    /// Wall time since the stage's previous prepaint — the UI thread's own
    /// cadence. If this tracks `interval_ms` the thread was blocked; if it
    /// stays at frame rate while `interval_ms` grows, the frames were made and
    /// something downstream held them.
    pub(super) ui_frame_gap_ms: f32,
    /// From this frame's `request_animation_frame` to the stage's prepaint —
    /// everything the UI thread did before reaching the stage. Names a stall
    /// that belongs to some *other* view without needing to instrument it.
    pub(super) request_to_prepaint_ms: f32,
    /// How long the renderer left this frame queued before starting it.
    /// Non-zero means the renderer was busy, not slow.
    pub(super) queued_ms: f32,
    /// Stage renders during `ui_frame_gap_ms`. One is healthy. Zero over a long
    /// gap means nothing asked for a frame — a different disease, and a
    /// different owner, from a thread that was busy elsewhere.
    pub(super) renders_in_gap: u32,
    /// Input events dispatched during `ui_frame_gap_ms`, the mouse moves
    /// among them, their total dispatch time and the longest one.
    pub(super) input_events: u32,
    pub(super) input_moves: u32,
    pub(super) input_ms: f32,
    pub(super) input_longest_ms: f32,
    /// `draw_ms` split at the driver's completion callback: the GPU's share
    /// (submit until it said it was done, including any wait to begin) and the
    /// worker's share (how long after that before anyone looked). This is the
    /// last unmeasured span — between them they say whether the frame was slow
    /// or merely unattended.
    pub(super) until_signalled_ms: Option<f32>,
    pub(super) until_noticed_ms: Option<f32>,
    /// Whether the frame crossed on the zero-copy shared surface. False means
    /// the CPU readback fallback, whose copy and map live inside `draw_ms` and
    /// are invisible to `gpu_total_ms`.
    pub(super) shared_surface: bool,
    /// Whether a completed frame reached the screen on this prepaint.
    ///
    /// **False rows are recorded too.** The stage submits a frame every
    /// prepaint but only sometimes has one to show, and recording only the
    /// deliveries would make a 345 ms outage look like one slow frame instead
    /// of forty silent ones. Everything measured before submission is still valid
    /// on a ghost row; everything measured on the way back is `None`.
    pub(super) delivered: bool,
    /// Whether this submission displaced an older frame that never reached the
    /// screen — the pipeline dropping work rather than falling behind.
    pub(super) replaced_undelivered: bool,
    /// The presentation slots at the moment of submission.
    pub(super) slots_idle: u8,
    pub(super) slots_rendering: u8,
    pub(super) slots_ready: u8,
    pub(super) slots_reserved: u8,
    /// Whether a slot could have been started for this frame. False is what
    /// `queued_ms` is waiting on.
    pub(super) slot_startable: bool,
}

/// A few seconds of [`FrameSample`], dumped when a frame arrives late.
///
/// # Why a ring and not a log
///
/// The interesting frames are the ones *before* the hitch — whatever built up
/// to it — and those are already past by the time anything knows a hitch
/// happened. A ring is the only shape that has them.
///
/// # Why it is always on
///
/// The bug this exists for cannot be reproduced on any machine we have. It
/// only happens on the operator's, mid-show, and asking them to first reproduce
/// it under a flag is asking them to notice it twice. Recording costs one
/// struct copy into a fixed array per frame; the file write happens only on a
/// hitch, and at most once per [`HITCH_COOLDOWN`].
pub(super) struct HitchRing {
    samples: [FrameSample; HITCH_RING],
    next: usize,
    len: usize,
    last_dump: Option<Instant>,
}

/// Four seconds at 60 Hz, matching the presentation window's own reasoning.
const HITCH_RING: usize = 240;

/// A frame that took this long to reach the screen is one the eye caught.
/// Three missed vsyncs at 60 Hz — below this is jitter, above it is a stutter.
pub(super) const HITCH_MS: f32 = 50.0;

/// A bad patch is one report, not one per frame.
const HITCH_COOLDOWN: Duration = Duration::from_secs(10);

impl Default for HitchRing {
    fn default() -> Self {
        Self {
            samples: [FrameSample::default(); HITCH_RING],
            next: 0,
            len: 0,
            last_dump: None,
        }
    }
}

impl HitchRing {
    /// Record one frame, and say whether it is worth reporting.
    ///
    /// Returns the run-up in order, oldest first, or `None` when the frame was
    /// fine or the last report is still recent.
    pub(super) fn record(&mut self, sample: FrameSample, now: Instant) -> Option<Vec<FrameSample>> {
        self.samples[self.next] = sample;
        self.next = (self.next + 1) % HITCH_RING;
        self.len = (self.len + 1).min(HITCH_RING);

        // A ghost row has no interval to be late by, and must never fire the
        // report — it is run-up, not a symptom.
        if !sample.delivered || sample.interval_ms < HITCH_MS {
            return None;
        }
        if self
            .last_dump
            .is_some_and(|last| now.duration_since(last) < HITCH_COOLDOWN)
        {
            return None;
        }
        self.last_dump = Some(now);
        // Oldest first, so the report reads forwards into the hitch.
        Some(self.recent().copied().collect())
    }

    /// The retained window, oldest first — the same order a hitch dump reads
    /// in, because it is the same window.
    pub(super) fn recent(&self) -> impl Iterator<Item = &FrameSample> + '_ {
        let start = (self.next + HITCH_RING - self.len) % HITCH_RING;
        (0..self.len).map(move |offset| &self.samples[(start + offset) % HITCH_RING])
    }
}

#[cfg(test)]
mod hitch_tests {
    use super::{FrameSample, HitchRing, HITCH_MS, HITCH_RING};
    use std::time::{Duration, Instant};

    fn frame(interval_ms: f32) -> FrameSample {
        FrameSample {
            delivered: true,
            interval_ms,
            ..FrameSample::default()
        }
    }

    /// A ghost row carries no interval, so it can never fire the report even
    /// though the ring is now mostly ghosts during a stall.
    #[test]
    fn a_prepaint_that_delivered_nothing_never_fires_the_report() {
        let mut ring = HitchRing::default();
        let now = Instant::now();
        for _ in 0..HITCH_RING {
            let ghost = FrameSample {
                delivered: false,
                // Nonsense value on purpose: even if a caller ever filled this
                // in on an undelivered frame, `delivered` is what decides.
                interval_ms: HITCH_MS * 10.0,
                ..FrameSample::default()
            };
            assert!(ring.record(ghost, now).is_none());
        }
        // And the delivery that ends the stall still reports, with the ghosts
        // as its run-up.
        let late = FrameSample {
            delivered: true,
            interval_ms: HITCH_MS + 1.0,
            ..FrameSample::default()
        };
        let report = ring.record(late, now).expect("the delivery reports");
        assert_eq!(report.len(), HITCH_RING);
        assert_eq!(
            report.iter().filter(|frame| !frame.delivered).count(),
            HITCH_RING - 1,
            "the run-up should be the frames that were made and thrown away"
        );
    }

    #[test]
    fn a_healthy_stream_reports_nothing() {
        let mut ring = HitchRing::default();
        let now = Instant::now();
        for _ in 0..HITCH_RING * 2 {
            assert!(ring.record(frame(16.0), now).is_none());
        }
    }

    /// The report is the run-up, oldest first, ending on the frame that was
    /// late — a dump that started at the hitch would describe the symptom and
    /// throw away the cause.
    #[test]
    fn a_late_frame_reports_the_run_up_in_order_ending_on_itself() {
        let mut ring = HitchRing::default();
        let now = Instant::now();
        for i in 0..10 {
            assert!(ring.record(frame(10.0 + i as f32), now).is_none());
        }
        let report = ring.record(frame(HITCH_MS + 1.0), now).expect("a hitch");
        assert_eq!(report.len(), 11);
        assert_eq!(report[0].interval_ms, 10.0);
        assert_eq!(report[9].interval_ms, 19.0);
        assert_eq!(report[10].interval_ms, HITCH_MS + 1.0);
    }

    /// A show that is hitching continuously is one report, then quiet — the
    /// log is a diagnosis, not a firehose that fills the user's disk.
    #[test]
    fn a_sustained_bad_patch_reports_once_until_the_cooldown_passes() {
        let mut ring = HitchRing::default();
        let start = Instant::now();
        assert!(ring.record(frame(HITCH_MS + 1.0), start).is_some());
        for tick in 1..60 {
            let soon = start + Duration::from_millis(tick * 100);
            assert!(
                ring.record(frame(HITCH_MS + 1.0), soon).is_none(),
                "reported again {tick} ticks into the cooldown"
            );
        }
        let later = start + super::HITCH_COOLDOWN + Duration::from_millis(1);
        assert!(ring.record(frame(HITCH_MS + 1.0), later).is_some());
    }

    /// Older than the ring is gone, and what is left is still in order — an
    /// off-by-one in the wrap would silently reorder the run-up.
    #[test]
    fn the_ring_keeps_the_most_recent_frames_in_order_once_it_has_wrapped() {
        let mut ring = HitchRing::default();
        let now = Instant::now();
        // Distinct, increasing, and all comfortably under HITCH_MS so the
        // run-up is a run-up and not a string of hitches. An eighth is exact in
        // binary, so the differences below compare exactly.
        let interval = |i: usize| i as f32 * 0.125;
        // Six more records than the ring holds, so the first six fall off.
        for i in 1..=HITCH_RING + 5 {
            assert!(ring.record(frame(interval(i)), now).is_none());
        }
        let report = ring
            .record(frame(HITCH_MS + 1.0), now)
            .expect("a hitch after wrapping");
        assert_eq!(report.len(), HITCH_RING);
        assert_eq!(report[0].interval_ms, interval(7), "the oldest survivor");
        assert_eq!(
            report[HITCH_RING - 1].interval_ms,
            HITCH_MS + 1.0,
            "the report ends on the late frame"
        );
        // Every step before the hitch is exactly one apart, which is what an
        // off-by-one in the wrap would break.
        for pair in report[..HITCH_RING - 1].windows(2) {
            assert_eq!(pair[1].interval_ms - pair[0].interval_ms, 0.125);
        }
    }
}
