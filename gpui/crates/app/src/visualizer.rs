//! The 3D stage view: one venue's rig, lit by the installed scene, drawn by
//! `luma-render` and composited into gpui.
//!
//! # Shape
//!
//! ```text
//! Visualizer      what venue is up, and where it is being looked at from
//!  └ Stage        the rig in the renderer's vocabulary, and the GPU that draws it
//! ```
//!
//! The split is by lifetime, not by chronology: [`Visualizer`] is per-frame
//! state the screen owns and gpui re-reads every render, while [`Stage`] holds
//! what must survive between frames and be reachable from a `'static` paint
//! closure — the device, the pipelines, the loaded meshes, the scene.
//!
//! # Where the pixels come from
//!
//! A renderer worker draws each frame off-thread and hands back a
//! [`StageFrame`]: on macOS, memory the window compositor addresses directly,
//! which `Window::paint_surface` draws where it lies; anywhere else, BGRA8 read
//! back asynchronously, published as a [`RenderImage`] under one atlas identity
//! and drawn by `Window::paint_image`. The screen does not choose between them
//! and does not know which it has — see `docs/design/presentation-seam.md`. The
//! worker's bounded slot seam drops obsolete work rather than blocking
//! prepaint.
//!
//! Frame submission happens in a [`canvas`] *prepaint*, because prepaint is the
//! first phase that knows the element's bounds. Rendering itself is off-thread;
//! prepaint only takes the newest completed frame and submits current inputs.
//!
//! # Where the light comes from
//!
//! At paint time [`Library::sample_universe`] evaluates the installed scene at
//! the transport's current time and hands back a `UniverseState`. That is the
//! whole live path: no 240 Hz timer, no JSON, no interpolation store, because
//! `eval::Scene::render` is pure in `t` and any frame can be the first frame
//! (spec §4.3). A venue with no track composited onto it draws its rig dark and
//! says so in the toolbar, which is what an unlit rig is — not a failure.
//!
//! # Three spaces meet here, and none of them is invented here
//!
//! Luma's models are Z-up *data* space. [`scene_desc::Scene`] takes rigs in
//! data space and its camera in *three* space (Y-up), because that is the space
//! the goldens were captured in. The camera itself is held in render-world
//! space by [`luma_scene::Camera`] and converted at exactly one boundary,
//! [`coords::three_from_world`].

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat3, Mat4, Quat, Vec2, Vec3};
use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Context, Corners, DispatchPhase, Div, Entity,
    Hitbox, HitboxBehavior, ImageId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, RenderImage, ScrollWheelEvent, Window,
};
use gpui_component::scroll::ScrollableElement;
use luma_lib::models::universe::UniverseState;
use luma_lib::stage_render;
use luma_render::{
    assets, build_frame_with, coords,
    frame::{EditorObject, MeshData},
    overlay::{Overlay, OverlayDepth},
    scene_desc, AsyncViewport, FrameTimings, MetricSummary, SubmitOutcome,
};
use luma_scene::{
    apply_rotation, apply_translation, bvh::MeshSource, hit_test_gizmo, selection_pivot,
    snap_angle_15, Camera, ClickOrbit, ClickOrbitRelease, ClickOrbitUpdate, Framing, GizmoHandle,
    GizmoMode, Insets, Marquee, MaterialHandle, MeshHandle, NodeContent, NodeFlags, ObjectKind,
    PivotMode, SceneGraph, Selection, SelectionTarget, Transform, TransformTarget, TriMesh, View,
    Viewfinder,
};
use luma_ui::node::{agent_paint_node, Instrument, Role};
use luma_ui::{ladder, Enabled};

use crate::library::Rig;
use crate::shell::Body;
use crate::{Library, LibraryError, Luma};

/// The three.js `<Canvas camera>` the web visualizer mounts with, in three
/// space: `position [0, 1, 3]`, target at the origin, 50° vertical field.
pub(crate) const FOV_Y_DEG: f32 = 50.0;

/// Frame shape to fit against before the viewport has been laid out once.
const DEFAULT_ASPECT: f32 = 16.0 / 9.0;

/// How far the frame-stats overlay sits in from the viewport's top edge. It is
/// a box in the corner, so it is a layout number and nothing else — see
/// [`Visualizer::view_finder`] for why it buys no camera distance.
const STATS_OVERLAY_TOP: Pixels = px(12.);
/// How far the floating toolbar sits in from the viewport's bottom edge. Read
/// by the overlay that draws it *and* by the camera fit, so the two cannot
/// drift: a rig framed to the whole pane is framed partly under this chrome.
const TOOLBAR_OVERLAY_BOTTOM: Pixels = px(16.);
/// Height of one control slab plus the hairline trim around it — the vertical
/// span the toolbar occupies, and so the band the fit keeps clear.
const OVERLAY_BAND: Pixels = px(30.);

/// `zoomSpeed={0.5}` on the web's `<OrbitControls>`, and three's own
/// `getZoomScale` base — `0.95 ** (zoomSpeed · distance · 0.01)`.
const ZOOM_SPEED: f32 = 0.5;
const ZOOM_BASE: f32 = 0.95;

/// The web toolbar's two zoom buttons are `dollyBy(0.8)` / `dollyBy(1.25)`.
const DOLLY_IN: f32 = 0.8;
const DOLLY_OUT: f32 = 1.25;

/// What the pointer is doing to the camera.
///
/// Named states rather than a button plus a flag: the three are exclusive and
/// each consumes the pointer delta differently, so a call site holding "this
/// button, but panning" could say something the camera has no answer for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    /// Left button: spherical rotation about the target.
    Orbit,
    /// Right button: slide the target across the view plane.
    Pan,
    /// Middle button: in and out along the view ray.
    Dolly,
}

/// CPU geometry and authored provenance for one submitted render frame.
/// It is immutable after submission and only becomes interactive when the
/// presentation carrying the same serial becomes the displayed image.
struct PickSnapshot {
    camera: Camera,
    graph: SceneGraph,
    meshes: Vec<Arc<TriMesh>>,
    objects: Vec<Option<(EditorObject, SelectionTarget)>>,
    canonical: HashMap<EditorObject, SelectionTarget>,
    ordered: Vec<SelectionTarget>,
    anchors: HashMap<SelectionTarget, Vec3>,
}

impl MeshSource for PickSnapshot {
    fn mesh(&self, handle: MeshHandle) -> Option<&TriMesh> {
        self.meshes.get(handle.0 as usize).map(AsRef::as_ref)
    }
}

impl PickSnapshot {
    fn from_frame(
        frame: &luma_render::Frame,
        scene: &scene_desc::Scene,
        camera: Camera,
        cache: &mut HashMap<String, Arc<TriMesh>>,
    ) -> Self {
        let meshes = frame
            .meshes
            .iter()
            .map(|mesh| {
                Arc::clone(cache.entry(mesh.key.clone()).or_insert_with(|| {
                    Arc::new(TriMesh::new(
                        mesh.vertices
                            .iter()
                            .map(|vertex| Vec3::from(vertex.position))
                            .collect(),
                        mesh.indices
                            .chunks_exact(3)
                            .map(|tri| [tri[0], tri[1], tri[2]])
                            .collect(),
                    ))
                }))
            })
            .collect();
        let mut graph = SceneGraph::new();
        let mut objects = Vec::new();
        let mut canonical = HashMap::new();
        let mut anchors = HashMap::new();
        let mut ordered = Vec::new();
        for draw in &frame.draws[..frame.draws.len().saturating_sub(frame.grid_draws)] {
            let Some(object) = draw.editor_object.clone() else {
                continue;
            };
            let (scale, rotation, translation) = draw.model.to_scale_rotation_translation();
            let node = graph.insert(
                None,
                Transform {
                    translation,
                    rotation,
                    scale,
                },
                NodeContent::Mesh {
                    mesh: MeshHandle(draw.mesh as u32),
                    material: MaterialHandle(0),
                },
                NodeFlags::DEFAULT,
            );
            let kind = match object {
                EditorObject::Fixture(_) => ObjectKind::Fixture,
                EditorObject::StagePiece(_) => ObjectKind::StagePiece,
            };
            let canonical_target = *canonical.entry(object.clone()).or_insert_with(|| {
                let target = SelectionTarget::new(kind, node);
                ordered.push(target);
                target
            });
            if canonical_target.node == node {
                anchors.insert(
                    canonical_target,
                    object_pose(scene, &object).map_or(translation, |pose| pose.anchor),
                );
            }
            if objects.len() <= node.0 as usize {
                objects.resize(node.0 as usize + 1, None);
            }
            objects[node.0 as usize] = Some((object, canonical_target));
        }
        graph.update_world_transforms();
        Self {
            camera,
            graph,
            meshes,
            objects,
            canonical,
            ordered,
            anchors,
        }
    }

    fn ray(&self, at: Vec2, viewport: Vec2) -> luma_scene::Ray {
        let ndc = Vec2::new(
            at.x / viewport.x.max(1.0) * 2.0 - 1.0,
            1.0 - at.y / viewport.y.max(1.0) * 2.0,
        );
        self.camera.ray(ndc, viewport.x / viewport.y.max(1.0))
    }

    fn pick(&self, at: Vec2, viewport: Vec2) -> Option<SelectionTarget> {
        self.graph
            .raycast(self.ray(at, viewport), Default::default(), self)
            .into_iter()
            .find_map(|hit| self.objects.get(hit.node.0 as usize)?.as_ref().map(|v| v.1))
    }

    fn object(&self, target: SelectionTarget) -> Option<&EditorObject> {
        self.canonical
            .iter()
            .find_map(|(object, candidate)| (*candidate == target).then_some(object))
    }

    fn marquee(&self, marquee: Marquee, viewport: Vec2) -> Vec<SelectionTarget> {
        self.ordered
            .iter()
            .filter(|target| {
                self.anchors
                    .get(target)
                    .is_some_and(|anchor| marquee.contains_world(&self.camera, viewport, *anchor))
            })
            .copied()
            .collect()
    }
}

struct SerialPairing<T> {
    pending: BTreeMap<u64, T>,
}

impl<T> Default for SerialPairing<T> {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }
}

impl<T> SerialPairing<T> {
    fn submitted(&mut self, serial: u64, snapshot: T, outcome: SubmitOutcome) {
        if let SubmitOutcome::Replaced { dropped_serial } = outcome {
            self.pending.remove(&dropped_serial);
        }
        self.pending.insert(serial, snapshot);
    }

    fn presented(&mut self, serial: u64) -> Option<T> {
        let snapshot = self.pending.remove(&serial);
        self.pending.retain(|candidate, _| *candidate > serial);
        snapshot
    }
}

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
struct UiSpans {
    /// Wall time since the stage's previous prepaint.
    frame_gap_ms: f32,
    /// `request_animation_frame` to this frame's prepaint.
    request_to_prepaint_ms: f32,
    /// How many times the stage's `render` ran since the previous prepaint.
    ///
    /// One is the healthy pairing. **Zero over a long gap is the whole reason
    /// this field exists**: it separates a UI thread that was busy elsewhere
    /// from one that was never asked for a frame at all, and those have
    /// different fixes and different owners.
    renders: u32,
}

/// The app's own record of a submitted frame, returned when it is presented.
struct SubmittedFrame {
    pick: PickSnapshot,
    spans: UiSpans,
}

type PickTimeline = SerialPairing<SubmittedFrame>;

impl Drag {
    /// three's `OrbitControls` defaults: LEFT rotate, MIDDLE dolly, RIGHT pan.
    fn of(button: MouseButton) -> Option<Self> {
        match button {
            MouseButton::Left => Some(Self::Orbit),
            MouseButton::Right => Some(Self::Pan),
            MouseButton::Middle => Some(Self::Dolly),
            _ => None,
        }
    }
}

/// Why the viewport is not showing a lit rig.
enum Status {
    Loading,
    /// Drawing. `lit` is the difference between a rig that is dark because the
    /// cue says so and a rig that is dark because nothing is composited onto
    /// it — two identical pictures with different causes.
    Live {
        lit: bool,
    },
    /// No GPU, a venue with nothing patched, or a load that failed. Shown
    /// verbatim.
    Empty(String),
}

/// The sprite-atlas identity every stage frame is published under.
///
/// One identity for the life of the process, deliberately. The tile it names is
/// refreshed in place ([`Window::update_image`]) rather than reinserted, so a
/// fresh id per frame would create and destroy a full-screen texture at frame
/// rate — and a fresh id per *stage* would strand the old tile, because a
/// dropped stage has no window to remove it with. The stage pane is a
/// singleton, so one identity covers every stage that will ever be on screen.
static STAGE_IMAGE_ID: std::sync::OnceLock<ImageId> = std::sync::OnceLock::new();

/// The 3D view's screen state.
pub(crate) struct Visualizer {
    /// Which room this stage is showing. The pane is derived from the active
    /// tab's subject venue every frame, and this is what that derivation is
    /// compared against — a stage already showing the right room is kept, GPU
    /// and all, rather than rebuilt because the eye moved between two tabs.
    venue_id: String,
    venue_name: String,
    /// The `(track, venue)` whose score is lighting the rig, when one is.
    /// Compared alongside [`Self::venue_id`] so switching tracks within one
    /// room re-composites instead of tearing the stage down.
    subject: Option<(String, String)>,
    /// Whether this stage may build a renderer at all — see
    /// [`stage_gpu_enabled`]. Captured once, when the stage is built, so the
    /// answer cannot change under a running viewport.
    gpu_enabled: bool,
    status: Status,
    camera: Camera,
    /// The rig's extent, which is what the camera is framed and clamped
    /// against. Set when the rig lands; [`Framing::default`] until then.
    framing: Framing,
    /// Set when a rig lands, cleared by the first prepaint that knows the
    /// viewport's shape. The opening pose is fitted to the *frame*, and
    /// nothing knows the frame's aspect until it has been laid out once — so
    /// framing is owed rather than done, and a rig that loads before the first
    /// layout still opens fitted instead of guessed.
    owes_opening_pose: bool,
    /// The button held, and where the pointer last was — `MouseMoveEvent`
    /// carries no delta.
    drag: Option<(Drag, Point<Pixels>)>,
    editor_drag: Option<EditorDrag>,
    selection: Selection,
    gizmo_mode: GizmoMode,
    /// The element's size as the last frame laid it out. Three's orbit rates
    /// are all per element *height*, and a rate needs the height a frame
    /// earlier than prepaint can supply it.
    size: gpui::Size<Pixels>,
    viewport_origin: Point<Pixels>,
    render_lab: RenderLab,
    /// Whether the FPS readout is unfolded into the full frame-stats panel.
    fps_expanded: bool,
    stage: Rc<RefCell<Stage>>,
}

/// Everything that outlives a frame and must be reachable from a `'static`
/// paint closure.
#[derive(Default)]
struct Stage {
    /// The rig. `None` until the load lands.
    scene: Option<scene_desc::Scene>,
    definitions: BTreeMap<String, scene_desc::Definition>,
    /// Acquired lazily on the first frame that has something to draw — see
    /// [`Visualizer::gpu_ready`].
    gpu: Option<Gpu>,
    /// What the previous frame painted, released only once the next one is on
    /// screen: an atlas entry dropped early leaves the view blank for a frame,
    /// and a shared surface released early is memory the compositor is still
    /// reading.
    previous: Option<StageFrame>,
    /// Hit-test world paired to `previous` by AsyncViewport serial.
    displayed_pick: Option<PickSnapshot>,
    /// Why the last frame did not draw. Written by the paint closure, which
    /// has no screen to put a message on, and read by the next `render`, which
    /// does — a viewport that failed silently is a black rectangle with no
    /// account of itself.
    error: Option<String>,
    /// End-to-end renderer wall time from the previous completed frame.
    last_draw_ms: Option<f32>,
    /// CPU scene encoding and queue submission time from the previous frame.
    last_cpu_ms: Option<f32>,
    /// GPU scene-through-composite time from the previous frame.
    last_gpu_ms: Option<f32>,
    /// Cold cluster rebuild time; zero on topology cache hits.
    last_cluster_ms: Option<f32>,
    /// Fixture shadow maps redrawn by the previous frame. Zero is the healthy
    /// steady state; a sustained non-zero run is tenancy churning.
    last_shadow_maps: Option<u32>,
    /// When the stage last asked for another frame, and when its prepaint last
    /// ran. Together these separate the two ways a frame can be late: the UI
    /// thread never ran one, or it ran one that took too long to reach the
    /// screen. Present-interval alone cannot tell those apart.
    requested_at: Option<Instant>,
    prepainted_at: Option<Instant>,
    /// Renders since the last prepaint — see [`UiSpans::renders`].
    renders_since_prepaint: u32,
    /// Rolling spacing of presented frames. Read against `last_gpu_ms` this is
    /// what separates a stage nobody is asking to repaint from one the GPU
    /// cannot keep up with; either alone is unattributable.
    last_present: Option<MetricSummary>,
    /// What the previous frame cost the UI thread, by phase.
    last_work: StageWork,
    /// The last few seconds of frames, and the report a hitch leaves behind.
    hitches: HitchRing,
    /// A hitch report waiting for a caller that can reach the library. The
    /// paint closure notices the hitch but has no `Library`; `body` has one and
    /// runs every frame, so it drains this.
    pending_hitch: Option<Vec<FrameSample>>,
    /// The inputs of the last submitted frame, with the settle countdown the
    /// temporal haze still needs on them; `None` while inputs keep changing.
    idle: Option<(IdleKey, u32)>,
    /// True while the idle gate is skipping submissions: the settled frame is
    /// on screen and the renderer is doing nothing at all.
    resting: bool,
    /// The size frames are drawn at, which lags the size the element occupies
    /// for as long as that size keeps moving.
    rendered_size: RenderSize,
}

/// The size the stage renders at, held still while its element is resizing.
///
/// # Why this is not simply the element's size
///
/// Every distinct size costs the renderer a full reallocation — seven
/// textures, [`luma_render::viewport::PRESENTATION_SLOTS`] presentation
/// surfaces — and resets the temporal haze history, whose accumulation is what
/// makes a lit frame affordable. That is the right trade for a size that
/// changed once. It is the wrong trade for a size that is *animating*: a ⌘B
/// slides the sidebar over [`luma_ui::motion::SWEEP`] and takes its width out
/// of the panel beside it, so the stage is handed a width it has never seen on
/// every frame of the slide, and pays that reallocation ~32 times for a picture
/// nobody is looking at yet. Measured at 2560x1440 with a 120-fixture rig, that
/// is a renderer frame of 16.2 ms median / 31.7 ms p95 against 6.5 ms held
/// still — the whole of the reported "⌘B is not 120 Hz".
///
/// So the stage keeps drawing at the size it already has and lets the paint
/// scale that picture into the element's live bounds, adopting the new size
/// once the layout stops moving. This is the same trade [`luma_ui::pane::pane`]
/// already makes one layer up — a sliding panel lays its content out at the
/// destination width for the whole slide rather than re-wrapping it forty times
/// on the way in — and it introduces no new visual mode: a resting stage
/// already re-presents one frame into changing bounds.
///
/// The rule is a size, not a gesture: a window-resize drag, a seam drag and a
/// ⌘B all produce the same stream of never-repeating sizes and all want the
/// same answer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RenderSize {
    /// What frames are being drawn at. `None` before the first laid-out frame,
    /// which adopts immediately — there is no picture to hold on to.
    current: Option<(u32, u32)>,
    /// A size seen but not yet adopted, and how many more frames it must
    /// survive unchanged. An animating size restarts this every frame and so
    /// never arrives.
    pending: Option<((u32, u32), u32)>,
}

/// Frames a new size must repeat before the stage redraws at it.
///
/// Three, not one. An eased tween is slowest at its ends, so short runs of an
/// unchanged rounded size are reachable *within* a slide; the count is what
/// decides how long a run has to be before it is believed. No count makes that
/// impossible — a slide can always crawl — so the guarantee this offers is a
/// **bound**, not zero: a gesture costs the renderer one or two reallocations
/// instead of one per frame, and the ones it does cost land within a pixel or
/// two of where the slide was going anyway. Three frames of holding is 25 ms at
/// display rate, which is what a settled resize waits before it is honoured.
const SIZE_HOLD_FRAMES: u32 = 3;

impl RenderSize {
    /// Observe this frame's laid-out size and answer with the size to draw at.
    fn settle(&mut self, observed: (u32, u32)) -> (u32, u32) {
        let Some(current) = self.current else {
            self.current = Some(observed);
            return observed;
        };
        if observed == current {
            self.pending = None;
            return current;
        }
        let remaining = match self.pending {
            Some((held, remaining)) if held == observed => remaining.saturating_sub(1),
            _ => SIZE_HOLD_FRAMES - 1,
        };
        if remaining == 0 {
            self.pending = None;
            self.current = Some(observed);
            return observed;
        }
        self.pending = Some((observed, remaining));
        current
    }
}

/// Everything a live frame is a function of.
///
/// Two prepaints with equal keys ask for the same picture, so once the
/// temporal haze has settled on one there is nothing left to render — the
/// stage's prepaint skips the submission and re-presents the frame already on
/// screen, which is what lets a still, paused stage cost zero GPU instead of
/// re-marching the haze at display rate for nobody. The scene's own geometry
/// is deliberately absent: this screen only mutates it during a pointer drag,
/// and a drag holds the gate open (see `interacting` in the prepaint).
#[derive(PartialEq)]
struct IdleKey {
    /// `f32::to_bits` — an equality key wants exactness, not float semantics.
    time_bits: u32,
    camera: Camera,
    size: (u32, u32),
    /// The whole lab, its `open` flag normalised out — panel visibility draws
    /// nothing.
    lab: RenderLab,
    selected: Vec<String>,
    gizmo_mode: GizmoMode,
    gizmo_pivot: Option<Vec3>,
    generic_translate: bool,
    universe: Option<UniverseState>,
}

/// Frames of unchanged inputs the temporal haze needs before its blue-noise
/// integration is visually converged and the stage may rest.
const SETTLE_FRAMES: u32 = 16;

enum EditorDrag {
    ClickOrbit {
        gesture: ClickOrbit,
        shift: bool,
        start: Vec2,
    },
    Marquee(Marquee),
    Gizmo {
        handle: GizmoHandle,
        start: Vec2,
        originals: Vec<(EditorObject, Vec3, Quat)>,
    },
}

/// Runtime renderer controls. They belong to the viewport, not persisted venue
/// data, so experimentation cannot silently rewrite a show.
///
/// `PartialEq` because the whole lab rides inside [`IdleKey`]: a curated
/// field list there would silently miss every dial added later.
#[derive(Clone, PartialEq)]
struct RenderLab {
    open: bool,
    sun_enabled: bool,
    sun_azimuth_deg: f32,
    sun_elevation_deg: f32,
    sun_intensity: f32,
    sun_color: [f32; 3],
    sun_shadows: bool,
    sun_shadow_softness: f32,
    environment_enabled: bool,
    background_color: [f32; 3],
    ambient_color: [f32; 3],
    ambient_intensity: f32,
    probe_enabled: bool,
    probe_intensity: f32,
    probe_rotation_deg: f32,
    probe_visible: bool,
    fixture_surface_lighting: bool,
    fixture_shadows: bool,
    cluster_debug: bool,
    haze_enabled: bool,
    haze_density: f32,
    haze_steps: u32,
    haze_resolution: f32,
    grid_enabled: bool,
    debug_view: scene_desc::DebugView,
}

impl RenderLab {
    fn new(editor_lit: bool) -> Self {
        Self {
            open: false,
            sun_enabled: editor_lit,
            sun_azimuth_deg: -36.87,
            sun_elevation_deg: 50.19,
            sun_intensity: 1.4,
            sun_color: [1.0; 3],
            sun_shadows: true,
            sun_shadow_softness: 1.0,
            environment_enabled: editor_lit,
            background_color: scene_desc::Environment::EDITOR.background,
            ambient_color: scene_desc::Environment::EDITOR.ambient_color,
            ambient_intensity: if editor_lit { 0.2 } else { 0.0 },
            probe_enabled: false,
            probe_intensity: 0.8,
            probe_rotation_deg: 0.0,
            probe_visible: false,
            fixture_surface_lighting: true,
            fixture_shadows: true,
            cluster_debug: false,
            haze_enabled: true,
            haze_density: 0.8,
            haze_steps: 8,
            haze_resolution: luma_render::LIVE_HAZE_RESOLUTION,
            grid_enabled: editor_lit,
            debug_view: scene_desc::DebugView::Pbr,
        }
    }

    /// Re-apply the editor/live lighting choice after the stage's subject
    /// changed under it.
    ///
    /// Exactly the fields [`Self::new`] derives from the same flag, and no
    /// others: everything else in the lab is an authored tweak, and a stage
    /// that reset a hand-set haze density because the track changed would be
    /// throwing away work nobody asked it to.
    fn set_editor_lit(&mut self, editor_lit: bool) {
        self.sun_enabled = editor_lit;
        self.environment_enabled = editor_lit;
        self.ambient_intensity = if editor_lit { 0.2 } else { 0.0 };
        self.grid_enabled = editor_lit;
    }

    fn sun_direction(&self) -> [f32; 3] {
        let azimuth = self.sun_azimuth_deg.to_radians();
        let elevation = self.sun_elevation_deg.to_radians();
        let horizontal = elevation.cos();
        [
            horizontal * azimuth.cos(),
            horizontal * azimuth.sin(),
            elevation.sin(),
        ]
    }

    fn cycle_debug_view(&mut self) {
        self.debug_view = match self.debug_view {
            scene_desc::DebugView::Pbr => scene_desc::DebugView::BaseColor,
            scene_desc::DebugView::BaseColor => scene_desc::DebugView::Normals,
            scene_desc::DebugView::Normals => scene_desc::DebugView::Metallic,
            scene_desc::DebugView::Metallic => scene_desc::DebugView::Roughness,
            scene_desc::DebugView::Roughness => scene_desc::DebugView::Shadow,
            scene_desc::DebugView::Shadow => scene_desc::DebugView::Depth,
            scene_desc::DebugView::Depth => scene_desc::DebugView::VolumetricAccumulation,
            scene_desc::DebugView::VolumetricAccumulation => scene_desc::DebugView::Pbr,
        };
    }

    fn debug_label(&self) -> &'static str {
        match self.debug_view {
            scene_desc::DebugView::Pbr => "PBR",
            scene_desc::DebugView::BaseColor => "Base color",
            scene_desc::DebugView::Normals => "Normals",
            scene_desc::DebugView::Metallic => "Metallic",
            scene_desc::DebugView::Roughness => "Roughness",
            scene_desc::DebugView::Shadow => "Shadow",
            scene_desc::DebugView::Depth => "Depth",
            scene_desc::DebugView::VolumetricAccumulation => "Volume accumulation",
        }
    }
}

#[derive(Clone, Copy)]
enum LabToggle {
    Sun,
    Shadows,
    Environment,
    Probe,
    ProbeVisible,
    FixtureSurfaceLighting,
    FixtureShadows,
    ClusterDebug,
    Haze,
    Grid,
}

#[derive(Clone, Copy)]
enum LabValue {
    Azimuth,
    Elevation,
    SunIntensity,
    SunColor(usize),
    SunShadowSoftness,
    BackgroundColor(usize),
    AmbientColor(usize),
    Ambient,
    ProbeIntensity,
    ProbeRotation,
    HazeDensity,
    HazeSteps,
    HazeResolution,
}

impl RenderLab {
    fn toggle(&mut self, control: LabToggle) {
        let value = match control {
            LabToggle::Sun => &mut self.sun_enabled,
            LabToggle::Shadows => &mut self.sun_shadows,
            LabToggle::Environment => &mut self.environment_enabled,
            LabToggle::Probe => &mut self.probe_enabled,
            LabToggle::ProbeVisible => &mut self.probe_visible,
            LabToggle::FixtureSurfaceLighting => &mut self.fixture_surface_lighting,
            LabToggle::FixtureShadows => &mut self.fixture_shadows,
            LabToggle::ClusterDebug => &mut self.cluster_debug,
            LabToggle::Haze => &mut self.haze_enabled,
            LabToggle::Grid => &mut self.grid_enabled,
        };
        *value = !*value;
    }

    /// Put a control at `value`, bounded — the one statement of every lab
    /// control's range.
    ///
    /// Absolute rather than a delta because the slider that drives it is a
    /// position (see [`luma_ui::luma_slider`]): a control asked for a value it
    /// cannot hold lands on its nearest legal one and stops there, where a
    /// stream of deltas would bank the overshoot and pay it back on the way
    /// out. The two angles wrap instead of clamping, and the one control that
    /// counts in whole steps rounds; those are properties of the parameter, so
    /// they live with the bound rather than beside the caller.
    fn set(&mut self, control: LabValue, value: f32) {
        match control {
            LabValue::Azimuth => {
                self.sun_azimuth_deg = (value + 180.0).rem_euclid(360.0) - 180.0;
            }
            LabValue::Elevation => {
                self.sun_elevation_deg = value.clamp(-85.0, 85.0);
            }
            LabValue::SunIntensity => {
                self.sun_intensity = value.clamp(0.0, 10.0);
            }
            LabValue::SunColor(channel) => {
                self.sun_color[channel] = value.clamp(0.0, 1.0);
            }
            LabValue::SunShadowSoftness => {
                self.sun_shadow_softness = value.clamp(0.0, 3.0);
            }
            LabValue::BackgroundColor(channel) => {
                self.background_color[channel] = value.clamp(0.0, 1.0);
            }
            LabValue::AmbientColor(channel) => {
                self.ambient_color[channel] = value.clamp(0.0, 1.0);
            }
            LabValue::Ambient => {
                self.ambient_intensity = value.clamp(0.0, 2.0);
            }
            LabValue::ProbeIntensity => {
                self.probe_intensity = value.clamp(0.0, 4.0);
            }
            LabValue::ProbeRotation => {
                self.probe_rotation_deg = (value + 180.0).rem_euclid(360.0) - 180.0;
            }
            LabValue::HazeDensity => {
                self.haze_density = value.clamp(0.0, 2.0);
            }
            LabValue::HazeSteps => {
                self.haze_steps = (value.round() as u32).clamp(1, 64);
            }
            LabValue::HazeResolution => {
                self.haze_resolution = value.clamp(0.25, 1.0);
            }
        }
    }
}

impl Visualizer {
    /// Open the view on a venue and start its load.
    ///
    /// `subject` is the `(track, venue)` whose score should light the rig, when
    /// the view was opened over a track. Compositing it is a dispatch command
    /// like any other; what is *not* a command is the per-frame sample that
    /// follows — see [`Library::sample_universe`].
    pub(crate) fn open(
        library: &Library,
        venue_id: &str,
        venue_name: String,
        subject: Option<(String, String)>,
        cx: &mut Context<Luma>,
    ) -> Self {
        let rig = library.venue_rig(venue_id);
        let editor_lit = subject.is_none();
        let composite = subject
            .clone()
            .map(|(track, venue)| library.composite_track(&track, &venue, None));
        let venue = venue_id.to_string();
        cx.spawn(async move |this, cx| {
            // The composite first: it is what makes the sample non-empty, and
            // a rig that appeared before its light would flash dark.
            if let Some(composite) = composite {
                composite.await.ok();
            }
            let loaded = rig.await;
            this.update(cx, |this, cx| {
                // Addressed to the room, not merely to whatever stage is up: a
                // rig landing after the eye moved to another venue must not
                // paint into the stage that replaced it.
                let Some(state) = this.visualizer.as_mut().filter(|it| it.venue_id == venue) else {
                    return;
                };
                state.rig_loaded(loaded);
                cx.notify();
            })
            .ok();
        })
        .detach();

        Self {
            venue_id: venue_id.to_string(),
            venue_name,
            subject,
            gpu_enabled: stage_gpu_enabled(),
            status: Status::Loading,
            camera: opening_camera(
                &Framing::default(),
                &Viewfinder::new(FOV_Y_DEG, DEFAULT_ASPECT),
            ),
            framing: Framing::default(),
            owes_opening_pose: false,
            drag: None,
            editor_drag: None,
            selection: Selection::default(),
            gizmo_mode: GizmoMode::Translate,
            size: gpui::Size::default(),
            viewport_origin: Point::default(),
            render_lab: RenderLab::new(editor_lit),
            fps_expanded: false,
            stage: Rc::default(),
        }
    }

    /// Light this stage with a different score, without rebuilding it.
    ///
    /// Switching tracks inside one room changes only *what is composited* —
    /// the rig, the camera framing and the GPU are all still about the same
    /// venue. Tearing the stage down to re-light it would drop the device and
    /// re-frame the camera, so a glance between two tracks would restart the
    /// view. Compositing is a dispatch command like any other; the per-frame
    /// sample that follows is not (see [`Library::sample_universe`]).
    fn relight(
        &mut self,
        library: &Library,
        subject: Option<(String, String)>,
        cx: &mut Context<Luma>,
    ) {
        self.subject = subject.clone();
        self.render_lab.set_editor_lit(subject.is_none());
        let Some((track, venue)) = subject else {
            return;
        };
        let composite = library.composite_track(&track, &venue, None);
        cx.spawn(async move |_, _| {
            composite.await.ok();
        })
        .detach();
    }

    fn rig_loaded(&mut self, loaded: Result<Rig, LibraryError>) {
        let rig = match loaded {
            Ok(rig) => rig,
            Err(error) => {
                self.status = Status::Empty(error.to_string());
                return;
            }
        };
        if rig.fixtures.is_empty() && rig.pieces.is_empty() {
            self.status = Status::Empty(format!("{} has nothing patched", self.venue_name));
            return;
        }
        let definitions: BTreeMap<_, _> = rig
            .definitions
            .iter()
            .map(|(path, def)| (path.clone(), stage_render::definition(def)))
            .collect();
        let scene = scene(&rig, &definitions);
        self.framing = scene.framing(&definitions);
        self.camera = opening_camera(&self.framing, &self.view_finder());
        self.owes_opening_pose = true;
        let mut stage = self.stage.borrow_mut();
        stage.definitions = definitions;
        stage.scene = Some(scene);
        drop(stage);
        self.status = Status::Live { lit: false };
    }

    /// How many fixtures are being drawn, for the toolbar's readout.
    fn fixture_count(&self) -> usize {
        self.stage
            .borrow()
            .scene
            .as_ref()
            .map_or(0, |s| s.fixtures.len())
    }

    /// Start the renderer worker once, on the first frame with something to draw.
    ///
    /// GPU acquisition itself happens on that worker. A failure is returned as
    /// an asynchronous frame result, so opening the screen never blocks the UI.
    fn gpu_ready(&mut self) -> bool {
        if !self.gpu_enabled {
            return false;
        }
        if self.stage.borrow().gpu.is_some() {
            return true;
        }
        self.stage.borrow_mut().gpu = Some(Gpu {
            viewport: AsyncViewport::new(),
            work: StageWork::default(),
            submission: Submission::default(),
            assets: assets::Library::new(stage_render::meshes_root(None)),
            picks: PickTimeline::default(),
            pick_meshes: HashMap::new(),
        });
        true
    }

    /// Dolly by a factor: the web toolbar's one zoom verb, and what both the
    /// wheel and the middle-button drag reduce to.
    ///
    /// The near bound is the rig's own extent: a camera closer than that is
    /// inside the beams, where every pixel is one saturated colour.
    /// Dolly all the way in, for the launch-time reproduction driver.
    ///
    /// Steps rather than a target radius: the near bound is
    /// [`Framing::radius_bounds`] and only the camera knows it, so repeating
    /// the same gesture the operator makes is both simpler and more faithful
    /// than computing where they would have ended up.
    pub(crate) fn dolly_in(&mut self, steps: usize) {
        for _ in 0..steps {
            self.dolly(DOLLY_IN);
        }
    }

    /// The frame every camera in this viewport is fitted to: its shape, and the
    /// band of it the floating chrome covers.
    ///
    /// Only chrome that *spans* an edge earns an inset, because that is what an
    /// inset claims — this band of the frame is covered. The toolbar's row runs
    /// the full width and is centred on exactly where a fitted rig's floor
    /// lands, so it does. The frame-stats readout is a box in the top-left
    /// corner and does not: reserving the whole top band for it cost 19% of the
    /// pane's height in the 943×220 viewport the pixel suite opens, which with
    /// the toolbar's own band left the fit 55% of the frame to work in and drew
    /// the rig at half size. Chrome that floats in a corner frames as
    /// background.
    ///
    /// Before the first layout there is no shape to read, so a landscape
    /// default stands in and [`Visualizer::owes_opening_pose`] re-fits once
    /// there is one.
    fn view_finder(&self) -> Viewfinder {
        let (w, h) = (f32::from(self.size.width), f32::from(self.size.height));
        if w <= 0.0 || h <= 0.0 {
            return Viewfinder::new(FOV_Y_DEG, DEFAULT_ASPECT);
        }
        let toolbar = (f32::from(TOOLBAR_OVERLAY_BOTTOM) + f32::from(OVERLAY_BAND)) / h;
        Viewfinder::new(FOV_Y_DEG, w / h).inset(Insets::vertical(0.0, toolbar))
    }

    fn dolly(&mut self, factor: f32) {
        let (near, far) = self
            .framing
            .radius_bounds(opening_camera(&self.framing, &self.view_finder()).radius);
        self.camera.radius = (self.camera.radius * factor).clamp(near, far);
    }

    /// Consume one pointer step, in logical pixels.
    fn dragged(&mut self, delta: Point<Pixels>) {
        let Some((drag, _)) = self.drag else { return };
        // Every one of three's rates is per element height, so a drag across a
        // tall panel turns the camera as far as the same drag across a short one.
        let height = f32::from(self.size.height).max(1.0);
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        match drag {
            // `rotateLeft(2π·dx/H)` and `rotateUp(2π·dy/H)`, both of which
            // *subtract* from the spherical angle. Our azimuth is three's theta
            // less a quarter turn and our polar is its phi exactly — both are
            // `world_from_three` of the same point — so the deltas carry over
            // unchanged and only the parameterisation differs.
            Drag::Orbit => {
                let turn = std::f32::consts::TAU / height;
                self.camera.azimuth -= turn * dx;
                // Three lets phi run the full half-turn, which on a stage means
                // orbiting under the floor and out the other side. Clamped to
                // the quadrant that can actually see a rig.
                self.camera.polar = Framing::clamp_polar(self.camera.polar - turn * dy);
            }
            // three's perspective pan: one screen height of drag moves the
            // target by the full visible extent at the target's depth, so a
            // point under the cursor stays under it.
            Drag::Pan => {
                let extent = 2.0 * self.camera.radius * (FOV_Y_DEG.to_radians() / 2.0).tan();
                let forward = (self.camera.target - self.camera.position()).normalize();
                let right = forward.cross(Vec3::Z).normalize();
                let up = right.cross(forward);
                self.camera.target += (right * -dx + up * dy) * (extent / height);
            }
            Drag::Dolly => self.dolly(zoom_scale(-dy)),
        }
    }

    fn viewport_point(&self, point: Point<Pixels>) -> Vec2 {
        Vec2::new(
            f32::from(point.x - self.viewport_origin.x),
            f32::from(point.y - self.viewport_origin.y),
        )
    }

    fn viewport_size(&self) -> Vec2 {
        Vec2::new(f32::from(self.size.width), f32::from(self.size.height))
    }

    fn editor_press(&mut self, point: Point<Pixels>, shift: bool) {
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        let stage = self.stage.borrow();
        let Some(pick) = stage.displayed_pick.as_ref() else {
            self.editor_drag = Some(EditorDrag::ClickOrbit {
                gesture: ClickOrbit::new(at),
                shift,
                start: at,
            });
            return;
        };
        if let Some(primary) = self.selection.primary() {
            if let Some(&pivot) = pick.anchors.get(&primary) {
                let distance = (pick.camera.position() - pivot).length();
                let scale = distance
                    * (1.9 * (pick.camera.fov_y_deg.to_radians() / 2.0).tan()).min(7.0)
                    * 0.5
                    / 7.0;
                if let Some(hit) = hit_test_gizmo(
                    pick.ray(at, viewport),
                    pivot,
                    scale,
                    pick.camera.position() - pivot,
                    self.gizmo_mode,
                ) {
                    let originals = self
                        .selection
                        .selected()
                        .iter()
                        .filter_map(|target| {
                            let object = pick.object(*target)?.clone();
                            let pose = stage
                                .scene
                                .as_ref()
                                .and_then(|scene| object_pose(scene, &object))?;
                            Some((object, pose.position, pose.rotation))
                        })
                        .collect();
                    self.editor_drag = Some(EditorDrag::Gizmo {
                        handle: hit.handle,
                        start: at,
                        originals,
                    });
                    return;
                }
            }
        }
        self.editor_drag = Some(EditorDrag::ClickOrbit {
            gesture: ClickOrbit::new(at),
            shift,
            start: at,
        });
    }

    fn editor_moved(&mut self, point: Point<Pixels>) {
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        let Some(mut interaction) = self.editor_drag.take() else {
            return;
        };
        match &mut interaction {
            EditorDrag::ClickOrbit {
                gesture,
                shift,
                start,
            } => {
                if *shift {
                    let mut marquee = Marquee::new(*start);
                    marquee.moved(at);
                    if marquee.qualifies() {
                        interaction = EditorDrag::Marquee(marquee);
                    }
                } else {
                    match gesture.moved(at) {
                        ClickOrbitUpdate::Pending => {}
                        ClickOrbitUpdate::BeginOrbit(delta) | ClickOrbitUpdate::Orbit(delta) => {
                            self.drag = Some((Drag::Orbit, point));
                            self.dragged(Point::new(px(delta.x), px(delta.y)));
                        }
                    }
                }
            }
            EditorDrag::Marquee(marquee) => marquee.moved(at),
            EditorDrag::Gizmo {
                handle,
                start,
                originals,
            } => self.apply_gizmo(*handle, at - *start, viewport, originals),
        }
        self.editor_drag = Some(interaction);
    }

    fn editor_release(&mut self, point: Point<Pixels>) {
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        // A release ends the camera drag whatever else this function decides:
        // the selection paths below can bail before reaching the end.
        self.drag = None;
        let Some(interaction) = self.editor_drag.take() else {
            return;
        };
        let stage = self.stage.borrow();
        let Some(pick) = stage.displayed_pick.as_ref() else {
            return;
        };
        match interaction {
            EditorDrag::ClickOrbit { gesture, shift, .. }
                if gesture.released() == ClickOrbitRelease::Click =>
            {
                if let Some(target) = pick.pick(at, viewport) {
                    self.selection.click(target, shift);
                } else if !shift {
                    self.selection.clear();
                }
            }
            EditorDrag::Marquee(marquee) => self.selection.replace(pick.marquee(marquee, viewport)),
            _ => {}
        }
    }

    fn apply_gizmo(
        &mut self,
        handle: GizmoHandle,
        pixels: Vec2,
        viewport: Vec2,
        originals: &[(EditorObject, Vec3, Quat)],
    ) {
        let mut stage = self.stage.borrow_mut();
        let Some(camera) = stage.displayed_pick.as_ref().map(|pick| pick.camera) else {
            return;
        };
        let Some(scene) = stage.scene.as_mut() else {
            return;
        };
        let targets: Vec<_> = originals
            .iter()
            .map(|(_, position, rotation)| TransformTarget {
                position: *position,
                rotation: *rotation,
                anchor: *position,
            })
            .collect();
        let Some(pivot) = selection_pivot(&targets) else {
            return;
        };
        let forward = (camera.target - camera.position()).normalize();
        let right = forward.cross(Vec3::Z).normalize_or(Vec3::X);
        let up = right.cross(forward).normalize_or(Vec3::Z);
        let extent = 2.0
            * (pivot - camera.position()).length()
            * (camera.fov_y_deg.to_radians() / 2.0).tan();
        let world_screen = (right * pixels.x - up * pixels.y) * (extent / viewport.y.max(1.0));
        for ((object, _, _), target) in originals.iter().zip(targets) {
            let changed = match handle {
                GizmoHandle::TranslateAxis(axis) => {
                    let axis = axis.vector();
                    apply_translation(target, axis * world_screen.dot(axis))
                }
                GizmoHandle::TranslatePlane(normal) => {
                    let normal = normal.vector();
                    apply_translation(target, world_screen - normal * world_screen.dot(normal))
                }
                GizmoHandle::TranslateScreen => apply_translation(target, world_screen),
                GizmoHandle::RotateAxis(axis) => apply_rotation(
                    target,
                    Quat::from_axis_angle(
                        axis.vector(),
                        snap_angle_15((pixels.x - pixels.y) * 0.01),
                    ),
                    pivot,
                    PivotMode::Group,
                ),
                GizmoHandle::RotateScreen => apply_rotation(
                    target,
                    Quat::from_axis_angle(-forward, snap_angle_15((pixels.x - pixels.y) * 0.01)),
                    pivot,
                    PivotMode::Group,
                ),
            };
            set_object_pose(scene, object, changed.position, changed.rotation);
        }
    }
}

fn object_pose(scene: &scene_desc::Scene, object: &EditorObject) -> Option<TransformTarget> {
    let (pos, rot) = match object {
        EditorObject::Fixture(id) => scene
            .fixtures
            .iter()
            .find(|object| &object.id == id)
            .map(|object| (object.pos, object.rot)),
        EditorObject::StagePiece(id) => scene
            .pieces
            .iter()
            .find(|object| &object.id == id)
            .map(|object| (object.pos, object.rot)),
    }?;
    let position = Vec3::from(pos);
    Some(TransformTarget {
        position,
        rotation: Quat::from_mat3(&coords::euler_xyz(rot[0], rot[1], rot[2])),
        anchor: position,
    })
}

fn set_object_pose(
    scene: &mut scene_desc::Scene,
    object: &EditorObject,
    position: Vec3,
    rotation: Quat,
) {
    let euler = coords::euler_xyz_of(Mat3::from_quat(rotation)).to_array();
    match object {
        EditorObject::Fixture(id) => {
            if let Some(object) = scene.fixtures.iter_mut().find(|object| &object.id == id) {
                object.pos = position.to_array();
                object.rot = euler;
            }
        }
        EditorObject::StagePiece(id) => {
            if let Some(object) = scene.pieces.iter_mut().find(|object| &object.id == id) {
                object.pos = position.to_array();
                object.rot = euler;
            }
        }
    }
}

/// The pose a rig opens at: [`View::Front`] of its own framing, fitted to the
/// frame it will be drawn into.
fn opening_camera(framing: &Framing, view: &Viewfinder) -> Camera {
    Camera::for_view(View::Front, framing, None, view)
}

/// three's `getZoomScale`: exponential in the scroll distance, so ten small
/// notches and one big flick land in the same place.
fn zoom_scale(distance: f32) -> f32 {
    ZOOM_BASE.powf(ZOOM_SPEED * distance * 0.05)
}

// -- the venue, in the renderer's vocabulary ---------------------------------

/// One venue as a scene description: geometry in data space, the render dials
/// the web's dark-stage view pins, and an **empty** state map — head state
/// arrives per frame through [`luma_render::StateSource`] instead.
pub(crate) fn scene(
    rig: &Rig,
    definitions: &BTreeMap<String, scene_desc::Definition>,
) -> scene_desc::Scene {
    scene_desc::Scene {
        id: "live".into(),
        times: Vec::new(),
        camera: scene_desc::CameraPose {
            position: [0.0; 3],
            target: [0.0; 3],
        },
        editing: true,
        // Interactive dark-stage defaults, with the haze resolution reduced
        // for the live path. Environment, sun and haze remain independent
        // controls on the renderer contract.
        render: scene_desc::RenderSettings::dark_stage(
            FOV_Y_DEG,
            luma_render::LIVE_HAZE_RESOLUTION,
        ),
        selected_fixture_ids: Vec::new(),
        // A fixture whose definition did not resolve has no mesh and no cone,
        // so it is left out rather than drawn as a guess.
        fixtures: rig
            .fixtures
            .iter()
            .filter(|f| definitions.contains_key(&f.fixture_path))
            .map(|f| scene_desc::Fixture {
                id: f.id.clone(),
                fixture_path: f.fixture_path.clone(),
                mode_name: f.mode_name.clone(),
                pos: [f.pos_x as f32, f.pos_y as f32, f.pos_z as f32],
                rot: [f.rot_x as f32, f.rot_y as f32, f.rot_z as f32],
            })
            .collect(),
        pieces: stage_render::flatten_pieces(&rig.pieces),
        state: BTreeMap::new(),
    }
}

// -- the GPU, and the presentation seam --------------------------------------

/// The renderer and the meshes it has loaded.
struct Gpu {
    viewport: AsyncViewport,
    /// The UI-thread cost of the most recent [`Self::frame`] call.
    ///
    /// Held here rather than returned because these phases run on every call
    /// while a *completed* frame arrives only sometimes, and a measurement
    /// that existed only on the frames that finished would miss precisely the
    /// frames that did not.
    work: StageWork,
    /// What the presentation seam did with the most recent submission.
    ///
    /// Held here for the same reason as `work`, and it is the half that was
    /// missing: a submission happens every call, a delivery only sometimes,
    /// and the frames that were submitted and never delivered are exactly the
    /// ones a stall is made of.
    submission: Submission,
    assets: assets::Library,
    picks: PickTimeline,
    /// Immutable per-asset CPU BVHs; frame snapshots only carry Arc handles.
    pick_meshes: HashMap<String, Arc<TriMesh>>,
}

/// What the presentation seam did with one submitted frame.
#[derive(Clone, Copy, Default)]
struct Submission {
    /// This frame pushed an older, undelivered one out of the queue.
    replaced_undelivered: bool,
    slots: luma_render::Occupancy,
    /// Worker completions at this submit, paired with the census so the two
    /// are read at the same instant — a count sampled a frame later would not
    /// say whether the slots this census calls `Rendering` were moving.
    finished: u64,
    last_signalled: std::time::Duration,
}

struct LiveFrameInputs<'a> {
    scene: &'a scene_desc::Scene,
    definitions: &'a BTreeMap<String, scene_desc::Definition>,
    state: Option<&'a UniverseState>,
    time: f32,
    size: (u32, u32),
    camera: Camera,
    gizmo_mode: GizmoMode,
    gizmo_pivot: Option<Vec3>,
    generic_translate: bool,
    /// Measured in the prepaint that is submitting this frame, so they come
    /// back paired with its own presentation interval.
    spans: UiSpans,
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
struct StageWork {
    /// Evaluating the score at the playhead — [`Library::sample_universe`],
    /// which composites every annotation whose span contains it.
    sample_ms: f32,
    /// Assembling the renderer's frame from the resolved scene.
    build_ms: f32,
    /// Rebuilding the hit-test world so a click can be paired to these pixels.
    pick_ms: f32,
}

impl StageWork {
    /// What the UI thread spent on the stage altogether.
    fn total_ms(self) -> f32 {
        self.sample_ms + self.build_ms + self.pick_ms
    }
}

struct CompletedFrame {
    frame: StageFrame,
    pick: PickSnapshot,
    draw_ms: f32,
    timings: Option<FrameTimings>,
    /// Spacing of the frames that have reached the screen, which is the only
    /// number here that reflects what the operator is actually seeing —
    /// `timings` describes a frame, this describes the stream of them.
    pacing: Option<MetricSummary>,
    /// This frame's own spacing from the one before it. `pacing` summarises the
    /// stream and so cannot name the frame that broke it; a hitch report needs
    /// the individual interval.
    interval_ms: Option<f32>,
    /// Fixture shadow maps this frame actually redrew.
    redrawn_shadow_maps: u32,
    /// The unified light index this frame shaded against.
    clusters: luma_render::LightIndexStats,
    /// How long the UI thread's request waited before the renderer started it.
    queued_ms: f32,
    /// The renderer worker's slot-claim-to-submit span, by phase. Always
    /// present — see `FrameSample::submit_total_ms`.
    cpu: luma_render::CpuSpans,
    /// `draw_ms` split at the driver's completion callback.
    until_signalled_ms: Option<f32>,
    until_noticed_ms: Option<f32>,
    /// Whether this frame came home on the zero-copy shared surface.
    shared_surface: bool,
    /// The UI-thread spans measured when *this* frame was submitted, not when
    /// it came back — which is what makes them comparable with `interval_ms`.
    spans: UiSpans,
}

/// One frame's cost, small enough to keep several seconds of them.
///
/// `Copy` and all-scalar on purpose: this is written every frame on the UI
/// thread, so it must not allocate, and it is only ever read when something
/// already went wrong.
#[derive(Clone, Copy, Default, serde::Serialize)]
struct FrameSample {
    /// Wall time between this frame reaching the screen and the previous one —
    /// the only number here the operator actually experiences.
    interval_ms: f32,
    /// gpui's element walk on the UI thread.
    draw_ms: f32,
    /// Score evaluation, frame assembly, hit-test rebuild.
    score_ms: f32,
    build_ms: f32,
    pick_ms: f32,
    /// Renderer-thread halves. `None`, not zero, until the first profiled
    /// frame lands. One frame in sixteen is profiled and its timings are
    /// carried to the next frame delivered, so consecutive rows do repeat a
    /// value — see `AsyncPresentation::timings` for why that beats attributing
    /// it to a frame that was never presented.
    cpu_encode_ms: Option<f32>,
    gpu_total_ms: Option<f32>,
    /// Of `gpu_total_ms`, the part spent on haze accumulation and temporal
    /// resolve. The volumetric march is fill-bound, so this is the term that
    /// should grow when beams fill the viewport — and the one to check before
    /// blaming anything else for a heavy frame. Zero also means the haze
    /// passes finished before the scene pass they ran alongside, which puts
    /// their cost in `gpu_scene_ms` — see `FrameTimings`.
    gpu_volumetric_ms: Option<f32>,
    /// Of `gpu_total_ms`, the scene pass plus any fixture-shadow passes ahead
    /// of it — separable by `redrawn_shadow_maps` being zero.
    gpu_scene_ms: Option<f32>,
    /// Of `gpu_total_ms`, the composite pass. The three regions are cut at
    /// consecutive fragment-stage completions and so exhaust the total
    /// exactly, which is what lets a heavy frame be attributed rather than
    /// guessed at. Where two adjacent passes overlap on the GPU, the overlap
    /// is charged to the earlier one — see `FrameTimings`.
    gpu_composite_ms: Option<f32>,
    /// Set only on a frame that rebuilt the clustered-light grid, which makes
    /// it a camera-motion marker as much as a cost.
    cluster_ms: Option<f32>,
    /// The renderer worker's CPU span between claiming this frame's
    /// presentation slot and handing the work to the driver, split by phase.
    ///
    /// Unlike every other renderer-thread field here these are never `None`:
    /// they are wall-clock brackets, not adapter timestamps. That is the whole
    /// point of them. A slot reads `Rendering` from the moment it is claimed,
    /// so a worker blocked in this span produces the stall signature — slots
    /// rendering, nothing on the GPU, UI thread healthy — and no field in this
    /// struct could previously see it.
    submit_total_ms: f32,
    submit_prepare_ms: f32,
    submit_clusters_ms: f32,
    submit_upload_ms: f32,
    /// Acquiring the presentation target, including the shared `IOSurface` the
    /// window compositor samples. The suspect that only exists with a real
    /// compositor, which is the harness-versus-window delta.
    submit_targets_ms: f32,
    submit_encode_ms: f32,
    /// Frames the renderer worker had retired when this prepaint ran, and the
    /// last one's submit-to-completion span.
    ///
    /// The delta between consecutive rows is the question the census cannot
    /// answer: through a stall with the slots pinned `Rendering`, a flat count
    /// means the GPU signalled nothing, and a climbing count means the worker
    /// finished frames the UI discarded as stale. Counted on the retire path,
    /// because a discarded frame's timings never reach a sample any other way.
    worker_finished: u64,
    worker_last_signalled_ms: f32,
    /// Camera distance, so a report says how zoomed in the stage was.
    camera_radius: f32,
    /// Physical pixels the renderer was asked for.
    width: u32,
    height: u32,
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
    window_active: bool,
    /// Where the transport was, in track seconds.
    ///
    /// **The field that de-confounds every comparison.** A hitch report is a
    /// ring dumped whenever a frame ran late, so two runs of the same test
    /// capture whatever content happened to be playing then — and comparing a
    /// camera change across two runs silently compared two pieces of music.
    /// With this, any analysis can hold the playhead constant and vary one
    /// thing, including analyses of captures that have already been taken.
    track_time_s: f32,
    /// Cones the score had lit — the content axis every other number scales on.
    lit_cones: u32,
    /// Mean broad-phase candidates per 8 px light-index tile. **The number
    /// that says whether culling works at all**: near the lit-cone count,
    /// every fragment is shading every light and the index is pure overhead.
    mean_lights_per_tile: f32,
    /// Cones that survived the screen cull, and the total (tile, light) mask
    /// bits the broad phase set this frame.
    lights_on_screen: u32,
    tile_references: u32,
    /// Fixture shadow maps redrawn this frame. Zero is the healthy steady
    /// state: a map is only redrawn when its slot's projection or caster set
    /// changed, so a non-zero run means tenancy is churning.
    redrawn_shadow_maps: u32,
    /// Wall time since the stage's previous prepaint — the UI thread's own
    /// cadence. If this tracks `interval_ms` the thread was blocked; if it
    /// stays at frame rate while `interval_ms` grows, the frames were made and
    /// something downstream held them.
    ui_frame_gap_ms: f32,
    /// From this frame's `request_animation_frame` to the stage's prepaint —
    /// everything the UI thread did before reaching the stage. Names a stall
    /// that belongs to some *other* view without needing to instrument it.
    request_to_prepaint_ms: f32,
    /// How long the renderer left this frame queued before starting it.
    /// Non-zero means the renderer was busy, not slow.
    queued_ms: f32,
    /// Stage renders during `ui_frame_gap_ms`. One is healthy. Zero over a long
    /// gap means nothing asked for a frame — a different disease, and a
    /// different owner, from a thread that was busy elsewhere.
    renders_in_gap: u32,
    /// `draw_ms` split at the driver's completion callback: the GPU's share
    /// (submit until it said it was done, including any wait to begin) and the
    /// worker's share (how long after that before anyone looked). This is the
    /// last unmeasured span — between them they say whether the frame was slow
    /// or merely unattended.
    until_signalled_ms: Option<f32>,
    until_noticed_ms: Option<f32>,
    /// Whether the frame crossed on the zero-copy shared surface. False means
    /// the CPU readback fallback, whose copy and map live inside `draw_ms` and
    /// are invisible to `gpu_total_ms`.
    shared_surface: bool,
    /// Whether a completed frame reached the screen on this prepaint.
    ///
    /// **False is the row that used to be missing.** The stage submits a frame
    /// every prepaint but only sometimes has one to show, and recording only
    /// the deliveries made a 345 ms outage look like one slow frame instead of
    /// forty silent ones. Everything measured before submission is still valid
    /// on a ghost row; everything measured on the way back is `None`.
    delivered: bool,
    /// Whether this submission displaced an older frame that never reached the
    /// screen — the pipeline dropping work rather than falling behind.
    replaced_undelivered: bool,
    /// The presentation slots at the moment of submission.
    slots_idle: u8,
    slots_rendering: u8,
    slots_ready: u8,
    slots_reserved: u8,
    /// Whether a slot could have been started for this frame. False is what
    /// `queued_ms` is waiting on.
    slot_startable: bool,
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
struct HitchRing {
    samples: [FrameSample; HITCH_RING],
    next: usize,
    len: usize,
    last_dump: Option<Instant>,
}

/// Four seconds at 60 Hz, matching the presentation window's own reasoning.
const HITCH_RING: usize = 240;

/// A frame that took this long to reach the screen is one the eye caught.
/// Three missed vsyncs at 60 Hz — below this is jitter, above it is a stutter.
const HITCH_MS: f32 = 50.0;

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
    fn record(&mut self, sample: FrameSample, now: Instant) -> Option<Vec<FrameSample>> {
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
    fn recent(&self) -> impl Iterator<Item = &FrameSample> + '_ {
        let start = (self.next + HITCH_RING - self.len) % HITCH_RING;
        (0..self.len).map(move |offset| &self.samples[(start + offset) % HITCH_RING])
    }
}

/// A finished stage frame in the form the compositor will draw it.
///
/// The renderer decides which of these it can produce; the screen paints
/// whichever it is handed. Neither end chooses — that is what keeps the CPU
/// path a fallback rather than a mode.
#[derive(Clone)]
enum StageFrame {
    /// Uploaded into gpui's sprite atlas under [`STAGE_IMAGE_ID`], refreshed in
    /// place each frame.
    Image(Arc<RenderImage>),
    /// Memory the renderer and the compositor both address. Nothing is
    /// uploaded and nothing is copied; the compositor samples where the
    /// renderer wrote.
    #[cfg(target_os = "macos")]
    Shared(luma_render::Surface),
}

type PaintedStage = Option<(StageFrame, Option<PickSnapshot>)>;

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
    fn frame(&mut self, input: LiveFrameInputs<'_>) -> Result<Option<CompletedFrame>, String> {
        let LiveFrameInputs {
            scene,
            definitions,
            state,
            time,
            size: (width, height),
            camera,
            gizmo_mode,
            gizmo_pivot,
            generic_translate,
            spans,
        } = input;
        let built = std::time::Instant::now();
        let mut frame = build_frame_with(
            scene,
            definitions,
            &|id, head| stage_render::primitive_state(state, id, head),
            time,
            &mut self.assets,
        )
        .map_err(|error| format!("Could not assemble the frame: {error}"))?;
        if gizmo_mode == GizmoMode::Rotate {
            install_rotation_gizmo(&mut frame, gizmo_pivot);
        } else if generic_translate
            || !frame
                .overlays
                .iter()
                .any(|overlay| overlay.depth == OverlayDepth::Free)
        {
            frame
                .overlays
                .retain(|overlay| overlay.depth == OverlayDepth::Tested);
            install_translation_gizmo(&mut frame, gizmo_pivot);
        }
        self.work.build_ms = built.elapsed().as_secs_f32() * 1_000.0;
        let picked = std::time::Instant::now();
        let pick = PickSnapshot::from_frame(&frame, scene, camera, &mut self.pick_meshes);
        self.work.pick_ms = picked.elapsed().as_secs_f32() * 1_000.0;
        let completed = self
            .viewport
            .take_latest()
            .transpose()
            .map_err(|error| format!("Could not render the frame: {error}"))?;
        let (serial, outcome, occupancy) = self.viewport.submit_numbered(frame, width, height);
        let (finished, last_signalled) = self.viewport.finished();
        self.submission = Submission {
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
            #[cfg(target_os = "macos")]
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

fn gizmo_scale(frame: &luma_render::Frame, pivot: Vec3) -> f32 {
    let distance = (frame.camera.eye - pivot).length();
    distance * (1.9 * (frame.camera.fov_y_deg.to_radians() / 2.0).tan()).min(7.0) * 0.5 / 7.0
}

fn install_translation_gizmo(frame: &mut luma_render::Frame, pivot: Option<Vec3>) {
    let Some(pivot) = pivot else { return };
    let mesh = frame.meshes.len();
    frame.meshes.push(MeshData {
        key: "::gizmo-translate-segment".into(),
        vertices: vec![
            assets::Vertex {
                position: [0.13, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0; 2],
                tangent: [1.0, 0.0, 0.0, 1.0],
            },
            assets::Vertex {
                position: [1.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0; 2],
                tangent: [1.0, 0.0, 0.0, 1.0],
            },
        ]
        .into(),
        indices: vec![0, 1].into(),
    });
    let scale = gizmo_scale(frame, pivot);
    for (rotation, color) in [
        (Mat4::IDENTITY, Vec3::X),
        (Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2), Vec3::Y),
        (
            Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_2),
            Vec3::new(0.15, 0.35, 1.0),
        ),
    ] {
        frame.overlays.push(Overlay {
            mesh,
            model: Mat4::from_translation(pivot) * rotation * Mat4::from_scale(Vec3::splat(scale)),
            lines: true,
            color,
            opacity: 1.0,
            depth: OverlayDepth::Free,
        });
    }
}

fn install_rotation_gizmo(frame: &mut luma_render::Frame, pivot: Option<Vec3>) {
    frame
        .overlays
        .retain(|overlay| overlay.depth == OverlayDepth::Tested);
    let Some(pivot) = pivot else { return };
    const SEGMENTS: u32 = 64;
    let mut vertices = Vec::with_capacity(SEGMENTS as usize);
    let mut indices = Vec::with_capacity(SEGMENTS as usize * 2);
    for i in 0..SEGMENTS {
        let angle = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        vertices.push(assets::Vertex {
            position: [angle.cos() * 0.8, angle.sin() * 0.8, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
        });
        indices.extend([i, (i + 1) % SEGMENTS]);
    }
    let mesh = frame.meshes.len();
    frame.meshes.push(MeshData {
        key: "::gizmo-rotate-ring".into(),
        vertices: vertices.into(),
        indices: indices.into(),
    });
    let scale = gizmo_scale(frame, pivot);
    for (rotation, color) in [
        (Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2), Vec3::X),
        (Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2), Vec3::Y),
        (Mat4::IDENTITY, Vec3::new(0.15, 0.35, 1.0)),
    ] {
        frame.overlays.push(Overlay {
            mesh,
            model: Mat4::from_translation(pivot) * rotation * Mat4::from_scale(Vec3::splat(scale)),
            lines: true,
            color,
            opacity: 1.0,
            depth: OverlayDepth::Free,
        });
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
fn stage_gpu_enabled() -> bool {
    luma_ui::runtime::Runtime::with(luma_ui::runtime::Runtime::stage_gpu_enabled)
}

// -- the screen --------------------------------------------------------------

/// What the stage should be showing, resolved from the shell's current subject.
///
/// A named triple rather than a bare tuple: `lit` is itself a `(track, venue)`
/// pair, and two levels of anonymous tuple is a return type no call site can
/// read. The two `String`s are also the same type as each other, which is
/// exactly when positional returns start getting swapped by accident.
struct StageSubject {
    venue_id: String,
    venue_name: String,
    /// The `(track, venue)` whose score lights the rig, when one does.
    lit: Option<(String, String)>,
}

/// The 3D view's transitions, kept with the screen the way every other one is
/// (`settings::open_settings`, `track_editor::open_track`), so `lib.rs` stays
/// the list of what exists rather than the list of how each screen is reached.
impl Luma {
    /// The room the stage should be showing this frame, and the score that
    /// should light it.
    ///
    /// **The room comes from the scope, not from the visible tab.** Now that
    /// the strip belongs to the picked track, the scope already names the room
    /// every tab in that strip is being worked on against — so clicking from a
    /// timeline to a pattern graph in the same strip leaves the stage exactly
    /// where it was, rather than tearing it down because the tab that happened
    /// to be showing named no venue. Only the *lighting* still asks the tab,
    /// because only a track editor knows a `(track, venue)` to composite.
    fn stage_subject(&self) -> Option<StageSubject> {
        // A hidden pane and a hidden workspace are the same fact to the stage:
        // there is no column to sit in. Both drop it rather than merely
        // leaving it undrawn, so a collapsed panel does not keep a GPU and a
        // loaded rig alive behind it.
        if self.visualizer_hidden || self.workspace_hidden {
            return None;
        }
        // Nothing open below it is nothing to be a view *of*. A venue picked in
        // the sidebar is a browser, not a room being worked on, and raising a
        // stage over it loads that venue's whole rig and builds its scene for a
        // surface nobody asked for — a cost the browser pays on every venue
        // click, in the app as much as in a test.
        if self.workspace.is_empty() {
            return None;
        }
        let scope = self.tab_scope()?;
        let venue_id = scope.venue().to_string();
        let lit = match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) => state
                .subject()
                .map(|(track, venue, _)| (track, venue))
                .filter(|(_, venue)| venue == &venue_id),
            Some(Body::Graph(_) | Body::Universe(_)) | None => None,
        };
        let name = self
            .sidebar
            .as_ref()
            .filter(|browser| browser.venue_id() == venue_id)
            .map_or_else(
                || venue_id.clone(),
                |browser| browser.venue_name().to_string(),
            );
        Some(StageSubject {
            venue_id,
            venue_name: name,
            lit,
        })
    }

    /// Keep the stage pane pointed at the tab below it: build it when a room
    /// appears, re-light it when only the score changed, drop it when the
    /// workspace has nothing about a room.
    ///
    /// Done at draw rather than at every navigation for the reason
    /// [`Luma::sync_chat`] is: a navigation is a field assignment, and a
    /// gesture that forgot to ask would leave the stage lighting a room
    /// nothing on screen is about. Comparing the venue *and* the score is what
    /// separates the two costs — a different room rebuilds, a different track
    /// only re-composites (see [`Visualizer::relight`]).
    pub(crate) fn sync_visualizer(&mut self, cx: &mut Context<Self>) {
        let Some(StageSubject {
            venue_id,
            venue_name,
            lit: subject,
        }) = self.stage_subject()
        else {
            // Dropping the state is what un-mounts the viewport, and
            // un-mounting is what stops its continuous redraw — see the
            // rendering note on [`visualizer`].
            if self.visualizer.take().is_some() {
                cx.notify();
            }
            return;
        };
        // Split the borrow: both arms read the library and mutate the stage,
        // and the two fields are disjoint. The same split `shell::active_tab`
        // takes for the same reason.
        let Luma {
            visualizer,
            library,
            ..
        } = self;
        match visualizer {
            Some(state) if state.venue_id == venue_id => {
                if state.subject != subject {
                    state.relight(library, subject, cx);
                    cx.notify();
                }
            }
            _ => {
                *visualizer = Some(Visualizer::open(
                    library, &venue_id, venue_name, subject, cx,
                ));
                cx.notify();
            }
        }
    }

    /// Give the stage's room back to the editor under it, or take it again.
    ///
    /// Hiding **drops** the stage rather than merely leaving it undrawn: an
    /// off-screen viewport holding a GPU and a loaded rig is the cost this
    /// pane most needs not to have, and there is nothing to preserve — which
    /// room it shows is re-derived from the workspace the moment it returns.
    /// Showing it again is therefore also the one gesture that re-frames a
    /// camera the pointer has wandered off with.
    pub(crate) fn toggle_visualizer(&mut self, cx: &mut Context<Self>) {
        self.visualizer_hidden = !self.visualizer_hidden;
        cx.notify();
    }

    /// The stage's state, when one is mounted. Every pointer handler and
    /// toolbar button goes through here, so none of them can act on a stage
    /// that is not on screen.
    pub(crate) fn visualizer_mut(&mut self) -> Option<&mut Visualizer> {
        self.visualizer.as_mut()
    }
}

/// The stage pane: chrome above, viewport below.
///
/// # Calling this is what starts the redraw loop
///
/// The request below is unconditional and self-sustaining: asking at the top of
/// a render is what makes the next one happen. There is deliberately no
/// "visible" flag guarding it, because a flag would be a second answer to a
/// question the element tree already answers — the loop stops when, and only
/// when, the shell stops mounting this pane. Every caller must therefore treat
/// *not calling* as the off switch (see [`Luma::sync_visualizer`] and
/// `shell::workspace_body`); a hidden pane that still rendered would keep a GPU
/// busy drawing frames nobody sees.
pub(crate) fn visualizer(
    state: &mut Visualizer,
    app: &Entity<Luma>,
    library: &Library,
    window: &mut Window,
) -> Div {
    // Continuous redraw: asking at the top of a render is what makes the next
    // one happen, and CVDisplayLink paces it (spec §4.3).
    window.request_animation_frame();
    // Stamped here so the stage's prepaint can say how much of the frame the
    // rest of the UI thread spent before reaching it, and counted so a gap with
    // *no* render can be told from a gap full of other work.
    {
        let mut stage = state.stage.borrow_mut();
        stage.requested_at = Some(Instant::now());
        stage.renders_since_prepaint = stage.renders_since_prepaint.saturating_add(1);
    }
    let chrome = toolbar(state, app, library);
    let floating = overlay_toolbar(state, app);
    let fps = fps_overlay(state, app);
    let lab = state
        .render_lab
        .open
        .then(|| renderer_lab(state, app).into_any_element());
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .child(chrome)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(body(state, app, library))
                .child(fps)
                .child(floating)
                .children(lab),
        )
}

fn toolbar(state: &Visualizer, app: &Entity<Luma>, library: &Library) -> Div {
    let readout = match &state.status {
        Status::Loading => "LOADING".to_string(),
        Status::Live { lit } => format!(
            "{} FIXTURES · {}",
            state.fixture_count(),
            if *lit { "LIVE" } else { "UNLIT" }
        ),
        Status::Empty(_) => "NO RIG".to_string(),
    };
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .px(px(16.))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            div()
                .text_size(px(12.))
                .child(state.venue_name.clone())
                .agent_node(Role::Text, state.venue_name.clone()),
        )
        .child(clock_readout(library))
        .child(renderer_lab_trigger(state, app))
        .child(luma_ui::silkscreen(readout))
}

fn renderer_lab_trigger(state: &Visualizer, app: &Entity<Luma>) -> impl IntoElement {
    let label = if state.render_lab.open {
        "Close Renderer Lab"
    } else {
        "Open Renderer Lab"
    };
    let app = app.clone();
    luma_ui::luma_button(label, Enabled::Yes)
        .id("renderer-lab")
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.render_lab.open = !state.render_lab.open;
                }
                cx.notify();
            });
        })
        .agent_node(Role::Toggle, label)
}

/// The lab panel: a fixed head over a body that scrolls.
///
/// Bounded top *and* bottom against the stage rather than sized by its
/// contents. The panel is longer than most windows are tall, and an absolutely
/// positioned column with no floor simply ran off the bottom of the pane —
/// every control below the fold was unreachable, with nothing on screen to say
/// so. Spanning the pane and scrolling the body is what makes the list's length
/// the body's problem instead of the window's.
///
/// The title stays out of the scroller: a panel whose own name scrolls away
/// reads as page content, not as a panel.
fn renderer_lab(state: &Visualizer, app: &Entity<Luma>) -> Div {
    div()
        .absolute()
        .top(px(12.))
        .right(px(12.))
        .bottom(px(12.))
        .w(px(360.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .border_1()
        .border_color(ladder::border())
        .bg(ladder::apex())
        .child(luma_ui::silkscreen("RENDERER LAB"))
        .child(lab_controls(state, app))
}

/// Every control in the lab, in one scrolling column.
fn lab_controls(state: &Visualizer, app: &Entity<Luma>) -> impl IntoElement {
    let lab = &state.render_lab;
    div()
        // `min_h_0` is what lets this actually shrink: without it a flex child
        // sized by its content refuses to be smaller than the content, and the
        // column overflows its parent exactly as it did before.
        .flex_1()
        .min_h_0()
        .overflow_y_scrollbar()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(lab_toggle(app, "Sun", lab.sun_enabled, LabToggle::Sun))
        .child(lab_value(
            app,
            "Sun azimuth",
            lab.sun_azimuth_deg,
            -180.0,
            180.0,
            LabValue::Azimuth,
        ))
        .child(lab_value(
            app,
            "Sun elevation",
            lab.sun_elevation_deg,
            -85.0,
            85.0,
            LabValue::Elevation,
        ))
        .child(lab_value(
            app,
            "Sun intensity",
            lab.sun_intensity,
            0.0,
            10.0,
            LabValue::SunIntensity,
        ))
        .children(color_controls(
            app,
            "Sun color",
            lab.sun_color,
            LabValue::SunColor,
        ))
        .child(lab_toggle(
            app,
            "Sun shadows",
            lab.sun_shadows,
            LabToggle::Shadows,
        ))
        .child(lab_value(
            app,
            "Shadow softness",
            lab.sun_shadow_softness,
            0.0,
            3.0,
            LabValue::SunShadowSoftness,
        ))
        .child(lab_toggle(
            app,
            "Environment",
            lab.environment_enabled,
            LabToggle::Environment,
        ))
        .children(color_controls(
            app,
            "Background",
            lab.background_color,
            LabValue::BackgroundColor,
        ))
        .children(color_controls(
            app,
            "Ambient color",
            lab.ambient_color,
            LabValue::AmbientColor,
        ))
        .child(lab_value(
            app,
            "Ambient intensity",
            lab.ambient_intensity,
            0.0,
            2.0,
            LabValue::Ambient,
        ))
        .child(lab_toggle(
            app,
            "HDR probe",
            lab.probe_enabled,
            LabToggle::Probe,
        ))
        .child(lab_value(
            app,
            "Probe intensity",
            lab.probe_intensity,
            0.0,
            4.0,
            LabValue::ProbeIntensity,
        ))
        .child(lab_value(
            app,
            "Probe rotation",
            lab.probe_rotation_deg,
            -180.0,
            180.0,
            LabValue::ProbeRotation,
        ))
        .child(lab_toggle(
            app,
            "Show probe background",
            lab.probe_visible,
            LabToggle::ProbeVisible,
        ))
        .child(lab_toggle(
            app,
            "Fixture haze",
            lab.haze_enabled,
            LabToggle::Haze,
        ))
        .child(lab_toggle(
            app,
            "Fixture surface light",
            lab.fixture_surface_lighting,
            LabToggle::FixtureSurfaceLighting,
        ))
        .child(lab_toggle(
            app,
            "Fixture shadows",
            lab.fixture_shadows,
            LabToggle::FixtureShadows,
        ))
        .child(lab_toggle(
            app,
            "Cluster occupancy",
            lab.cluster_debug,
            LabToggle::ClusterDebug,
        ))
        .child(lab_value(
            app,
            "Haze density",
            lab.haze_density,
            0.0,
            2.0,
            LabValue::HazeDensity,
        ))
        .child(lab_value(
            app,
            "Haze steps",
            lab.haze_steps as f32,
            1.0,
            64.0,
            LabValue::HazeSteps,
        ))
        .child(lab_value(
            app,
            "Haze resolution",
            lab.haze_resolution,
            0.25,
            1.0,
            LabValue::HazeResolution,
        ))
        .child(lab_toggle(
            app,
            "Editor grid",
            lab.grid_enabled,
            LabToggle::Grid,
        ))
        .child(debug_view_button(app, lab.debug_label()))
}

fn color_controls(
    app: &Entity<Luma>,
    label: &'static str,
    color: [f32; 3],
    control: fn(usize) -> LabValue,
) -> [Div; 3] {
    [0, 1, 2].map(|index| {
        let channel_label = match (label, index) {
            ("Sun color", 0) => "Sun color red",
            ("Sun color", 1) => "Sun color green",
            ("Sun color", _) => "Sun color blue",
            ("Background", 0) => "Background red",
            ("Background", 1) => "Background green",
            ("Background", _) => "Background blue",
            ("Ambient color", 0) => "Ambient color red",
            ("Ambient color", 1) => "Ambient color green",
            ("Ambient color", _) => "Ambient color blue",
            _ => unreachable!("color controls have a fixed label set"),
        };
        lab_value(app, channel_label, color[index], 0.0, 1.0, control(index))
    })
}

fn debug_view_button(app: &Entity<Luma>, view: &'static str) -> impl IntoElement {
    let app = app.clone();
    let label = format!("Debug view: {view}");
    luma_ui::luma_button(&label, Enabled::Yes)
        .id("renderer-debug-view")
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.render_lab.cycle_debug_view();
                }
                cx.notify();
            });
        })
        .agent_node(Role::Button, label)
}

fn lab_toggle(
    app: &Entity<Luma>,
    label: &'static str,
    checked: bool,
    control: LabToggle,
) -> impl IntoElement {
    let app = app.clone();
    div()
        .id(label)
        .flex()
        .items_center()
        .gap(px(8.))
        .child(luma_ui::luma_checkbox(checked))
        .child(div().text_size(px(11.)).child(label))
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.render_lab.toggle(control);
                }
                cx.notify();
            });
        })
        .agent_node(Role::Checkbox, label)
}

/// One labelled parameter: a name and the slider that sets it.
///
/// The slider is the whole control. It used to be a picture flanked by
/// `Decrease`/`Increase` buttons, because the picture could not be dragged —
/// two ways to set one number, and the pair only existed to stand in for the
/// one that did not work. With the drag ported the buttons are the duplicate,
/// so they are gone; the slab's own bounds are the range, and `set` is where
/// that range is enforced.
fn lab_value(
    app: &Entity<Luma>,
    label: &'static str,
    value: f32,
    min: f32,
    max: f32,
    control: LabValue,
) -> Div {
    let app = app.clone();
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().w(px(104.)).text_size(px(10.)).child(label))
        .child(
            luma_ui::luma_slider(label, value, min, max, 140.0, move |value, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(state) = this.visualizer_mut() {
                        state.render_lab.set(control, value);
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Slider, label),
        )
}

/// Play/pause and the clock, over the *host's* transport rather than the track
/// editor's.
///
/// The editor keeps a playhead of its own because it draws one; this view draws
/// none — every frame reads [`Library::render_time`] afresh — so what it needs
/// is the host running, and nothing in between. That is why this is two calls
/// and not a share of `track_editor`'s transport, which is mostly the machinery
/// for keeping a local playhead in step with a remote one.
/// Where the host transport has got to, as the stage sees it.
///
/// A **readout and not a control**: the stage sits over an editor that owns a
/// timeline, and that editor's transport already drives this very clock —
/// `library.play()`/`pause()` is one host transport, not one per view. A second
/// Play button here would be the same verb spelled twice, which is both a
/// design smell and, for anything addressing controls by label, an ambiguity.
///
/// The readout itself is not duplicated: it reports `render_time`, which is
/// what the *renderer* last drew, and no editor shows that.
fn clock_readout(library: &Library) -> Div {
    let host = library.transport();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(luma_ui::silkscreen(format!(
            "{} / {}",
            crate::track_editor::clock(library.render_time()),
            crate::track_editor::clock(host.duration_seconds)
        )))
}

/// The web's bottom-centre floating toolbar, cut to the verbs that mean
/// something with no editing: the two zoom buttons.
fn overlay_toolbar(state: &Visualizer, app: &Entity<Luma>) -> Div {
    let zoom = |label: &'static str, factor: f32| {
        let app = app.clone();
        luma_ui::luma_button(label, Enabled::Yes)
            .id(label)
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(state) = this.visualizer_mut() {
                        state.dolly(factor);
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Button, label)
    };
    let mode = |label: &'static str, mode: GizmoMode| {
        let app = app.clone();
        luma_ui::luma_button(label, Enabled::Yes)
            .id(label)
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(state) = this.visualizer_mut() {
                        state.gizmo_mode = mode;
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Button, label)
    };
    div()
        .absolute()
        .bottom(TOOLBAR_OVERLAY_BOTTOM)
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .when(matches!(state.status, Status::Live { .. }), |el| {
            el.child(
                div()
                    .flex()
                    .gap(px(1.))
                    .bg(ladder::trim())
                    .p(px(1.))
                    .child(mode("Translate", GizmoMode::Translate))
                    .child(mode("Rotate", GizmoMode::Rotate))
                    .child(zoom("Zoom In", DOLLY_IN))
                    .child(zoom("Zoom Out", DOLLY_OUT)),
            )
        })
}

/// One 60 Hz frame — the graph's hairline, and the bound a clean frame sits
/// under.
const FRAME_BUDGET_MS: f32 = 1_000.0 / 60.0;

/// Bars in the frame-time graph — about two seconds at 60 Hz.
const GRAPH_BARS: usize = 120;

/// The window the headline rate is averaged over.
const FPS_WINDOW_MS: f32 = 1_000.0;

/// What the presented-frame stream has been doing lately, read from the hitch
/// ring — the one place several seconds of frames already live, so the readout
/// costs no second record.
struct FpsReading {
    /// Delivered rate over the trailing second, when anything arrived.
    fps: Option<f32>,
    /// The worst delivered interval in the ring's whole window. The rate alone
    /// averages away exactly the frames the eye catches; this is the dip.
    low_ms: Option<f32>,
    /// Every delivered interval in the window, oldest first, for the graph.
    intervals: Vec<f32>,
}

fn fps_reading(stage: &Stage) -> FpsReading {
    let intervals: Vec<f32> = stage
        .hitches
        .recent()
        .filter(|sample| sample.delivered && sample.interval_ms > 0.0)
        .map(|sample| sample.interval_ms)
        .collect();
    let low_ms = intervals.iter().copied().reduce(f32::max);
    let mut sum = 0.0;
    let mut frames = 0u32;
    for ms in intervals.iter().rev() {
        sum += ms;
        frames += 1;
        if sum >= FPS_WINDOW_MS {
            break;
        }
    }
    FpsReading {
        fps: (sum > 0.0).then(|| frames as f32 * 1_000.0 / sum),
        low_ms,
        intervals,
    }
}

/// The corner FPS readout, and the frame-stats panel it unfolds into.
///
/// This is the stats' one home — the toolbar and the lab used to publish
/// overlapping halves of the same numbers, and two surfaces for one reading is
/// a drifted duplicate waiting to happen. Folded it is the rate and its worst
/// recent frame; unfolded it adds the frame-time graph and the per-phase
/// numbers the hitch ring already records, under the same labels the harness
/// has always read (`DRAW`, `UI`, `PRES`, `CPU`).
fn fps_overlay(state: &Visualizer, app: &Entity<Luma>) -> Div {
    let live = matches!(state.status, Status::Live { .. });
    let expanded = state.fps_expanded;
    let (resting, reading, draw, ui, pres, gpu, shadows) = {
        let stage = state.stage.borrow();
        let work = stage.last_work;
        (
            stage.resting,
            fps_reading(&stage),
            stage
                .last_draw_ms
                .map_or_else(|| "DRAW —".to_string(), |ms| format!("DRAW {ms:.1} MS")),
            format!(
                "UI {:.1} (S {:.1} B {:.1} P {:.1}) MS",
                work.total_ms(),
                work.sample_ms,
                work.build_ms,
                work.pick_ms
            ),
            stage.last_present.map_or_else(
                || "PRES —".to_string(),
                |present| format!("PRES {:.1}/{:.1} MS", present.p50_ms, present.p95_ms),
            ),
            match (stage.last_cpu_ms, stage.last_gpu_ms, stage.last_cluster_ms) {
                (Some(cpu), Some(gpu), Some(cluster)) => {
                    format!("CPU {cpu:.2} · GPU {gpu:.2} · CLUSTER {cluster:.2} MS")
                }
                _ => "CPU/GPU timing unavailable".to_string(),
            },
            format!("SHADOWS {} REDRAWN", stage.last_shadow_maps.unwrap_or(0)),
        )
    };
    // A resting stage is not rendering slowly, it is not rendering at all —
    // a number here, stale or zero, would read as one or the other.
    let fps_text = if resting {
        "IDLE".to_string()
    } else {
        reading
            .fps
            .map_or_else(|| "—".to_string(), |fps| format!("{fps:.0}"))
    };
    let low_text = reading.low_ms.map_or_else(
        || "LOW —".to_string(),
        |ms| format!("LOW {:.0}", 1_000.0 / ms.max(1.0)),
    );
    let dipped = reading.low_ms.is_some_and(|ms| ms >= HITCH_MS);
    let header = div()
        .flex()
        .items_end()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(18.))
                .line_height(px(18.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ladder::foreground())
                .child(fps_text.clone())
                .agent_node(Role::Text, format!("FPS {fps_text}")),
        )
        .child(luma_ui::silkscreen("FPS"))
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(9.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(if dipped {
                    ladder::status_bad()
                } else {
                    ladder::muted_foreground()
                })
                .child(low_text.clone())
                .agent_node(Role::Text, low_text),
        );
    let toggle = {
        let app = app.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.fps_expanded = !state.fps_expanded;
                }
                cx.notify();
            });
        }
    };
    div()
        .absolute()
        .top(STATS_OVERLAY_TOP)
        .left(px(12.))
        .when(live, |el| {
            el.child(
                div()
                    .id("fps-overlay")
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(8.))
                    .border_1()
                    .border_color(ladder::border())
                    .bg(ladder::apex())
                    .when(expanded, |el| el.w(px(224.)))
                    .child(header)
                    .when(expanded, |el| {
                        el.child(frame_graph(reading.intervals))
                            .child(div().h(px(1.)).bg(ladder::trim()))
                            .child(luma_ui::silkscreen(draw))
                            .child(luma_ui::silkscreen(ui))
                            .child(luma_ui::silkscreen(pres))
                            .child(luma_ui::silkscreen(gpu))
                            .child(luma_ui::silkscreen(shadows))
                    })
                    .on_click(toggle)
                    .agent_node(Role::Toggle, "Frame stats"),
            )
        })
}

/// Delivered frame intervals as bars, newest at the right.
///
/// The y scale is pinned to the hitch threshold rather than the data's own
/// maximum, so a dip reads at the same height in every capture and a graph
/// with no dips does not stretch its jitter to fill the box. A frame over one
/// missed vsync warns, over the hitch threshold it is the failure colour —
/// hue for meaning, as the ladder allows.
fn frame_graph(intervals: Vec<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            window.paint_quad(gpui::fill(bounds, ladder::background()));
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let budget_y = height * (1.0 - FRAME_BUDGET_MS / HITCH_MS);
            window.paint_quad(gpui::fill(
                Bounds {
                    origin: bounds.origin + Point::new(px(0.), px(budget_y)),
                    size: gpui::Size {
                        width: bounds.size.width,
                        height: px(1.),
                    },
                },
                ladder::foreground_alpha(0.15),
            ));
            let bar = width / GRAPH_BARS as f32;
            for (slot, ms) in intervals.iter().rev().take(GRAPH_BARS).enumerate() {
                let fraction = (ms / HITCH_MS).clamp(0.04, 1.0);
                let color: gpui::Hsla = if *ms >= HITCH_MS {
                    ladder::status_bad().into()
                } else if *ms > FRAME_BUDGET_MS * 1.5 {
                    ladder::status_warn().into()
                } else {
                    ladder::foreground_alpha(0.35)
                };
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: bounds.origin
                            + Point::new(
                                px(width - (slot + 1) as f32 * bar),
                                px(height * (1.0 - fraction)),
                            ),
                        size: gpui::Size {
                            width: px((bar - 1.0).max(1.0)),
                            height: px(height * fraction),
                        },
                    },
                    color,
                ));
            }
        },
    )
    .w_full()
    .h(px(36.))
}

/// The viewport itself, or the reason there isn't one.
fn body(state: &mut Visualizer, app: &Entity<Luma>, library: &Library) -> AnyElement {
    // A frame that failed drew nothing and left its reason behind; adopt it
    // before deciding what this frame shows.
    if let Some(error) = state.stage.borrow_mut().error.take() {
        state.status = Status::Empty(error);
    }
    // Device off by request: keep the pane's own node so a tree assertion
    // cannot tell the two configurations apart, and skip the renderer.
    if !state.gpu_enabled {
        return plate("Stage rendering is off".to_string())
            .agent_node(Role::Card, "Stage")
            .into_any_element();
    }
    // Idempotent, and normally a no-op: launch has already started this. It is
    // here as well so a stage opened by something that is not the app — a
    // harness, a test — still drives the warmup to a conclusion instead of
    // sitting on `Cold` for ever.
    luma_render::warm();
    // Compiling every pipeline is the one wait at launch long enough to need
    // naming. Saying so beats a blank rectangle, which is indistinguishable
    // from a stage with nothing in it. The pane re-renders every frame
    // (`request_animation_frame` above), so this clears itself.
    match luma_render::warming() {
        luma_render::Warming::Compiling | luma_render::Warming::Cold => {
            return plate("Compiling shaders…".to_string());
        }
        luma_render::Warming::Unavailable(why) => {
            return plate(format!("The GPU is unavailable: {why}"));
        }
        luma_render::Warming::Ready { .. } => {}
    }
    if matches!(state.status, Status::Empty(_))
        || state.stage.borrow().scene.is_none()
        || !state.gpu_ready()
    {
        return plate(match &state.status {
            Status::Loading => "Loading the rig…".to_string(),
            Status::Empty(why) => why.clone(),
            Status::Live { .. } => "Nothing to draw".to_string(),
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
                        "shared_surface": "true = zero-copy IOSurface; false = CPU readback, whose copy and map sit inside draw_ms and are invisible to gpu_total_ms",
                        "delivered": "false = a prepaint that submitted a frame and got none back; interval/draw/queued/gpu are absent on those rows, everything measured before submission is valid",
                        "replaced_undelivered": "this submission pushed an older frame out of the queue before it ever reached the screen",
                        "slots_idle/rendering/ready/reserved": "presentation slots at submit; reserved are held because their surface is still on screen",
                        "slot_startable": "false = no slot could take this frame, which is what queued_ms then waits on",
                        "draw_ms": "renderer submit to observed completion (latency, not cost); until_signalled_ms + until_noticed_ms is the same span, split",
                        "until_signalled_ms": "submit until the driver's completion callback fired — the GPU's share, including any wait to begin executing",
                        "until_noticed_ms": "completion callback until the worker observed it — nobody was looking, not anything slow",
                        "score_ms": "score evaluation, UI thread",
                        "build_ms": "frame assembly, UI thread",
                        "pick_ms": "hit-test rebuild, UI thread",
                        "cpu_encode_ms": "renderer CPU encode and submit; null when this frame carried no timestamps",
                        "gpu_total_ms": "GPU timeline from the first render pass to composite completion, from the most recently profiled frame (one in sixteen) rather than necessarily this one; null until the first lands",
                        "gpu_volumetric_ms": "of gpu_total_ms, haze accumulation and temporal resolve — the fill-bound term; zero also means the haze passes finished before the scene pass they ran alongside, putting their cost in gpu_scene_ms",
                        "gpu_scene_ms": "of gpu_total_ms, the scene pass plus any shadow passes ahead of it (redrawn_shadow_maps==0 makes it scene alone)",
                        "gpu_composite_ms": "of gpu_total_ms, the composite pass; scene+volumetric+composite are cut at consecutive fragment-stage completions and exhaust the total exactly, with time two concurrent passes shared charged to whichever finished first",
                        "cluster_ms": "clustered-light rebuild; null when untimed, 0 on a cache hit",
                        "submit_total_ms": "renderer worker, slot claim to queue.submit. NEVER null — a wall bracket, not an adapter timestamp. A slot reads Rendering from the moment it is claimed, so this is the only field that can see a worker blocked BEFORE anything reaches the GPU, which is the stall signature (slots rendering, no GPU work, UI thread healthy)",
                        "submit_prepare/clusters/upload/targets/encode_ms": "of submit_total_ms, disjoint and summing to it. targets = acquiring the presentation target including the shared IOSurface the compositor samples; encode = command encoding through queue.submit",
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
                    "reading": "All spans describe the same frame as interval_ms; they are paired by submission serial. Work the late frame in order: renders_in_gap 0 means nothing asked for a frame; ui_frame_gap_ms tracking interval_ms means the UI thread did not produce one, and request_to_prepaint_ms says how much of that belonged to another view before the walk reached the stage; if ui_frame_gap_ms stays at frame rate while interval_ms grows, the frames were made and something downstream held them. Then count the delivered=false rows between two deliveries: that is how many frames were made and thrown away, and on those rows slot_startable, the slot counts and replaced_undelivered say why.",
                },
                "hitchMs": run_up.last().map_or(0.0, |frame| frame.interval_ms),
                "frames": run_up,
            },
        }));
    }

    // The one live read. `render_time` and `sample_universe` are synchronous
    // because a frame's inputs must be this frame's — see `Library`.
    let time = library.render_time();
    let sampled = std::time::Instant::now();
    let universe = library.sample_universe(time);
    let sample_ms = sampled.elapsed().as_secs_f32() * 1_000.0;
    state.status = Status::Live {
        lit: universe.is_some(),
    };

    // Only resolved values cross into the `'static` paint closure; the mutable
    // lab remains owned by the screen.
    let stage = Rc::clone(&state.stage);
    let camera = state.camera;
    let sun_enabled = state.render_lab.sun_enabled;
    let sun_direction = state.render_lab.sun_direction();
    let sun_intensity = state.render_lab.sun_intensity;
    let sun_color = state.render_lab.sun_color;
    let sun_shadows = state.render_lab.sun_shadows;
    let sun_shadow_softness = state.render_lab.sun_shadow_softness;
    let environment_enabled = state.render_lab.environment_enabled;
    let ambient_intensity = state.render_lab.ambient_intensity;
    let ambient_color = state.render_lab.ambient_color;
    let background_color = state.render_lab.background_color;
    let probe_enabled = state.render_lab.probe_enabled;
    let probe_intensity = state.render_lab.probe_intensity;
    let probe_rotation_deg = state.render_lab.probe_rotation_deg;
    let probe_visible = state.render_lab.probe_visible;
    let fixture_surface_lighting = state.render_lab.fixture_surface_lighting;
    let fixture_shadows = state.render_lab.fixture_shadows;
    let cluster_debug = state.render_lab.cluster_debug;
    let haze_enabled = state.render_lab.haze_enabled;
    let haze_density = state.render_lab.haze_density;
    let haze_steps = state.render_lab.haze_steps;
    let haze_resolution = state.render_lab.haze_resolution;
    let grid_enabled = state.render_lab.grid_enabled;
    let debug_view = state.render_lab.debug_view;
    let selected_fixture_ids = {
        let stage = state.stage.borrow();
        stage.displayed_pick.as_ref().map_or_else(Vec::new, |pick| {
            // Overlay builder expects primary first; Selection keeps primary
            // at the tail for deterministic shift-toggle reassignment.
            state
                .selection
                .selected()
                .iter()
                .rev()
                .filter_map(|target| match pick.object(*target) {
                    Some(EditorObject::Fixture(id)) => Some(id.clone()),
                    _ => None,
                })
                .collect()
        })
    };
    let gizmo_pivot = {
        let stage = state.stage.borrow();
        stage.displayed_pick.as_ref().and_then(|pick| {
            let targets: Vec<_> = state
                .selection
                .selected()
                .iter()
                .filter_map(|target| pick.anchors.get(target))
                .map(|anchor| TransformTarget {
                    position: *anchor,
                    rotation: Quat::IDENTITY,
                    anchor: *anchor,
                })
                .collect();
            selection_pivot(&targets)
        })
    };
    let generic_translate = {
        let stage = state.stage.borrow();
        stage.displayed_pick.as_ref().is_some_and(|pick| {
            state
                .selection
                .selected()
                .iter()
                .any(|target| matches!(pick.object(*target), Some(EditorObject::StagePiece(_))))
        })
    };
    let gizmo_mode = state.gizmo_mode;
    // A pointer drag holds the idle gate open: camera drags move the key's
    // camera anyway, but a gizmo drag mutates the scene's geometry, which the
    // key deliberately does not carry.
    let interacting = state.drag.is_some() || state.editor_drag.is_some();
    let key_lab = {
        let mut lab = state.render_lab.clone();
        lab.open = false;
        lab
    };
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
                        state.camera = opening_camera(&state.framing, &state.view_finder());
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
                }
            };
            let scale = window.scale_factor();
            // What the element occupies, and — while that is still moving —
            // the size the stage keeps drawing at instead. See [`RenderSize`].
            let laid_out = (
                (f32::from(bounds.size.width) * scale).round().max(1.0) as u32,
                (f32::from(bounds.size.height) * scale).round().max(1.0) as u32,
            );
            let (width, height) = {
                let mut stage = stage.borrow_mut();
                let settled = stage.rendered_size.settle(laid_out);
                if stage.rendered_size.pending.is_some() {
                    // The hold has to be able to end on its own. The shell's
                    // tween stops asking for frames the moment it lands, and a
                    // paused stage asks for none of its own — without this a
                    // ⌘B over a still rig would leave the last picture
                    // stretched until something else happened to redraw.
                    window.request_animation_frame();
                }
                settled
            };

            let image = {
                let mut stage = stage.borrow_mut();
                let stage = &mut *stage;
                match (stage.scene.as_mut(), stage.gpu.as_mut()) {
                    (Some(scene), Some(gpu)) => {
                        let key = IdleKey {
                            time_bits: time.to_bits(),
                            camera,
                            size: (width, height),
                            lab: key_lab.clone(),
                            selected: selected_fixture_ids.clone(),
                            gizmo_mode,
                            gizmo_pivot,
                            generic_translate,
                            universe: universe.clone(),
                        };
                        let rest = if interacting {
                            stage.idle = None;
                            false
                        } else {
                            match &mut stage.idle {
                                Some((held, settle)) if *held == key => match settle {
                                    0 => true,
                                    _ => {
                                        *settle -= 1;
                                        false
                                    }
                                },
                                slot => {
                                    *slot = Some((key, SETTLE_FRAMES));
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
                            }
                            stage.previous.clone().map(|frame| (frame, None))
                        } else {
                            stage.resting = false;
                            scene.camera.position =
                                coords::three_from_world(camera.position()).to_array();
                            scene.camera.target =
                                coords::three_from_world(camera.target).to_array();
                            scene.render.fov = camera.fov_y_deg;
                            scene.render.environment = scene_desc::Environment {
                                background: if environment_enabled {
                                    background_color
                                } else {
                                    scene_desc::Environment::DARK.background
                                },
                                ambient_color,
                                ambient_intensity,
                                probe: probe_enabled.then(|| scene_desc::EnvironmentProbe {
                                    asset: "environments/studio.hdr".into(),
                                    intensity: probe_intensity,
                                    rotation_deg: probe_rotation_deg,
                                    visible: probe_visible,
                                }),
                            };
                            scene.render.sun =
                                sun_enabled.then_some(scene_desc::DirectionalLight {
                                    direction: sun_direction,
                                    color: sun_color,
                                    intensity: sun_intensity,
                                    shadows: sun_shadows,
                                    shadow_softness: sun_shadow_softness,
                                });
                            scene.render.haze.enabled = haze_enabled;
                            scene.render.haze.density = haze_density;
                            scene.render.haze.steps = haze_steps;
                            scene.render.haze.resolution = haze_resolution;
                            scene.render.show_grid = grid_enabled;
                            scene.render.debug_view = debug_view;
                            scene.render.fixture_surface_lighting = fixture_surface_lighting;
                            scene.render.fixture_shadows = fixture_shadows;
                            scene.render.cluster_debug = cluster_debug;
                            scene.selected_fixture_ids = selected_fixture_ids.clone();
                            match gpu.frame(LiveFrameInputs {
                                scene,
                                definitions: &stage.definitions,
                                state: universe.as_ref(),
                                time,
                                size: (width, height),
                                camera,
                                gizmo_mode,
                                gizmo_pivot,
                                generic_translate,
                                spans,
                            }) {
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
                                        delivered: false,
                                        score_ms: sample_ms,
                                        build_ms: stage.last_work.build_ms,
                                        pick_ms: stage.last_work.pick_ms,
                                        camera_radius: camera.radius,
                                        width,
                                        height,
                                        lit_cones,
                                        track_time_s: time,
                                        window_active,
                                        ui_frame_gap_ms: spans.frame_gap_ms,
                                        request_to_prepaint_ms: spans.request_to_prepaint_ms,
                                        renders_in_gap: spans.renders,
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
                                            stage.last_draw_ms = Some(completed.draw_ms);
                                            if let Some(timings) = completed.timings {
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
                                            // `None` rather than the last frame's
                                            // number: only one frame at a time is
                                            // profiled, and a repeated value belongs
                                            // to neither row.
                                            if let Some(timings) = completed.timings {
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
                                        // Submitted, nothing back. The row that
                                        // used to be missing entirely.
                                        _ => stage.previous.clone().map(|frame| (frame, None)),
                                    };
                                    if let Some(run_up) =
                                        stage.hitches.record(sample, Instant::now())
                                    {
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
                        #[cfg(target_os = "macos")]
                        StageFrame::Shared(surface) => {
                            window.paint_surface(bounds, surface.pixel_buffer());
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
            if let Some(state) = this.visualizer_mut() {
                if event.button == MouseButton::Left {
                    state.editor_press(at, shift);
                } else if let Some(drag) = Drag::of(event.button) {
                    state.drag = Some((drag, at));
                }
                cx.notify();
            }
        });
    });

    let dragged = app.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let at = event.position;
        let held = event.pressed_button;
        dragged.update(cx, |this, cx| {
            let Some(state) = this.visualizer_mut() else {
                return;
            };
            // The held button is the authority on whether a drag is live. Press
            // and release bookkeeping can only ever agree with it, so a stale
            // anchor cannot turn a hover into an orbit.
            let Some(held) = held else {
                state.drag = None;
                return;
            };
            if held == MouseButton::Left {
                if state.editor_drag.is_some() {
                    state.editor_moved(at);
                    cx.notify();
                }
                return;
            }
            let Some((drag, was)) = state.drag else {
                return;
            };
            state.drag = Some((drag, at));
            state.dragged(at - was);
            cx.notify();
        });
    });

    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        released.update(cx, |this, cx| {
            if let Some(state) = this.visualizer_mut() {
                if event.button == MouseButton::Left {
                    state.editor_release(event.position);
                } else {
                    state.drag = None;
                }
                cx.notify();
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
                state.dolly(zoom_scale(-wheel));
                cx.notify();
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
        .child(luma_ui::silkscreen(message.clone()))
        .agent_node(Role::Text, message)
        .into_any_element()
}

#[cfg(test)]
mod render_lab_tests {
    use super::*;

    #[test]
    fn pick_snapshots_follow_presented_serial_not_latest_submission() {
        let mut pairing = SerialPairing::default();
        pairing.submitted(1, "pixels-one", SubmitOutcome::Queued);
        pairing.submitted(2, "pixels-two", SubmitOutcome::Queued);
        pairing.submitted(
            3,
            "pixels-three",
            SubmitOutcome::Replaced { dropped_serial: 2 },
        );

        assert_eq!(pairing.presented(1), Some("pixels-one"));
        assert_eq!(pairing.presented(3), Some("pixels-three"));
        assert_eq!(pairing.presented(2), None);
    }

    #[test]
    fn presenting_newer_pixels_discards_stale_pick_worlds() {
        let mut pairing = SerialPairing::default();
        pairing.submitted(7, "old", SubmitOutcome::Queued);
        pairing.submitted(8, "displayed", SubmitOutcome::Queued);
        pairing.submitted(9, "future", SubmitOutcome::Queued);

        assert_eq!(pairing.presented(8), Some("displayed"));
        assert_eq!(pairing.presented(7), None);
        assert_eq!(pairing.presented(9), Some("future"));
    }

    #[test]
    fn render_lab_controls_are_independent_and_bounded() {
        let mut lab = RenderLab::new(true);
        let original_background = lab.background_color;

        // Out-of-range targets on purpose: a slider hands over whatever the
        // pointer asks for, and `set` is the only thing between that and the
        // renderer.
        lab.set(LabValue::SunColor(0), 0.65);
        lab.set(LabValue::SunShadowSoftness, 10.0);
        lab.set(LabValue::BackgroundColor(2), original_background[2] + 0.2);
        lab.set(LabValue::AmbientColor(1), 0.6);
        lab.set(LabValue::ProbeIntensity, 10.0);
        lab.set(LabValue::ProbeRotation, 225.0);
        lab.toggle(LabToggle::Probe);
        lab.toggle(LabToggle::ProbeVisible);
        lab.toggle(LabToggle::FixtureSurfaceLighting);
        lab.toggle(LabToggle::ClusterDebug);
        lab.set(LabValue::HazeSteps, 100.0);
        lab.set(LabValue::HazeResolution, -10.0);

        assert!((lab.sun_color[0] - 0.65).abs() < f32::EPSILON);
        assert_eq!(lab.sun_shadow_softness, 3.0);
        assert!((lab.background_color[2] - (original_background[2] + 0.2)).abs() < f32::EPSILON);
        assert!((lab.ambient_color[1] - 0.6).abs() < f32::EPSILON);
        assert_eq!(lab.background_color[..2], original_background[..2]);
        assert_eq!(lab.probe_intensity, 4.0);
        assert_eq!(lab.probe_rotation_deg, -135.0);
        assert!(lab.probe_enabled);
        assert!(lab.probe_visible);
        assert!(!lab.fixture_surface_lighting);
        assert!(lab.cluster_debug);
        assert_eq!(lab.haze_steps, 64);
        assert_eq!(lab.haze_resolution, 0.25);

        lab.set(LabValue::HazeSteps, -100.0);
        lab.set(LabValue::HazeResolution, 10.0);
        assert_eq!(lab.haze_steps, 1);
        assert_eq!(lab.haze_resolution, 1.0);
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

#[cfg(test)]
mod render_size_tests {
    use super::{RenderSize, SIZE_HOLD_FRAMES};

    /// A `SWEEP` slide at display rate, eased the way `motion::ROOT` is: the
    /// stream of sizes one ⌘B hands the stage.
    fn slide(frames: u32) -> impl Iterator<Item = (u32, u32)> {
        (1..=frames).map(move |frame| {
            let t = f64::from(frame) / f64::from(frames);
            let eased = 1.0 - (1.0 - t).powi(3);
            (2560 - (eased * 256.0) as u32, 1440)
        })
    }

    /// Nothing on screen to protect, so nothing to wait for.
    #[test]
    fn the_first_size_is_adopted_at_once() {
        let mut size = RenderSize::default();
        assert_eq!(size.settle((1920, 1080)), (1920, 1080));
        assert!(size.pending.is_none());
    }

    /// The whole point. Without the hold the renderer reallocates once per
    /// frame of the slide; the claim is a bound on that count, not zero — see
    /// [`SIZE_HOLD_FRAMES`] for why no count can promise zero.
    #[test]
    fn a_slide_costs_a_bounded_number_of_redraws_not_one_per_frame() {
        let mut size = RenderSize::default();
        size.settle((2560, 1440));
        let mut drawn = Vec::new();
        let mut last = (2560, 1440);
        for observed in slide(32) {
            let at = size.settle(observed);
            if at != last {
                drawn.push(at);
            }
            last = at;
        }
        assert!(
            drawn.len() <= 2,
            "the stage redrew {} times during one slide: {drawn:?}",
            drawn.len()
        );
        // And whatever it did redraw at was within a hair of the destination,
        // so the reallocations it spent were not wasted on the way past.
        for (width, _) in &drawn {
            assert!(
                (2304..=2308).contains(width),
                "redrew at {width}, far from where the slide was going"
            );
        }
    }

    /// And the destination does arrive, which is what stops a held size from
    /// being a permanently stretched picture.
    #[test]
    fn a_settled_size_arrives_after_the_hold() {
        let mut size = RenderSize::default();
        size.settle((2560, 1440));
        for _ in 1..SIZE_HOLD_FRAMES {
            assert_eq!(size.settle((2304, 1440)), (2560, 1440));
            assert!(size.pending.is_some(), "the hold must keep asking to end");
        }
        assert_eq!(size.settle((2304, 1440)), (2304, 1440));
        assert!(size.pending.is_none());
        // Adopted, so the steady state costs no countdown at all.
        assert_eq!(size.settle((2304, 1440)), (2304, 1440));
        assert!(size.pending.is_none());
    }

    /// A slide reversed mid-flight — ⌘B twice in quick succession — ends where
    /// it started, and the stage never redrew at anything between.
    #[test]
    fn a_reversed_slide_costs_no_resize_at_all() {
        let mut size = RenderSize::default();
        size.settle((2560, 1440));
        for width in [2540, 2480, 2400, 2360, 2400, 2480, 2540, 2560] {
            assert_eq!(size.settle((width, 1440)), (2560, 1440));
        }
        assert!(
            size.pending.is_none(),
            "arriving back at the drawn size must clear the countdown"
        );
    }
}
