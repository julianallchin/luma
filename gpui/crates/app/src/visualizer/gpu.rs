//! The renderer worker behind the stage: one frame's inputs, its submission
//! through the async viewport, and the completed frame that comes back.
use super::*;

/// The app's own record of a submitted frame, returned when it is presented.
pub(super) struct SubmittedFrame {
    pick: PickSnapshot,
    spans: UiSpans,
}

pub(super) type PickTimeline = SerialPairing<SubmittedFrame>;

/// The sprite-atlas identity every stage frame is published under.
///
/// One identity for the life of the process, deliberately. The tile it names is
/// refreshed in place ([`Window::update_image`]) rather than reinserted, so a
/// fresh id per frame would create and destroy a full-screen texture at frame
/// rate — and a fresh id per *stage* would strand the old tile, because a
/// dropped stage has no window to remove it with. The stage pane is a
/// singleton, so one identity covers every stage that will ever be on screen.
static STAGE_IMAGE_ID: std::sync::OnceLock<ImageId> = std::sync::OnceLock::new();

/// The stage's free-running clock: [`luma_render::strobe::clock`], whether
/// or not the show is playing. The air drifts and the strobes flash on it,
/// and the DMX engine gates its strobes on the same clock, so the screen
/// flashes in phase with the rig.
/// The fixtures whose strobe the rig runs on the fixture's own clock — those
/// whose mode has a strobe channel — and a [`FixtureClock`] per head of them.
///
/// The DMX engine writes those fixtures a strobe rate and lets them flash, so
/// the stage view simulates the fixture's clock instead of flashing them on
/// the shared one: a rate that changes faster than the fixture adopts it
/// flashes on screen as rarely as it does on the rig. Every other fixture's
/// strobe is the engine pulsing its dimmer on the shared clock, which the
/// renderer already draws.
#[derive(Default)]
pub(super) struct FixtureStrobes {
    own_clock: std::collections::HashSet<String>,
    clocks: HashMap<(String, usize), FixtureClock>,
}

impl FixtureStrobes {
    pub(super) fn new(rig: &crate::library::Rig) -> Self {
        let own_clock = rig
            .fixtures
            .iter()
            .filter(|fixture| {
                rig.definitions
                    .get(&fixture.fixture_path)
                    .is_some_and(|def| {
                        luma_lib::fixtures::engine::strobes_on_own_clock(def, &fixture.mode_name)
                    })
            })
            .map(|fixture| fixture.id.clone())
            .collect();
        Self {
            own_clock,
            clocks: HashMap::new(),
        }
    }

    /// `state` as the rig shows it over `slice`: a fixture on its own clock
    /// has its flashing folded into its dimmer and no strobe left for the
    /// renderer's shared clock.
    fn apply(
        &mut self,
        id: &str,
        head: usize,
        mut state: scene_desc::PrimitiveState,
        slice: luma_render::strobe::Slice,
    ) -> scene_desc::PrimitiveState {
        if self.own_clock.contains(id) {
            let clock = self.clocks.entry((id.to_string(), head)).or_default();
            state.dimmer *= clock.gain(state.strobe, slice);
            state.strobe = 0.0;
        }
        state
    }
}

#[derive(Default)]
pub(super) struct StageClock {
    /// The clock at the last submitted frame.
    submitted: Option<f64>,
}

impl StageClock {
    /// The longest slice of the clock a frame covers, in seconds. After a
    /// pause in drawing, the next frame does not answer for everything missed.
    const LONGEST_S: f64 = 1.0 / 15.0;

    /// The clock now, and the seconds since the last submitted frame.
    pub(super) fn now(&self) -> (f64, f64) {
        let now = luma_render::strobe::clock();
        let interval = self
            .submitted
            .map_or(luma_render::footage::FRAME_S, |last| {
                (now - last).clamp(1e-4, Self::LONGEST_S)
            });
        (now, interval)
    }

    pub(super) fn submitted(&mut self, clock: f64) {
        self.submitted = Some(clock);
    }
}

// -- the GPU, and the presentation seam --------------------------------------

/// The renderer and the meshes it has loaded.
pub(super) struct Gpu {
    pub(super) viewport: AsyncViewport,
    /// The camera export recorded with the last submission, and its serial.
    /// Taken by the prepaint, which writes it.
    pub(super) exported: Option<(u64, luma_render::camera_export::CameraExport)>,
    /// The UI-thread cost of the most recent [`Self::frame`] call.
    ///
    /// Held here rather than returned because these phases run on every call
    /// while a *completed* frame arrives only sometimes, and a measurement
    /// that existed only on the frames that finished would miss precisely the
    /// frames that did not.
    pub(super) work: StageWork,
    /// What the presentation seam did with the most recent submission.
    ///
    /// Held here for the same reason as `work`, and it is the half that was
    /// missing: a submission happens every call, a delivery only sometimes,
    /// and the frames that were submitted and never delivered are exactly the
    /// ones a stall is made of.
    pub(super) submission: Submission,
    pub(super) assets: assets::Library,
    pub(super) picks: PickTimeline,
    /// The hit-test geometry and its per-asset BVHs, kept across frames.
    pub(super) pick_cache: PickCache,
}

/// What the presentation seam did with one submitted frame.
#[derive(Clone, Copy, Default)]
pub(super) struct Submission {
    pub(super) serial: u64,
    pub(super) haze_time_s: f32,
    /// This frame pushed an older, undelivered one out of the queue.
    pub(super) replaced_undelivered: bool,
    pub(super) slots: luma_render::Occupancy,
    /// Worker completions at this submit, paired with the census so the two
    /// are read at the same instant — a count sampled a frame later would not
    /// say whether the slots this census calls `Rendering` were moving.
    pub(super) finished: u64,
    pub(super) last_signalled: std::time::Duration,
}

pub(super) struct LiveFrameInputs<'a> {
    pub(super) scene: &'a scene_desc::Scene,
    pub(super) definitions: &'a BTreeMap<String, scene_desc::Definition>,
    /// Fixtures that strobe on their own clock, advanced by this frame.
    pub(super) strobes: &'a mut FixtureStrobes,
    /// The frame's moment on the stage clock.
    pub(super) moment: Moment,
    /// The heads' state at it.
    pub(super) state: Option<&'a UniverseState>,
    /// The projectors that turn its beams into point streams.
    pub(super) lasers: &'a luma_lib::laser::LaserRig,
    pub(super) size: (u32, u32),
    pub(super) camera: Camera,
    /// Measured in the prepaint that is submitting this frame, so they come
    /// back paired with its own presentation interval.
    pub(super) spans: UiSpans,
    /// Record this frame's camera and sun for a camera export.
    pub(super) export: Option<luma_render::camera_export::Context>,
}

/// What one frame cost the **UI thread**, split by phase.
///
/// [`FrameTimings`] is the renderer's half and [`luma_render::Pacing`] the
/// presentation seam's. This is the half that runs inside gpui's `draw`, and
/// it is the only one whose cost freezes the window rather than merely slowing
/// the picture: a stalled worker still leaves the app answering the pointer.
///
/// Split by phase because the three have nothing to do with each other and
/// different fixes. A profiler that renders frames in a loop sees none of
/// them, which is why an isolated benchmark can look healthy while playback is
/// unusable.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct StageWork {
    /// Evaluating the score at the playhead — [`Library::sample_universe`],
    /// which composites every annotation whose span contains it.
    pub(super) sample_ms: f32,
    /// Assembling the renderer's frame from the resolved scene.
    pub(super) build_ms: f32,
    /// Rebuilding the hit-test world so a click can be paired to these pixels.
    pub(super) pick_ms: f32,
}

impl StageWork {
    /// What the UI thread spent on the stage altogether.
    pub(super) fn total_ms(self) -> f32 {
        self.sample_ms + self.build_ms + self.pick_ms
    }
}

pub(super) struct CompletedFrame {
    pub(super) serial: u64,
    /// Physical pixels of `frame`.
    pub(super) size: (u32, u32),
    pub(super) timings_serial: Option<u64>,
    pub(super) frame: StageFrame,
    pub(super) pick: PickSnapshot,
    pub(super) draw_ms: f32,
    pub(super) timings: Option<FrameTimings>,
    /// Spacing of the frames that have reached the screen, which is the only
    /// number here that reflects what the operator is actually seeing —
    /// `timings` describes a frame, this describes the stream of them.
    pub(super) pacing: Option<MetricSummary>,
    /// This frame's own spacing from the one before it. `pacing` summarises the
    /// stream and so cannot name the frame that broke it; a hitch report needs
    /// the individual interval.
    pub(super) interval_ms: Option<f32>,
    /// Fixture shadow maps this frame actually redrew.
    pub(super) redrawn_shadow_maps: u32,
    /// The unified light index this frame shaded against.
    pub(super) clusters: luma_render::LightIndexStats,
    /// How long the UI thread's request waited before the renderer started it.
    pub(super) queued_ms: f32,
    /// The renderer worker's slot-claim-to-submit span, by phase. Always
    /// present — see `FrameSample::submit_total_ms`.
    pub(super) cpu: luma_render::CpuSpans,
    /// `draw_ms` split at the driver's completion callback.
    pub(super) until_signalled_ms: Option<f32>,
    pub(super) until_noticed_ms: Option<f32>,
    /// Whether this frame came home on the zero-copy shared surface.
    pub(super) shared_surface: bool,
    /// The UI-thread spans measured when *this* frame was submitted, not when
    /// it came back — which is what makes them comparable with `interval_ms`.
    pub(super) spans: UiSpans,
}

/// A finished stage frame in the form the compositor will draw it.
///
/// The renderer decides which of these it can produce; the screen paints
/// whichever it is handed. Neither end chooses — that is what keeps the CPU
/// path a fallback rather than a mode.
/// Start compiling the renderer's pipelines, on the compositor's device
/// where the compositor has one to offer.
///
/// The one place gpui's device meets `luma_render`: neither crate knows the
/// other, so the struct is spread here, field for field. Idempotent — the
/// adoption keeps a live device and the warmup starts once.
pub fn warm_renderer(window: &Window) {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if let Some(gpui::WgpuDevice {
        device,
        queue,
        adapter,
        lost,
    }) = window.wgpu_device()
    {
        luma_render::device::DeviceContext::adopt(device, queue, adapter, lost);
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let _ = window;
    luma_render::warm();
}

#[derive(Clone)]
pub(super) enum StageFrame {
    /// Uploaded into gpui's sprite atlas under [`STAGE_IMAGE_ID`], refreshed in
    /// place each frame.
    Image(Arc<RenderImage>),
    /// Memory the renderer and the compositor both address. Nothing is
    /// uploaded and nothing is copied; the compositor samples where the
    /// renderer wrote.
    Shared(luma_render::Surface),
}

pub(super) type PaintedStage = Option<(StageFrame, Option<PickSnapshot>)>;

impl Gpu {
    /// Queue one frame and hand back the newest completed image, if any.
    ///
    /// Frame assembly stays on the caller because it samples current evaluator
    /// state. Device submission, mapping and its blocking wait are owned by
    /// [`AsyncViewport`]'s renderer thread. This method only enqueues and drains
    /// a completed slot; neither operation polls the GPU.
    ///
    /// # Errors
    /// A mesh that would not load, or a readback that would not map, as a
    /// message fit to put on the screen.
    pub(super) fn frame(
        &mut self,
        input: LiveFrameInputs<'_>,
    ) -> Result<Option<CompletedFrame>, String> {
        let LiveFrameInputs {
            scene,
            definitions,
            strobes,
            moment,
            state,
            lasers,
            size: (width, height),
            camera,
            spans,
            export,
        } = input;
        let built = std::time::Instant::now();
        let strobes = RefCell::new(strobes);
        let mut frame = build_frame_at(
            scene,
            definitions,
            &|id, head| {
                let primitive = stage_render::primitive_state(state, id, head)?;
                Some(
                    strobes
                        .borrow_mut()
                        .apply(id, head, primitive, moment.slice),
                )
            },
            moment,
            &mut self.assets,
        )
        .map_err(|error| format!("Could not assemble the frame: {error}"))?;
        if let Some(state) = state {
            frame.lasers = stage_render::laser_scans(lasers, state, moment.time);
        }
        let haze_time_s = frame.time;
        self.work.build_ms = built.elapsed().as_secs_f32() * 1_000.0;
        let picked = std::time::Instant::now();
        // The stored camera, not `frame.camera`: the footage look's shake
        // turns only the picture, so a click aims where the camera points.
        // The camera export below keeps the shake.
        let pick = PickSnapshot::from_frame(&frame, scene, camera, &mut self.pick_cache);
        self.work.pick_ms = picked.elapsed().as_secs_f32() * 1_000.0;
        let completed = self
            .viewport
            .take_latest()
            .transpose()
            .map_err(|error| format!("Could not render the frame: {error}"))?;
        let exported = export
            .map(|context| luma_render::camera_export::CameraExport::capture(&frame, context));
        let (serial, outcome, occupancy) = self.viewport.submit_numbered(frame, width, height);
        self.exported = exported.map(|export| (serial, export));
        let (finished, last_signalled) = self.viewport.finished();
        self.submission = Submission {
            serial,
            haze_time_s,
            replaced_undelivered: matches!(outcome, SubmitOutcome::Replaced { .. }),
            slots: occupancy,
            finished,
            last_signalled,
        };
        self.picks
            .submitted(serial, SubmittedFrame { pick, spans }, outcome);
        let Some(presented) = completed else {
            return Ok(None);
        };
        let draw_ms = presented.draw_time.as_secs_f32() * 1_000.0;
        let (width, height) = (presented.width, presented.height);
        // Which path the frame came home on. `draw_ms` covers submit to
        // observed completion, and on the CPU fallback that span contains a
        // full-viewport copy and buffer map that the GPU timestamps (scene
        // through composite) do not see — so without this, "draw_ms far exceeds
        // gpu_total_ms" has two very different explanations and no way to pick.
        let shared_surface = matches!(presented.image, luma_render::Presented::Shared(_));
        let frame = match presented.image {
            luma_render::Presented::Shared(surface) => StageFrame::Shared(surface),
            luma_render::Presented::Pixels(pixels) => {
                let buffer = image::RgbaImage::from_raw(width, height, pixels)
                    .ok_or_else(|| "readback was not width * height * 4 bytes".to_string())?;
                let mut image = RenderImage::new([image::Frame::new(buffer)]);
                // Publish every frame under one atlas identity — see
                // `STAGE_IMAGE_ID`.
                match STAGE_IMAGE_ID.get() {
                    Some(id) => image.id = *id,
                    None => {
                        let _ = STAGE_IMAGE_ID.set(image.id);
                    }
                }
                StageFrame::Image(Arc::new(image))
            }
        };
        let submitted = self
            .picks
            .presented(presented.serial)
            .ok_or_else(|| format!("presentation {} lost its pick snapshot", presented.serial))?;
        Ok(Some(CompletedFrame {
            serial: presented.serial,
            size: (width, height),
            timings_serial: presented.timings_serial,
            frame,
            pick: submitted.pick,
            spans: submitted.spans,
            draw_ms,
            timings: presented.timings,
            pacing: self.viewport.pacing(),
            redrawn_shadow_maps: presented.shadows.redrawn_maps as u32,
            clusters: presented.clusters,
            queued_ms: presented.queued.as_secs_f32() * 1_000.0,
            cpu: presented.cpu,
            until_signalled_ms: presented
                .until_signalled
                .map(|gap| gap.as_secs_f32() * 1_000.0),
            until_noticed_ms: presented
                .until_noticed
                .map(|gap| gap.as_secs_f32() * 1_000.0),
            shared_surface,
            interval_ms: presented
                .since_previous
                .map(|gap| gap.as_secs_f32() * 1_000.0),
        }))
    }
}

/// Whether a stage may build a renderer, from
/// [`Runtime::stage_gpu`](luma_ui::runtime::Runtime).
///
/// **Default on.** Production, and any test that has not deliberately opted
/// out, are untouched: the pane mounting wherever a venue is on screen is
/// correct product behaviour, and this switch does not change it.
///
/// What it buys is the headless suite. The stage mounts on *any* venue, so a
/// test asserting on venue-list focus rings was paying a full shader
/// compilation for a viewport it never looks at — cost with no evidence
/// attached. Turning the device off leaves the pane, its chrome, its layout
/// and its node tree exactly as they are (see [`body`], which substitutes an
/// inert plate carrying the same `Stage` node) and skips only the wgpu device.
///
/// It is read once per stage rather than per frame, and only ever *off* by
/// request: a run that forgets to set it renders normally, which is the safe
/// direction for a switch whose wrong value is invisible in a screenshot.
pub(super) fn stage_gpu_enabled() -> bool {
    luma_ui::runtime::Runtime::with(luma_ui::runtime::Runtime::stage_gpu_enabled)
}
