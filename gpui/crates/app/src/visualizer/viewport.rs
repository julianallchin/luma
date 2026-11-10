//! The stage canvas: the idle gate, the per-frame prepaint that samples the
//! score and submits a frame, the paint that presents it, and pointer input.
use super::*;

/// Everything a live frame is a function of.
///
/// With stationary haze, two prepaints with equal keys ask for the same
/// picture. Once temporal sampling has settled there is nothing left to render — the
/// stage's prepaint skips the submission and re-presents the frame already on
/// screen, which is what lets a still, paused stage cost zero GPU instead of
/// re-marching the haze at display rate for nobody. The scene's own geometry
/// is deliberately absent: this screen only mutates it during a pointer drag,
/// and a drag holds the gate open (see `interacting` in the prepaint).
#[derive(PartialEq)]
pub(super) struct IdleKey {
    /// `f32::to_bits` — an equality key wants exactness, not float semantics.
    pub(super) time_bits: u32,
    pub(super) camera: Camera,
    pub(super) size: (u32, u32),
    /// Every renderer control. Panel visibility is not part of it, because it
    /// draws nothing.
    pub(super) controls: RenderControls,
    pub(super) selected: Vec<String>,
    pub(super) selected_pieces: Vec<String>,
    pub(super) gizmo_mode: GizmoMode,
    pub(super) gizmo_hover: Option<GizmoHandle>,
    pub(super) universe: Option<UniverseState>,
    /// The window's luminance range: a switch between SDR and HDR must redraw
    /// a resting stage, whose last frame was made for the other one.
    pub(super) display: luma_render::DisplayRange,
}

/// The luminance range `window` presents in, as the renderer takes it.
fn display_range(window: &Window) -> luma_render::DisplayRange {
    window
        .hdr_output()
        .map_or(luma_render::DisplayRange::Sdr, |output| {
            luma_render::DisplayRange::Hdr {
                headroom: output.headroom(),
            }
        })
}

/// Frames of unchanged inputs before the stage may rest: moved fixture
/// shadow maps are redrawn a budget a frame, and the haze caches settle
/// behind them.
const SETTLE_FRAMES: u32 = 16;
/// The same with auto exposure on: the exposure adapts over a few seconds
/// (`post.rs`), and a stage that rested sooner would freeze it half way.
const EXPOSURE_SETTLE_FRAMES: u32 = 180;

/// The viewport itself, or the reason there isn't one.
pub(super) fn body(state: &mut Visualizer, app: &Entity<Luma>, library: &Library) -> AnyElement {
    // A hovered preset plays alone in place of the score. A failed sample
    // shows the score, as if nothing were hovered.
    let graph_sample = state.audition.as_ref().and_then(|audition| {
        let sampled = std::time::Instant::now();
        let (time, universe) = audition.sample().ok()?;
        Some((
            time,
            Some(universe),
            sampled.elapsed().as_secs_f32() * 1_000.0,
        ))
    });
    // A frame that failed drew nothing and left its reason behind; adopt it
    // before deciding what this frame shows.
    if let Some(error) = state.stage.borrow_mut().error.take() {
        log::warn!("the stage stopped drawing: {error}");
        state.status = Status::Empty("The stage stopped drawing".to_string());
    }
    // Device off by request: keep the pane's own node so a tree assertion
    // cannot tell the two configurations apart, and skip the renderer.
    if !state.gpu_enabled {
        return plate("Stage rendering is off".to_string())
            .agent_node(Role::Card, "Stage")
            .into_any_element();
    }
    if state.exporting {
        return plate("Exporting the show…".to_string());
    }
    // Idempotent, and normally a no-op: launch has already started this (see
    // `warm_renderer`, which also adopts the window's device first). It is
    // here as well so a stage opened by something that is not the app — a
    // harness, a test — still drives the warmup to a conclusion instead of
    // sitting on `Cold` for ever; with no window to adopt from, that warmup
    // builds the renderer's own device, which is the right answer headless.
    luma_render::warm();
    // Compiling every pipeline is the one wait at launch long enough to need
    // naming. Saying so beats a blank rectangle, which is indistinguishable
    // from a stage with nothing in it. The pane re-renders every frame
    // (`request_animation_frame` above), so this clears itself.
    match luma_render::warming() {
        luma_render::Warming::Compiling | luma_render::Warming::Cold => {
            return plate("Compiling shaders…".to_string());
        }
        // The reason is the renderer's, and the renderer logged it.
        luma_render::Warming::Unavailable(_) => {
            return plate("The GPU is unavailable".to_string());
        }
        luma_render::Warming::Ready { .. } => {}
    }
    if matches!(state.status, Status::Empty(_))
        || state.stage.borrow().scene.is_none()
        || matches!(state.stage.borrow().assets, Some(Warming::Loading))
        || !state.gpu_ready()
    {
        return plate(match &state.status {
            Status::Loading => "Loading the rig…".to_string(),
            Status::Empty(why) => why.clone(),
            Status::Live if state.stage.borrow().assets.is_some() => "Loading the rig…".to_string(),
            Status::Live => "Nothing to draw".to_string(),
        });
    }

    // A hitch the paint closure noticed last frame, reported from here because
    // this is the side of the stage that can reach the library. One write, at
    // most once every `HITCH_COOLDOWN`, and never on the frame that stuttered.
    if let Some(run_up) = state.stage.borrow_mut().pending_hitch.take() {
        library.record_telemetry(serde_json::json!({
            "event": "stage-hitch",
            "data": {
                // Read cold, months later, by someone who did not write this.
                // The schema block is not padding: a bare array of numbers with
                // no units and no marker for the offending frame is a capture
                // that has to be decoded before it can be used, and the point
                // of this record is that it arrives from a machine we cannot
                // ask questions of.
                "schema": {
                    "units": "every field ending in Ms is milliseconds",
                    "order": "frames are oldest first; the last one is the late frame",
                    "lateFrameIndex": run_up.len().saturating_sub(1),
                    "thresholdMs": HITCH_MS,
                    "fields": {
                        "interval_ms": "frame-to-screen spacing; what the eye saw",
                        "ui_frame_gap_ms": "gap between stage prepaints (UI thread cadence)",
                        "request_to_prepaint_ms": "request_animation_frame to this prepaint",
                        "queued_ms": "waited for the renderer to start it",
                        "renders_in_gap": "stage renders during ui_frame_gap_ms; 1 is healthy, 0 means nothing asked for a frame",
                        "input_ms": "input dispatch time during ui_frame_gap_ms (input_events events, input_moves of them mouse moves; input_longest_ms the longest)",
                        "shared_surface": "true = zero-copy (an IOSurface on Metal, a texture on the compositor's own device on wgpu); false = CPU readback, whose copy and map sit inside draw_ms and are invisible to gpu_total_ms",
                        "delivered": "false = a prepaint that got no frame back; interval/draw/queued/gpu are absent on those rows, everything measured before submission is valid",
                        "submitted": "false = this prepaint built and submitted nothing because an earlier frame was still in flight (one frame is on the GPU at a time); its submitted_serial is that earlier frame's, and its camera and track time were never rendered",
                        "replaced_undelivered": "this submission pushed an older frame out of the queue before it ever reached the screen",
                        "slots_idle/rendering/ready/reserved": "presentation slots at this prepaint; reserved are held because their surface is still on screen",
                        "slot_startable": "false = no slot could take this frame, which is what queued_ms then waits on",
                        "draw_ms": "renderer submit to observed completion (latency, not cost); until_signalled_ms + until_noticed_ms is the same span, split",
                        "until_signalled_ms": "submit until the driver's completion callback fired — the GPU's share, including any wait to begin executing",
                        "until_noticed_ms": "completion callback until the worker observed it — nobody was looking, not anything slow",
                        "score_ms": "score evaluation, UI thread",
                        "build_ms": "frame assembly, UI thread",
                        "pick_ms": "hit-test rebuild, UI thread",
                        "cpu_encode_ms": "renderer CPU encode and submit; null when this frame carried no timestamps",
                        "gpu_total_ms": "GPU timeline from the first render pass to composite completion, from the most recently profiled frame (one in sixteen) rather than necessarily this one; null until the first lands",
                        "gpu_volumetric_ms": "of gpu_total_ms, the haze passes — the fill-bound term; zero also means the haze passes finished before the scene pass they ran alongside, putting their cost in gpu_scene_ms",
                        "gpu_scene_ms": "of gpu_total_ms, the scene pass plus any shadow passes ahead of it (redrawn_shadow_maps==0 makes it scene alone)",
                        "gpu_composite_ms": "of gpu_total_ms, the composite pass; scene+volumetric+composite are cut at consecutive fragment-stage completions and exhaust the total exactly, with time two concurrent passes shared charged to whichever finished first",
                        "cluster_ms": "clustered-light rebuild; null when untimed, 0 on a cache hit",
                        "submit_total_ms": "renderer worker, slot claim to queue.submit. NEVER null — a wall bracket, not an adapter timestamp. A slot reads Rendering from the moment it is claimed, so this is the only field that can see a worker blocked BEFORE anything reaches the GPU, which is the stall signature (slots rendering, no GPU work, UI thread healthy)",
                        "submit_prepare/clusters/upload/targets/encode_ms": "of submit_total_ms, disjoint and summing to it. targets = acquiring the presentation target including the shared surface the compositor samples; encode = command encoding through queue.submit",
                        "worker_finished": "frames the renderer worker had RETIRED at this submit, delivered or discarded. Read the delta between rows: through a stall with slots pinned Rendering, flat = the GPU signalled nothing, climbing = the worker finished frames the UI threw away as stale. The two need opposite fixes",
                        "worker_last_signalled_ms": "the last retired frame's submit-to-completion span, including frames discarded as stale — whose timings reach no other field",
                        "redrawn_shadow_maps": "fixture shadow maps redrawn; 0 is healthy",
                        "track_time_s": "transport position in track seconds — hold this constant when comparing anything else",
                        "window_active": "false = the OS did not consider this window active (switched away); macOS deprioritises background GPU work, so check this against any stall. ALWAYS false under the headless harness, which has no OS window — only meaningful in a real session",
                        "lit_cones": "cones the score had lit",
                        "mean_lights_per_tile": "mean broad-phase candidates per 8px light-index tile; near lit_cones means culling is not working and every fragment shades every light",
                        "lights_on_screen": "cones that survived the screen cull",
                        "tile_references": "total (tile, light) mask bits the broad phase set",
                        "camera_radius": "camera distance; how zoomed in the stage was",
                        "width": "physical pixels rendered across",
                        "height": "physical pixels rendered down",
                    },
                    "reading": "All spans describe the same frame as interval_ms; they are paired by submission serial. Work the late frame in order: renders_in_gap 0 means nothing asked for a frame; ui_frame_gap_ms tracking interval_ms means the UI thread did not produce one, and request_to_prepaint_ms says how much of that belonged to another view before the walk reached the stage; if ui_frame_gap_ms stays at frame rate while interval_ms grows, the frames were made and something downstream held them. Then read the delivered=false rows between two deliveries: with submitted=false the stage was waiting for the frame in flight, which is the GPU taking longer than a refresh; with submitted=true a frame was made and thrown away, and on those rows slot_startable, the slot counts and replaced_undelivered say why.",
                },
                "hitchMs": run_up.last().map_or(0.0, |frame| frame.interval_ms),
                "frames": run_up,
            },
        }));
    }

    // The one live read. `render_time` and `sample_universe` are synchronous
    // because a frame's inputs must be this frame's — see `Library`.
    let (time, universe, sample_ms) = graph_sample.unwrap_or_else(|| {
        let time = library.render_time();
        let sampled = std::time::Instant::now();
        let universe = library.sample_universe(time);
        (time, universe, sampled.elapsed().as_secs_f32() * 1_000.0)
    });
    // The frame is one moment on the stage's free-running clock: its strobes
    // show the share of the slice since the last frame their gate is on. The
    // score is sampled at the transport time, which stands still while paused.
    let (clock, interval) = state.stage.borrow().clock.now();
    // Paused, nothing is coming out of the subs, so nothing shakes — the
    // envelope under a parked playhead is not a sound being played.
    let bass = match &state.bass {
        Some(bass) if library.transport().is_playing => bass.bass_at(time),
        _ => 0.0,
    };
    let moment = luma_render::footage::moment(clock, interval, bass);
    // The drawn heads lag the score the way motors would; the output does not.
    let universe = state.motors.step(clock, time, universe);
    state.status = Status::Live;

    // Only resolved values cross into the `'static` paint closure; the mutable
    // render controls remain owned by the screen.
    let stage = Rc::clone(&state.stage);
    let camera = state.camera;
    // Overlay builder expects primary first; Selection keeps primary at the
    // tail for deterministic shift-toggle reassignment.
    let selected_fixture_ids: Vec<String> = state
        .selection
        .selected()
        .iter()
        .filter(|_| !state.presentation)
        .rev()
        .filter_map(|object| match object {
            EditorObject::Fixture(id) => Some(id.clone()),
            EditorObject::StagePiece(_) => None,
        })
        .collect();
    let selected_piece_ids: Vec<String> = state
        .selection
        .selected()
        .iter()
        .filter(|_| !state.presentation)
        .filter_map(|object| match object {
            EditorObject::StagePiece(id) => Some(id.clone()),
            EditorObject::Fixture(_) => None,
        })
        .collect();
    // Keep selection highlights on bolted pieces while surface placements
    // receive only the handles admitted by their mounting frame.
    let gizmo_piece_ids: Vec<String> = selected_piece_ids
        .iter()
        .filter(|id| {
            state
                .build
                .as_ref()
                .is_none_or(|build| build.gizmo_space(id).is_some())
        })
        .cloned()
        .collect();
    let gizmo_space = state.selection_gizmo_space();
    let gizmo_mode = state.gizmo_mode;
    let gizmo_hover = state.gizmo_hover;
    // The builder's ghost, measurement and socket beads, snapshotted for the
    // paint closure. They ride inside `scene.editor`, which `IdleKey` compares
    // whole — so a ghost that moved is a frame the stage owes, without a
    // second flag to remember.
    let build_affordances = state
        .build
        .as_ref()
        .filter(|_| !state.presentation)
        .map_or_else(luma_render::scene_desc::Build::default, |build| {
            let mut editor = scene_desc::Editor::default();
            crate::stage::install(build, &mut editor);
            editor.build
        });
    // A pointer drag holds the idle gate open: camera drags move the key's
    // camera anyway, but a gizmo drag mutates the scene's geometry, which the
    // key deliberately does not carry.
    let interacting = state.drag.is_some()
        || state.editor_drag.is_some()
        || state
            .build
            .as_ref()
            .filter(|_| !state.presentation)
            .is_some_and(|build| build.hand.owns_pointer());
    let key_controls = state.render_controls.clone();
    let sized = app.clone();

    canvas(
        move |bounds: Bounds<Pixels>, window, cx| {
            // The size the next drag will be scaled by. Written here because
            // prepaint is where a laid-out size first exists.
            sized.update(cx, |this, _| {
                if let Some(state) = this.visualizer_mut() {
                    state.size = bounds.size;
                    state.viewport_origin = bounds.origin;
                    if std::mem::take(&mut state.owes_opening_pose) {
                        state.camera = state.opening_pose();
                        state.posed = true;
                    }
                }
            });
            // The UI thread's own cadence, taken before anything in this
            // closure runs so it describes the gap rather than this frame.
            // Read before anything else in the prepaint: it describes the
            // conditions the frame was built under, not the outcome.
            let window_active = window.is_window_active();
            let spans = {
                let mut stage = stage.borrow_mut();
                let now = Instant::now();
                UiSpans {
                    frame_gap_ms: stage
                        .prepainted_at
                        .replace(now)
                        .map_or(0.0, |last| (now - last).as_secs_f32() * 1_000.0),
                    request_to_prepaint_ms: stage
                        .requested_at
                        .map_or(0.0, |asked| (now - asked).as_secs_f32() * 1_000.0),
                    renders: std::mem::take(&mut stage.renders_since_prepaint),
                    input_events: 0,
                    input_moves: 0,
                    input_ms: 0.0,
                    input_longest_ms: 0.0,
                }
            };
            let spans = {
                let load = window.take_input_load();
                UiSpans {
                    input_events: load.events,
                    input_moves: load.mouse_moves,
                    input_ms: load.busy.as_secs_f32() * 1_000.0,
                    input_longest_ms: load.longest.as_secs_f32() * 1_000.0,
                    ..spans
                }
            };
            let scale = window.scale_factor();
            // The render size, not the element's: past the pixel budget the
            // stage renders smaller and the compositor upscales it. Everything
            // downstream — the idle key, the renderer, the trace — sees this.
            // `from_env` caches in a `OnceLock`, so the live percent cannot go
            // through it and is threaded in instead.
            let (width, height) = RenderScale::from_env().size(
                key_controls.render_scale_percent,
                key_controls.quality.pixel_budget(),
                stage.borrow().dynamic.factor(),
                (
                    (f32::from(bounds.size.width) * scale).round().max(1.0) as u32,
                    (f32::from(bounds.size.height) * scale).round().max(1.0) as u32,
                ),
            );
            // Published for the panel's caption: the budget can clamp below the
            // operator's percent, so the percent alone would mislead and this
            // is the only place the effective answer exists.
            stage.borrow_mut().render_size = Some((width, height));
            let display = display_range(window);

            let image = {
                let mut stage = stage.borrow_mut();
                let stage = &mut *stage;
                match (stage.scene.as_mut(), stage.gpu.as_mut()) {
                    (Some(scene), Some(gpu)) => {
                        let key = IdleKey {
                            time_bits: time.to_bits(),
                            camera,
                            size: (width, height),
                            controls: key_controls.clone(),
                            selected: selected_fixture_ids.clone(),
                            selected_pieces: selected_piece_ids.clone(),
                            gizmo_mode,
                            gizmo_hover,
                            universe: universe.clone(),
                            display,
                        };
                        let moving_haze = key_controls.haze.enabled
                            && key_controls.haze.density > 0.0
                            && key_controls.haze.appearance.cloudiness > 0.0
                            && (key_controls.haze.appearance.wind_speed > 0.0
                                || key_controls.haze.appearance.turbulence > 0.0);
                        // A strobe keeps flashing on a paused show, and the
                        // footage look's grain and sway never hold still.
                        let strobing = universe.as_ref().is_some_and(|universe| {
                            universe
                                .primitives
                                .values()
                                .any(|head| head.strobe > 0.0 && head.dimmer > 0.0)
                        });
                        let rest = if interacting
                            || moving_haze
                            || strobing
                            || key_controls.look.footage.enabled
                        {
                            stage.idle = None;
                            false
                        } else {
                            match &mut stage.idle {
                                Some((held, settle)) if *held == key => *settle == 0,
                                slot => {
                                    let settle = if key_controls.look.exposure.auto {
                                        EXPOSURE_SETTLE_FRAMES
                                    } else {
                                        SETTLE_FRAMES
                                    };
                                    // The frame that shows the change, and
                                    // then the frames it settles over: each
                                    // one submitted counts one off.
                                    *slot = Some((key, settle + 1));
                                    false
                                }
                            }
                        };
                        if rest {
                            // Nothing changed and the haze has converged:
                            // re-present the frame already on screen and hand
                            // the GPU nothing at all. The pause is declared to
                            // the pacing seam so the first frame after it is
                            // not spaced against the whole rest.
                            if !stage.resting {
                                stage.resting = true;
                                gpu.viewport.rest();
                                // A rate across the rest would count the rest.
                                stage.rendered = RenderedRate::default();
                            }
                            stage.previous.clone().map(|frame| (frame, None))
                        } else {
                            stage.resting = false;
                            scene.camera.position =
                                coords::three_from_world(camera.position()).to_array();
                            scene.camera.target =
                                coords::three_from_world(camera.target).to_array();
                            scene.render = key_controls.settings(camera.fov_y_deg);
                            scene.selected_fixture_ids = selected_fixture_ids.clone();
                            scene.editor = scene_desc::Editor {
                                selected_piece_ids: selected_piece_ids.clone(),
                                gizmo_piece_ids: gizmo_piece_ids.clone(),
                                gizmo: gizmo_mode,
                                gizmo_space,
                                hover: gizmo_hover,
                                build: build_affordances.clone(),
                            };
                            gpu.viewport.set_display_range(display);
                            // One frame on the GPU at a time. A second one
                            // submitted behind it does not finish sooner: the
                            // two take turns, finish together and one is
                            // thrown away. So a prepaint that finds a frame
                            // still in flight builds nothing, and the clock,
                            // the settle count and a camera export wait for
                            // the prepaint that does submit.
                            let arrived = gpu.latest().and_then(|completed| {
                                let (busy, slots) = gpu.viewport.in_flight();
                                if busy {
                                    gpu.held(slots);
                                    return Ok(completed);
                                }
                                stage.clock.submitted(clock);
                                if let Some((_, settle)) = &mut stage.idle {
                                    *settle = settle.saturating_sub(1);
                                }
                                gpu.submit(LiveFrameInputs {
                                    scene,
                                    definitions: &stage.definitions,
                                    lights: &stage.lights,
                                    strobes: &mut stage.strobes,
                                    moment,
                                    state: universe.as_ref(),
                                    lasers: &stage.lasers,
                                    size: (width, height),
                                    camera,
                                    spans,
                                    export: stage.exports.request.take().map(|request| {
                                        request.context(time, camera, (width, height), scale)
                                    }),
                                })?;
                                Ok(completed)
                            });
                            match arrived {
                                Err(error) => {
                                    // Drop the last good frame with the error. A
                                    // renderer that has stopped must not leave a
                                    // stale picture behind that looks like a live
                                    // one — the whole failure this reports is a
                                    // frozen image nobody can tell from a still
                                    // scene.
                                    stage.previous = None;
                                    stage.error = Some(error);
                                    None
                                }
                                outcome => {
                                    if let Some((serial, export)) = gpu.exported.take() {
                                        stage.exports.written(serial, &export);
                                    }
                                    // What this prepaint built: nothing,
                                    // while an earlier frame is in flight.
                                    let submitted = gpu.submission.submitted;
                                    let work = if submitted {
                                        gpu.work
                                    } else {
                                        StageWork::default()
                                    };
                                    stage.last_work = StageWork {
                                        sample_ms,
                                        ..gpu.work
                                    };
                                    let lit_cones = universe.as_ref().map_or(0, |state| {
                                        state
                                            .primitives
                                            .values()
                                            .filter(|p| p.dimmer > 0.001)
                                            .count() as u32
                                    });
                                    // Everything known before the frame was handed
                                    // over. True on a delivery and on a prepaint
                                    // that got nothing back, which is the point:
                                    // the silent ones are what a stall is made of.
                                    let mut sample = FrameSample {
                                        submitted,
                                        submitted_serial: gpu.submission.serial,
                                        haze_time_s: gpu.submission.haze_time_s,
                                        delivered: false,
                                        score_ms: sample_ms,
                                        build_ms: work.build_ms,
                                        pick_ms: work.pick_ms,
                                        camera_radius: camera.radius,
                                        // `build_frame_with` converts the scene's
                                        // Y-up camera back to this internal Z-up
                                        // space before rendering. Recording the UI
                                        // camera directly therefore matches the
                                        // `Frame` camera a replay must supply.
                                        camera_eye: camera.position().to_array(),
                                        camera_target: camera.target.to_array(),
                                        camera_fov_y_deg: camera.fov_y_deg,
                                        width,
                                        height,
                                        lit_cones,
                                        track_time_s: time,
                                        window_active,
                                        ui_frame_gap_ms: spans.frame_gap_ms,
                                        request_to_prepaint_ms: spans.request_to_prepaint_ms,
                                        renders_in_gap: spans.renders,
                                        input_events: spans.input_events,
                                        input_moves: spans.input_moves,
                                        input_ms: spans.input_ms,
                                        input_longest_ms: spans.input_longest_ms,
                                        replaced_undelivered: gpu.submission.replaced_undelivered,
                                        slots_idle: gpu.submission.slots.idle,
                                        slots_rendering: gpu.submission.slots.rendering,
                                        slots_ready: gpu.submission.slots.ready,
                                        slots_reserved: gpu.submission.slots.reserved,
                                        slot_startable: gpu.submission.slots.startable,
                                        worker_finished: gpu.submission.finished,
                                        worker_last_signalled_ms: gpu
                                            .submission
                                            .last_signalled
                                            .as_secs_f32()
                                            * 1_000.0,
                                        ..FrameSample::default()
                                    };
                                    let painted = match outcome {
                                        Ok(Some(completed)) => {
                                            stage.exports.presented(
                                                completed.serial,
                                                &completed.frame,
                                                completed.size,
                                            );
                                            stage.last_draw_ms = Some(completed.draw_ms);
                                            // A window in the background is
                                            // throttled by the system: its
                                            // frames say nothing about what
                                            // this level can hold.
                                            if window_active {
                                                stage.dynamic.observe(
                                                    stage.last_work.total_ms() + completed.draw_ms,
                                                    stage.last_gpu_ms,
                                                    key_controls.quality.frame_rate(),
                                                );
                                            }
                                            if let Some(timings) = &completed.timings {
                                                stage.last_cpu_ms =
                                                    Some(timings.cpu_encode_submit_ms as f32);
                                                stage.last_gpu_ms =
                                                    Some(timings.gpu_total_ms as f32);
                                                stage.last_cluster_ms =
                                                    Some(timings.cpu_cluster_ms as f32);
                                            }
                                            stage.last_shadow_maps =
                                                Some(completed.redrawn_shadow_maps);
                                            stage.last_present = completed.pacing;
                                            // The spans that came back with *this*
                                            // frame, not the ones measured a moment
                                            // ago on the prepaint doing the reading.
                                            sample.delivered = true;
                                            sample.presented_serial = Some(completed.serial);
                                            sample.profiled_serial = completed.timings_serial;
                                            sample.interval_ms =
                                                completed.interval_ms.unwrap_or(0.0);
                                            sample.draw_ms = completed.draw_ms;
                                            sample.queued_ms = completed.queued_ms;
                                            sample.until_signalled_ms =
                                                completed.until_signalled_ms;
                                            sample.until_noticed_ms = completed.until_noticed_ms;
                                            sample.shared_surface = completed.shared_surface;
                                            sample.redrawn_shadow_maps =
                                                completed.redrawn_shadow_maps;
                                            sample.mean_lights_per_tile =
                                                completed.clusters.mean_lights_per_tile as f32;
                                            sample.lights_on_screen =
                                                completed.clusters.lights_on_screen;
                                            sample.tile_references =
                                                completed.clusters.tile_references as u32;
                                            sample.ui_frame_gap_ms = completed.spans.frame_gap_ms;
                                            sample.request_to_prepaint_ms =
                                                completed.spans.request_to_prepaint_ms;
                                            sample.renders_in_gap = completed.spans.renders;
                                            sample.input_events = completed.spans.input_events;
                                            sample.input_moves = completed.spans.input_moves;
                                            sample.input_ms = completed.spans.input_ms;
                                            sample.input_longest_ms =
                                                completed.spans.input_longest_ms;
                                            // The worker retains the latest profile
                                            // even when its image was discarded.
                                            // `profiled_serial` identifies its source;
                                            // it is not necessarily this delivery.
                                            if let Some(timings) = &completed.timings {
                                                sample.cpu_encode_ms =
                                                    Some(timings.cpu_encode_submit_ms as f32);
                                                sample.gpu_total_ms =
                                                    Some(timings.gpu_total_ms as f32);
                                                sample.gpu_volumetric_ms =
                                                    Some(timings.gpu_volumetric_ms as f32);
                                                sample.gpu_scene_ms =
                                                    Some(timings.gpu_scene_ms as f32);
                                                sample.gpu_composite_ms =
                                                    Some(timings.gpu_composite_ms as f32);
                                                sample.cluster_ms =
                                                    Some(timings.cpu_cluster_ms as f32);
                                            }
                                            let ms = |span: std::time::Duration| {
                                                span.as_secs_f32() * 1_000.0
                                            };
                                            sample.submit_total_ms = ms(completed.cpu.total);
                                            sample.submit_prepare_ms = ms(completed.cpu.prepare);
                                            sample.submit_clusters_ms = ms(completed.cpu.clusters);
                                            sample.submit_upload_ms = ms(completed.cpu.upload);
                                            sample.submit_targets_ms = ms(completed.cpu.targets);
                                            sample.submit_encode_ms = ms(completed.cpu.encode);
                                            Some((completed.frame, Some(completed.pick)))
                                        }
                                        // Nothing back: a row with no
                                        // delivered frame.
                                        _ => stage.previous.clone().map(|frame| (frame, None)),
                                    };
                                    let now = Instant::now();
                                    stage.rendered.record(now, gpu.submission.finished);
                                    if let Some(run_up) = stage.hitches.record(sample, now) {
                                        stage.pending_hitch = Some(run_up);
                                    }
                                    painted
                                }
                            }
                        }
                    }
                    _ => None,
                }
            };
            // The viewport's own node: a script has to be able to say "drag
            // *here*", and there is no control inside it to name instead.
            agent_paint_node(Role::Card, "Stage", bounds, window, cx);
            // Where the camera is, as a reading. What a gesture did to it is
            // otherwise only visible in pixels, which makes "this drag must
            // not move the camera" a screenshot diff of a scene that has its
            // own reasons to change. Six numbers, because the orbit's two
            // angles and its distance are only half a pose: a pan moves the
            // target and nothing else, and a reading that cannot see that is
            // one a pan test would pass without moving. Together they are the
            // camera a script can project a world point through.
            agent_paint_node(
                Role::Text,
                format!(
                    "CAMERA {:.4} {:.4} {:.4} {:.4} {:.4} {:.4}",
                    camera.azimuth,
                    camera.polar,
                    camera.radius,
                    camera.target.x,
                    camera.target.y,
                    camera.target.z
                ),
                bounds,
                window,
                cx,
            );
            (image, window.insert_hitbox(bounds, HitboxBehavior::Normal))
        },
        {
            let stage = Rc::clone(&state.stage);
            let app = app.clone();
            move |bounds, (frame, hitbox): (PaintedStage, Hitbox), window, cx| {
                if let Some((frame, pick)) = frame {
                    let mut stage = stage.borrow_mut();
                    if let Some(pick) = pick {
                        stage.displayed_pick = Some(pick);
                    }
                    match &frame {
                        StageFrame::Image(image) => {
                            let already_presented = matches!(
                                &stage.previous,
                                Some(StageFrame::Image(current)) if Arc::ptr_eq(current, image)
                            );
                            if !already_presented {
                                // New pixels under the same identity, so the
                                // atlas has to be told — it would otherwise
                                // answer the paint below from its cache and
                                // show the previous frame. The tile is kept
                                // rather than dropped: it is the one allocation
                                // this viewport ever needs.
                                window.update_image(image).ok();
                            }
                            window
                                .paint_image(
                                    bounds,
                                    bounds,
                                    Corners::default(),
                                    Arc::clone(image),
                                    0,
                                    false,
                                )
                                .ok();
                        }
                        // Nothing to publish: the pixels are already in memory
                        // the compositor can address.
                        StageFrame::Shared(surface) => {
                            window.paint_upscaled_surface(bounds, surface.source());
                        }
                    }
                    stage.previous = Some(frame);
                }
                listen(&app, &hitbox, window, cx);
            }
        },
    )
    .size_full()
    .into_any_element()
}

/// Register this frame's pointer handlers.
///
/// Press and scroll are scoped to the viewport's hitbox; move and release are
/// not — a camera drag that wanders off the element, or off the window, must
/// keep tracking and must end wherever the button comes up. The same asymmetry
/// the graph canvas keeps, and for the same reason.
///
/// # What "scoped to the hitbox" leans on
///
/// gpui hit-tests in paint order and reports *every* hitbox under the pointer,
/// so this canvas is hovered even where something is drawn on top of it — the
/// settings panel, the toolbar, a seam grip. Both guards below therefore mean "no
/// surface in front of me has claimed this": `is_hovered` is false behind an
/// occluder, and `should_handle_scroll` is false behind a full one. That holds
/// only because every surface that floats over this one says so with
/// `occlude` / `block_mouse_except_scroll`. A new overlay that forgets is not
/// a bug in *this* function: pressing it would also orbit the camera, and a
/// wheel over it would also zoom.
fn listen(app: &Entity<Luma>, hitbox: &Hitbox, window: &mut Window, _cx: &mut gpui::App) {
    let pressed = app.clone();
    let inside = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !inside.is_hovered(window) {
            return;
        }
        let at = event.position;
        let shift = event.modifiers.shift;
        pressed.update(cx, |this, cx| {
            window.focus(&this.visualizer_focus, cx);
            if let Some(state) = this.visualizer_mut() {
                if event.button == MouseButton::Left {
                    state.editor_press(at, shift);
                } else if let Some(drag) = Drag::of(event.button) {
                    state.begin_drag(drag, at);
                    state.sync_pointer_lock(window);
                }
                cx.notify();
            }
        });
    });

    let dragged = app.clone();
    let over_move = hitbox.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let at = event.position;
        let held = event.pressed_button;
        let over = over_move.is_hovered(window);
        dragged.update(cx, |this, cx| {
            let mut aim = false;
            // Hovering the gizmo and dragging on the stage change only the
            // stage, so they redraw only the stage.
            let mut stage_only = false;
            {
                let Some(state) = this.visualizer_mut() else {
                    return;
                };
                // The held button is the authority on whether a drag is live.
                // Press and release bookkeeping can only ever agree with it,
                // so a stale anchor cannot turn a hover into an orbit.
                match held {
                    None => {
                        state.end_drag();
                        // Nothing pressed: the move is a hover — over the
                        // gizmo, or aiming the held ghost.
                        let hover = over.then(|| state.hover_gizmo(at)).flatten();
                        if state.gizmo_hover != hover {
                            state.gizmo_hover = hover;
                            stage_only = true;
                        }
                        aim = over
                            && !state.presentation
                            && state
                                .build
                                .as_ref()
                                .is_some_and(|build| build.hand.aims_with_pointer());
                    }
                    Some(MouseButton::Left) => {
                        if state.editor_drag.is_some() {
                            // An orbit, a marquee or a gizmo in flight: the
                            // stage's own state until the release commits it.
                            state.editor_moved(at);
                            stage_only = true;
                        }
                    }
                    Some(_) => {
                        if let Some((drag, was)) = state.drag {
                            state.drag = Some((drag, at));
                            state.dragged(at - was);
                            stage_only |= state.input_redraws();
                        }
                    }
                }
            }
            if aim {
                this.stage_aim_from_pointer(at, cx);
                cx.notify();
            } else if stage_only {
                this.notify_stage(cx);
            }
        });
    });

    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        released.update(cx, |this, cx| {
            let mut place = None;
            let mut selected = None;
            if let Some(state) = this.visualizer_mut() {
                if event.button == MouseButton::Left {
                    let selecting = state.editor_drag.is_some();
                    place = state.editor_release(event.position);
                    if selecting {
                        selected = Some(
                            state
                                .selection
                                .selected()
                                .iter()
                                .filter_map(|object| match object {
                                    EditorObject::Fixture(id) => Some(id.clone()),
                                    EditorObject::StagePiece(_) => None,
                                })
                                .collect(),
                        );
                    }
                } else {
                    state.end_drag();
                    state.sync_pointer_lock(window);
                }
                cx.notify();
            }
            if let (Some(selected), Some(Body::Patch(page))) =
                (selected, this.workspace.active_body_mut())
            {
                if page.selected != selected && !page.group_busy {
                    page.group_editor = None;
                }
                page.selected = selected;
            }
            match place {
                Some(ReleaseAct::Place(at)) => this.stage_click_room(at, cx),
                Some(ReleaseAct::CommitPose(pieces)) => this.stage_commit_pose(pieces, cx),
                None => {}
            }
        });
    });

    let zoomed = app.clone();
    let over = hitbox.clone();
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        // `should_handle_scroll`, not `is_hovered`: gpui suppresses hover for
        // the whole of a keyboard input modality, and a wheel names its own
        // position — see the note at `track_editor.rs`'s wheel listener.
        if phase != DispatchPhase::Bubble || !over.should_handle_scroll(window) {
            return;
        }
        // `pixel_delta` is what normalises a wheel's lines against a
        // trackpad's pixels; handling only one of them feels broken on the
        // other hardware.
        let wheel = f32::from(event.delta.pixel_delta(window.line_height()).y);
        zoomed.update(cx, |this, cx| {
            if let Some(state) = this.visualizer_mut() {
                state.wheel(wheel, event.position);
                this.notify_stage(cx);
            }
        });
    });
}

fn plate(message: String) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(luma_ui::caption(message.clone()))
        .agent_node(Role::Text, message)
        .into_any_element()
}
