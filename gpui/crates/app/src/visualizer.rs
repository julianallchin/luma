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

mod settings;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4, Quat, Vec2, Vec3};
use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Context, Corners, DispatchPhase, Div, Entity,
    Hitbox, HitboxBehavior, ImageId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, RenderImage, ScrollWheelEvent, Window,
};
use luma_lib::models::universe::UniverseState;
use luma_lib::stage_render;
use luma_render::{
    assets, build_frame_with, coords, frame::EditorObject, house, scene_desc,
    scene_desc::VenueEnvironment, AsyncViewport, FrameTimings, MetricSummary, SubmitOutcome,
};
use luma_scene::{
    apply_rotation, apply_translation, bvh::MeshSource, gizmo_scale, Aabb, Camera, ClickOrbit,
    ClickOrbitRelease, ClickOrbitUpdate, Framing, GizmoHandle, GizmoMode, Insets, Marquee,
    MaterialHandle, MeshHandle, NodeContent, NodeFlags, PivotMode, SceneGraph, Selection,
    Transform, TransformTarget, TriMesh, View, Viewfinder,
};
use luma_ui::ladder;
use luma_ui::node::{agent_paint_node, Instrument, Role};

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
pub(crate) const DOLLY_IN: f32 = 0.8;
pub(crate) const DOLLY_OUT: f32 = 1.25;

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

/// The hit-test geometry itself: what a ray can hit and which authored object
/// each hit names.
///
/// Separated from [`PickSnapshot`] because it is the expensive half and the
/// half that almost never changes. A playing score moves colour through a
/// static rig: the same meshes at the same transforms, frame after frame.
/// Rebuilding this per frame cost more than the whole rest of the UI thread's
/// share of a frame, so it is built once and shared by every snapshot whose
/// draw list still matches [`Self::draws`].
struct PickGeometry {
    graph: SceneGraph,
    meshes: Vec<Arc<TriMesh>>,
    /// Frame node → the authored object it draws. Identity is the *authored
    /// id*, never the node index: an index names whatever inherited the slot
    /// after a re-solve, which is how deleting a piece used to leave the
    /// selection pointing at some other object.
    objects: Vec<Option<EditorObject>>,
    ordered: Vec<EditorObject>,
    /// Where each object is, for the marquee to test against.
    anchors: HashMap<EditorObject, Vec3>,
    /// Geometry bounds in camera world space, including every draw of an object.
    bounds: HashMap<EditorObject, Aabb>,
    /// The mesh set this was built against, by key: a mesh index means nothing
    /// on its own, and a re-banked frame can keep an index while changing what
    /// it points at.
    mesh_keys: Vec<String>,
    /// The picked draws this was built from, in order — mesh, transform and
    /// the object the draw names. Every input the build reads is in here, so
    /// an equal list is the same geometry.
    draws: Vec<(usize, Mat4, EditorObject)>,
}

impl PickGeometry {
    /// Whether `frame` would build exactly this geometry again.
    ///
    /// Compared in place against the stored draw list rather than by building
    /// a fresh key: the whole point is to touch no allocator on the frames
    /// that match, which is nearly all of them.
    fn matches(&self, frame: &luma_render::Frame) -> bool {
        if self.mesh_keys.len() != frame.meshes.len()
            || !self
                .mesh_keys
                .iter()
                .zip(&frame.meshes)
                .all(|(key, mesh)| key == &mesh.key)
        {
            return false;
        }
        let mut resident = self.draws.iter();
        for draw in picked_draws(frame) {
            let Some(object) = draw.editor_object.as_ref() else {
                continue;
            };
            let Some((mesh, model, was)) = resident.next() else {
                return false;
            };
            if *mesh != draw.mesh || *model != draw.model || was != object {
                return false;
            }
        }
        resident.next().is_none()
    }
}

/// The opaque draws a ray can hit: transparent draws trail the opaque ones and
/// are grid, compass and cables, none of which is an authored object.
fn picked_draws(frame: &luma_render::Frame) -> &[luma_render::frame::Draw] {
    &frame.draws[..frame.draws.len().saturating_sub(frame.transparent.len())]
}

/// CPU geometry and authored provenance for one submitted render frame.
/// It is immutable after submission and only becomes interactive when the
/// presentation carrying the same serial becomes the displayed image.
struct PickSnapshot {
    camera: Camera,
    geometry: Arc<PickGeometry>,
    /// The pivot the frame drew its gizmo on, carried over from
    /// [`luma_render::Frame::gizmo_pivot`] so a press tests the widget that is
    /// actually on screen.
    gizmo_pivot: Option<Vec3>,
    gizmo_space: luma_scene::gizmo::GizmoSpace,
}

/// What survives between frames so the geometry above can be reused: the
/// per-asset BVHs and the last geometry built from them.
#[derive(Default)]
struct PickCache {
    /// Immutable per-asset CPU BVHs; frame snapshots only carry Arc handles.
    meshes: HashMap<String, Arc<TriMesh>>,
    geometry: Option<Arc<PickGeometry>>,
}

impl MeshSource for PickSnapshot {
    fn mesh(&self, handle: MeshHandle) -> Option<&TriMesh> {
        self.geometry
            .meshes
            .get(handle.0 as usize)
            .map(AsRef::as_ref)
    }
}

impl PickSnapshot {
    fn from_frame(
        frame: &luma_render::Frame,
        scene: &scene_desc::Scene,
        camera: Camera,
        cache: &mut PickCache,
    ) -> Self {
        let geometry = match cache
            .geometry
            .take()
            .filter(|resident| resident.matches(frame))
        {
            Some(resident) => resident,
            None => Arc::new(PickGeometry::build(frame, scene, &mut cache.meshes)),
        };
        cache.geometry = Some(Arc::clone(&geometry));
        Self {
            camera,
            geometry,
            gizmo_pivot: frame.gizmo_pivot,
            gizmo_space: scene.editor.gizmo_space,
        }
    }

    fn ray(&self, at: Vec2, viewport: Vec2) -> luma_scene::Ray {
        let ndc = Vec2::new(
            at.x / viewport.x.max(1.0) * 2.0 - 1.0,
            1.0 - at.y / viewport.y.max(1.0) * 2.0,
        );
        self.camera.ray(ndc, viewport.x / viewport.y.max(1.0))
    }

    fn pick(&self, at: Vec2, viewport: Vec2) -> Option<EditorObject> {
        self.geometry
            .graph
            .raycast(self.ray(at, viewport), Default::default(), self)
            .into_iter()
            .find_map(|hit| self.geometry.objects.get(hit.node.0 as usize)?.clone())
    }

    fn marquee(&self, marquee: Marquee, viewport: Vec2) -> Vec<EditorObject> {
        self.geometry
            .ordered
            .iter()
            .filter(|object| {
                self.geometry
                    .anchors
                    .get(*object)
                    .is_some_and(|anchor| marquee.contains_world(&self.camera, viewport, *anchor))
            })
            .cloned()
            .collect()
    }
}

impl PickGeometry {
    fn build(
        frame: &luma_render::Frame,
        scene: &scene_desc::Scene,
        cache: &mut HashMap<String, Arc<TriMesh>>,
    ) -> Self {
        let meshes: Vec<Arc<TriMesh>> = frame
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
        let mut objects: Vec<Option<EditorObject>> = Vec::new();
        let mut anchors = HashMap::new();
        let mut bounds: HashMap<EditorObject, Aabb> = HashMap::new();
        let mut ordered = Vec::new();
        let mut picked = Vec::new();
        for draw in picked_draws(frame) {
            let Some(object) = draw.editor_object.clone() else {
                continue;
            };
            picked.push((draw.mesh, draw.model, object.clone()));
            let mesh_bounds = meshes[draw.mesh].bounds();
            if !mesh_bounds.is_empty() {
                let draw_bounds = Aabb::from_points(
                    mesh_bounds
                        .corners()
                        .map(|corner| draw.model.transform_point3(corner)),
                );
                bounds
                    .entry(object.clone())
                    .or_insert(Aabb::EMPTY)
                    .union(&draw_bounds);
            }
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
            // First draw of an object is its anchor; every draw names it.
            if !anchors.contains_key(&object) {
                ordered.push(object.clone());
                anchors.insert(
                    object.clone(),
                    object_pose(scene, &object).map_or(translation, |pose| pose.anchor),
                );
            }
            if objects.len() <= node.0 as usize {
                objects.resize(node.0 as usize + 1, None);
            }
            objects[node.0 as usize] = Some(object);
        }
        graph.update_world_transforms();
        Self {
            graph,
            meshes,
            objects,
            ordered,
            anchors,
            bounds,
            mesh_keys: frame.meshes.iter().map(|mesh| mesh.key.clone()).collect(),
            draws: picked,
        }
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
    Live,
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
    pub(crate) venue_id: String,
    venue_name: String,
    /// The score lighting the rig, when one is. Compared alongside
    /// [`Self::venue_id`] so moving between scores — of one track or of two —
    /// re-composites instead of tearing the stage down.
    subject: Option<Lit>,
    graph_preview: Option<crate::graph::preview::View>,
    /// The score whose composite has actually *landed* on the render engine.
    ///
    /// Distinct from [`Self::subject`], which is what this stage has asked
    /// for: an install is a round trip, and between the ask and the answer the
    /// rig is still lit by the score before it. The toolbar reads this one, so
    /// what it names is what is on the light rather than what was intended.
    lit: Option<Lit>,
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
    selection: Selection<EditorObject>,
    gizmo_mode: GizmoMode,
    /// The gizmo handle under the pointer, or the one being dragged — lit in
    /// the picture so the hand knows what it is about to grab. Recomputed on
    /// every unbuttoned move over the viewport.
    gizmo_hover: Option<GizmoHandle>,
    /// The element's size as the last frame laid it out. Three's orbit rates
    /// are all per element *height*, and a rate needs the height a frame
    /// earlier than prepaint can supply it.
    size: gpui::Size<Pixels>,
    viewport_origin: Point<Pixels>,
    /// The preview environment, shared by venue editing and score playback.
    venue_environment: VenueEnvironment,
    render_lab: RenderLab,
    pub(crate) settings_open: bool,
    presentation: bool,
    bottom_bar_visible: bool,
    settings_motion: RefCell<settings::DockMotion>,
    selection_motion: SelectionMotion,
    environment_error: Option<String>,
    environment_saving: bool,
    environment_edited: bool,
    environment_pending: Rc<RefCell<Option<VenueEnvironment>>>,
    /// Whether the FPS readout is unfolded into the full frame-stats panel.
    fps_expanded: bool,
    /// The builder. `None` until the rig lands, and the one place a position
    /// is editable in this app — see [`crate::stage`].
    pub(crate) build: Option<crate::stage::Build>,
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
    /// Where the viewport sits in the window, as of the last layout.
    ///
    /// Recorded by a measuring element that is mounted whether or not there is
    /// a renderer, because the builder's own layer projects sockets into these
    /// bounds and must work with the device off — [`Self::displayed_pick`] and
    /// [`Visualizer::viewport_origin`] both exist only once a frame has been
    /// drawn.
    pane: Bounds<Pixels>,
    /// Measured card size; short cards align with the object just like tall ones.
    selection_card_size: gpui::Size<Pixels>,
}

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
struct IdleKey {
    /// `f32::to_bits` — an equality key wants exactness, not float semantics.
    time_bits: u32,
    camera: Camera,
    size: (u32, u32),
    /// The whole lab, its `open` flag normalised out — panel visibility draws
    /// nothing.
    lab: RenderLab,
    selected: Vec<String>,
    selected_pieces: Vec<String>,
    gizmo_mode: GizmoMode,
    gizmo_hover: Option<GizmoHandle>,
    universe: Option<UniverseState>,
}

/// Frames of unchanged inputs the temporal haze needs before its blue-noise
/// integration is visually converged and the stage may rest.
const SETTLE_FRAMES: u32 = 16;

/// The most pixels the stage renders, whatever the size of its element.
///
/// The live renderer costs per pixel: fullscreen 3600×2260 took 2.3× the GPU
/// time of the 1984×1511 window and fell to 27 fps. Past this budget the stage
/// renders smaller at the same aspect and the compositor upscales it (MetalFX
/// Spatial on macOS — see `Window::paint_upscaled_surface`). The budget is the
/// reference frame the renderer's timings are held to, so an ordinary window
/// stays native.
const RENDER_BUDGET_PIXELS: f64 = 2227.0 * 1391.0;

/// How the stage's render size follows its element's physical size.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RenderScale {
    /// Native up to [`RENDER_BUDGET_PIXELS`], scaled down past it.
    Budget,
    /// Always this fraction of the element — `LUMA_RENDER_SCALE`, 0.25..=1.
    Fixed(f32),
    /// Always the element's own size — `LUMA_RENDER_BUDGET=off`.
    Native,
}

impl RenderScale {
    /// Read once per process; the choice is logged.
    fn from_env() -> Self {
        static SCALE: std::sync::OnceLock<RenderScale> = std::sync::OnceLock::new();
        *SCALE.get_or_init(|| {
            let scale = Self::parse(
                std::env::var("LUMA_RENDER_SCALE").ok().as_deref(),
                std::env::var("LUMA_RENDER_BUDGET").ok().as_deref(),
            );
            eprintln!("[stage] render size: {scale:?}");
            scale
        })
    }

    /// `LUMA_RENDER_SCALE` wins over `LUMA_RENDER_BUDGET`. A value that does
    /// not parse is reported and ignored.
    fn parse(scale: Option<&str>, budget: Option<&str>) -> Self {
        if let Some(scale) = scale {
            match scale.trim().parse::<f32>() {
                Ok(value) if value.is_finite() => return Self::Fixed(value.clamp(0.25, 1.0)),
                _ => eprintln!("[stage] ignoring LUMA_RENDER_SCALE={scale:?}; expected 0.25..1"),
            }
        }
        match budget {
            Some("off") => Self::Native,
            Some(other) => {
                eprintln!("[stage] ignoring LUMA_RENDER_BUDGET={other:?}; expected off");
                Self::Budget
            }
            None => Self::Budget,
        }
    }

    /// The size to render an element of `physical` device pixels at: the
    /// same aspect, whole pixels, never larger than the element.
    fn size(self, (width, height): (u32, u32)) -> (u32, u32) {
        let scale = match self {
            Self::Native => 1.0,
            Self::Fixed(scale) => f64::from(scale),
            Self::Budget => (RENDER_BUDGET_PIXELS / (f64::from(width) * f64::from(height)))
                .sqrt()
                .min(1.0),
        };
        let fit = |side: u32| ((f64::from(side) * scale).round() as u32).clamp(1, side.max(1));
        (fit(width), fit(height))
    }
}

/// What a finished press owes the app — see [`Visualizer::editor_release`].
pub(crate) enum ReleaseAct {
    /// A click while the hand was placing: aim there and drop.
    Place(Point<Pixels>),
    /// A gizmo drag ended on these stage pieces: write their previewed poses
    /// into the graph.
    CommitPose(Vec<String>),
}

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
        /// The widget's world pivot, as it stood when the press landed. Held
        /// rather than re-read: the live one moves with the objects, and a
        /// rotation of the press-time poses about a pivot that has since
        /// travelled is a different rotation every frame.
        pivot: Vec3,
        camera: Camera,
        space: luma_scene::gizmo::GizmoSpace,
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
    /// The room's environment, unchanged by score playback.
    ///
    /// Every field below it is this value's *fill*, resolved once through
    /// [`house::fill`] and then free to be overridden by hand. This one is not
    /// a fill: it is what hangs the house rig and what supplies the sky, both
    /// of which the frame builder resolves for itself against the room's
    /// bounds, so it is carried whole rather than flattened into dials.
    house: VenueEnvironment,
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
    geometry_shadows: bool,
    cluster_debug: bool,
    haze_enabled: bool,
    haze_density: f32,
    haze_appearance: scene_desc::HazeAppearance,
    haze_steps: u32,
    haze_resolution: f32,
    grid_enabled: bool,
    gizmos_enabled: bool,
    debug_view: scene_desc::DebugView,
}

/// [`scene_desc::DirectionalLight::EDITOR`]'s direction as the lab's azimuth
/// and elevation, in degrees. The lab authors a sun in polar form because that
/// is what a pair of sliders can hold; the catalogue authors it as a vector.
/// One of the two has to convert, and it is this one, because the vector is
/// the shared contract.
fn editor_sun_angles() -> (f32, f32) {
    let [x, y, z] = scene_desc::DirectionalLight::EDITOR.direction;
    (y.atan2(x).to_degrees(), z.atan2(x.hypot(y)).to_degrees())
}

impl Visualizer {
    pub(crate) fn render_settings(&self) -> scene_desc::RenderSettings {
        self.render_lab.settings(self.camera.fov_y_deg)
    }
}

impl RenderLab {
    /// One settings snapshot for the main viewport and both picker previews.
    fn settings(&self, fov: f32) -> scene_desc::RenderSettings {
        let mut render = scene_desc::RenderSettings::room(self.house, fov, self.haze_resolution);
        render.fov = fov;
        render.environment = scene_desc::Environment {
            background: if self.environment_enabled {
                self.background_color
            } else {
                scene_desc::Environment::DARK.background
            },
            ambient_color: self.ambient_color,
            ambient_intensity: self.ambient_intensity,
            probe: self.probe_enabled.then(|| scene_desc::EnvironmentProbe {
                asset: "environments/studio.hdr".into(),
                intensity: self.probe_intensity,
                rotation_deg: self.probe_rotation_deg,
                visible: self.probe_visible,
            }),
        };
        render.sun = self.sun_enabled.then_some(scene_desc::DirectionalLight {
            direction: self.sun_direction(),
            color: self.sun_color,
            intensity: self.sun_intensity,
            shadows: self.sun_shadows,
            shadow_softness: self.sun_shadow_softness,
        });
        // The room's own two halves, which are not fill
        // dials and so are not the lab's to override: the
        // house rig the frame builder hangs once it knows
        // the bounds, and the atmosphere an open-air venue
        // takes its background, ambient and sun from.
        render.house = Some(self.house);
        render.sky = house::fill(self.house).sky;
        render.haze.enabled = self.haze_enabled;
        render.haze.density = self.haze_density;
        render.haze.appearance = self.haze_appearance;
        render.haze.steps = self.haze_steps;
        render.haze.resolution = self.haze_resolution;
        render.show_grid = self.grid_enabled;
        render.show_gizmos = self.gizmos_enabled;
        render.debug_view = self.debug_view;
        render.fixture_surface_lighting = self.fixture_surface_lighting;
        render.fixture_shadows = self.fixture_shadows;
        render.geometry_shadows = self.geometry_shadows;
        render.cluster_debug = self.cluster_debug;
        render
    }

    fn new(environment: VenueEnvironment) -> Self {
        let (azimuth_deg, elevation_deg) = editor_sun_angles();
        let mut lab = Self {
            house: environment,
            sun_enabled: true,
            // The editor key light, in the lab's own polar spelling. Read off
            // `DirectionalLight::EDITOR` rather than typed again: the const and
            // the three numbers that used to sit here were the same light said
            // twice, and only one of them moved when the room was made
            // legible — which is why the golden brightened and the page it is
            // a golden *of* did not.
            sun_azimuth_deg: azimuth_deg,
            sun_elevation_deg: elevation_deg,
            sun_intensity: scene_desc::DirectionalLight::EDITOR.intensity,
            sun_color: scene_desc::DirectionalLight::EDITOR.color,
            sun_shadows: true,
            sun_shadow_softness: 1.0,
            // A pure dev override, on by default: what the background and the
            // ambient term actually *are* is the environment's answer, written
            // by `set_environment` below.
            environment_enabled: true,
            background_color: scene_desc::Environment::EDITOR.background,
            ambient_color: scene_desc::Environment::EDITOR.ambient_color,
            ambient_intensity: scene_desc::Environment::EDITOR.ambient_intensity,
            probe_enabled: false,
            probe_intensity: 0.8,
            probe_rotation_deg: 0.0,
            probe_visible: false,
            fixture_surface_lighting: true,
            fixture_shadows: true,
            geometry_shadows: true,
            cluster_debug: false,
            haze_enabled: true,
            haze_density: 0.24,
            haze_appearance: scene_desc::HazeAppearance::default(),
            haze_steps: 8,
            haze_resolution: luma_render::LIVE_HAZE_RESOLUTION,
            grid_enabled: true,
            gizmos_enabled: true,
            debug_view: scene_desc::DebugView::Pbr,
        };
        lab.set_environment(environment);
        lab
    }

    /// Re-derive the lighting from the environment the room is now drawn
    /// under.
    ///
    /// Exactly the fields [`house::fill`] answers, and no others: everything
    /// else in the lab is an authored tweak, and a stage that reset a hand-set
    /// haze density because the track changed would be throwing away work
    /// nobody asked it to.
    ///
    /// The grid goes with the room being visible at all. It is editor chrome
    /// laid on the floor, and a dark stage is the one picture whose whole
    /// point is that the floor is not lit.
    fn set_environment(&mut self, environment: VenueEnvironment) {
        let fill = house::fill(environment);
        self.house = environment;
        self.background_color = fill.environment.background;
        self.ambient_color = fill.environment.ambient_color;
        self.ambient_intensity = fill.environment.ambient_intensity;
        self.sun_enabled = fill.sun.is_some();
        self.sun_intensity = fill
            .sun
            .map_or(scene_desc::DirectionalLight::EDITOR.intensity, |sun| {
                sun.intensity
            });
        self.grid_enabled = fill.sun.is_some() || fill.sky.is_some();
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
}

#[derive(Clone, Copy)]
enum LabToggle {
    FixtureShadows,
    Haze,
    Grid,
    Gizmos,
}
#[derive(Clone, Copy)]
enum LabValue {
    HazeDensity,
    Cloudiness,
    CloudSize,
    Turbulence,
    WindSpeed,
    WindDirection,
}
const MAX_HAZE_DENSITY: f32 = 0.5;

impl RenderLab {
    fn toggle(&mut self, control: LabToggle) {
        let value = match control {
            LabToggle::FixtureShadows => &mut self.fixture_shadows,
            LabToggle::Haze => &mut self.haze_enabled,
            LabToggle::Grid => &mut self.grid_enabled,
            LabToggle::Gizmos => &mut self.gizmos_enabled,
        };
        *value = !*value;
    }
    fn set(&mut self, control: LabValue, value: f32) {
        match control {
            LabValue::HazeDensity => self.haze_density = value.clamp(0.0, MAX_HAZE_DENSITY),
            LabValue::Cloudiness => self.haze_appearance.cloudiness = value,
            LabValue::CloudSize => self.haze_appearance.cloud_size = value,
            LabValue::Turbulence => self.haze_appearance.turbulence = value,
            LabValue::WindSpeed => self.haze_appearance.wind_speed = value,
            LabValue::WindDirection => self.haze_appearance.wind_direction = value,
        }
        self.haze_appearance = self.haze_appearance.sanitized();
    }
}

impl Visualizer {
    /// Open the view on a venue and start its load.
    ///
    /// `subject` is the score that should light the rig, when the view was
    /// opened over a track editor. Compositing it is a dispatch command like
    /// any other; what is *not* a command is the per-frame sample that follows
    /// — see [`Library::sample_universe`].
    pub(crate) fn open(
        library: &Library,
        venue_id: &str,
        venue_name: String,
        subject: Option<Lit>,
        cx: &mut Context<Luma>,
    ) -> Self {
        let rig = library.venue_rig(venue_id);
        // The venue's own environment arrives with its rig; until then the
        // default room, which is the picture this app has always opened with.
        let environment = VenueEnvironment::default();
        let composite = subject
            .clone()
            .map(|lit| (library.composite_score(&lit.score, None), lit));
        let venue = venue_id.to_string();
        cx.spawn(async move |this, cx| {
            // The composite first: it is what makes the sample non-empty, and
            // a rig that appeared before its light would flash dark.
            let mut installed = None;
            if let Some((composite, lit)) = composite {
                if composite.await.is_ok() {
                    installed = Some(lit);
                }
            }
            let loaded = rig.await;
            this.update(cx, |this, cx| {
                // Addressed to the room, not merely to whatever stage is up: a
                // rig landing after the eye moved to another venue must not
                // paint into the stage that replaced it.
                let Some(state) = this.visualizer.as_mut().filter(|it| it.venue_id == venue) else {
                    return;
                };
                state.composite_landed(installed);
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
            lit: None,
            graph_preview: None,
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
            gizmo_hover: None,
            size: gpui::Size::default(),
            viewport_origin: Point::default(),
            venue_environment: VenueEnvironment::default(),
            render_lab: RenderLab::new(environment),
            settings_open: false,
            presentation: false,
            bottom_bar_visible: false,
            settings_motion: RefCell::new(settings::DockMotion::new(cx.reduce_motion())),
            selection_motion: SelectionMotion::new(cx.reduce_motion()),
            environment_error: None,
            environment_saving: false,
            environment_edited: false,
            environment_pending: Rc::default(),
            fps_expanded: false,
            build: None,
            stage: Rc::default(),
        }
    }

    /// The environment edited by View and used by every preview in this venue.
    pub(crate) fn venue_environment(&self) -> VenueEnvironment {
        self.venue_environment
    }

    /// Adopt an edited environment without waiting for the write to land.
    ///
    /// The lab is re-derived from it, which is what puts the new light on
    /// screen: its `house` field rides in the idle key, so a resting viewport
    /// wakes for the change rather than re-presenting the room it had.
    pub(crate) fn set_venue_environment(&mut self, environment: VenueEnvironment) {
        self.venue_environment = environment;
        self.render_lab.set_environment(self.environment());
    }

    /// Preview and authoring use the same saved room. Selecting a score
    /// changes its fixtures' output, not the time of day or the house lights.
    fn environment(&self) -> VenueEnvironment {
        self.venue_environment
    }

    /// Light this stage with a different score, without rebuilding it.
    ///
    /// Moving between scores inside one room changes only *what is composited*
    /// — the rig, the camera framing and the GPU are all still about the same
    /// venue. Tearing the stage down to re-light it would drop the device and
    /// re-frame the camera, so a glance between two documents would restart the
    /// view. Compositing is a dispatch command like any other; the per-frame
    /// sample that follows is not (see [`Library::sample_universe`]).
    fn relight(&mut self, library: &Library, subject: Option<Lit>, cx: &mut Context<Luma>) {
        self.subject = subject.clone();
        self.render_lab.set_environment(self.environment());
        let Some(lit) = subject else {
            // Nothing on screen is about a score any more, so the stage stops
            // claiming one. What the engine still holds is stale and unread —
            // the lab has already gone back to editor lighting.
            self.lit = None;
            return;
        };
        let composite = library.composite_score(&lit.score, None);
        cx.spawn(async move |this, cx| {
            let landed = composite.await.is_ok().then_some(lit);
            this.update(cx, |this, cx| {
                if let Some(state) = this.visualizer.as_mut() {
                    state.composite_landed(landed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Adopt an install that has come back, unless the eye moved on while it
    /// was in flight.
    ///
    /// Two composites can be outstanding at once — a fast one issued after a
    /// slow one — and the later *answer* is not necessarily the later *ask*.
    /// Checking against the current subject is what keeps the readout from
    /// naming a score the stage has already left.
    fn composite_landed(&mut self, landed: Option<Lit>) {
        if landed.is_some() && landed != self.subject {
            return;
        }
        if let Some(landed) = landed {
            self.lit = Some(landed);
        }
    }

    /// A rig re-read after a graph edit. The camera stays where the operator
    /// left it — a builder that re-framed on every placement would throw the
    /// view away once per piece.
    pub(crate) fn rig_reloaded(&mut self, loaded: Result<Rig, LibraryError>) {
        let framing = self.framing.clone();
        let camera = self.camera;
        self.rig_loaded(loaded);
        self.framing = framing;
        self.camera = camera;
        self.owes_opening_pose = false;
    }

    fn rig_loaded(&mut self, loaded: Result<Rig, LibraryError>) {
        let rig = match loaded {
            Ok(rig) => rig,
            Err(error) => {
                self.status = Status::Empty(error.to_string());
                return;
            }
        };
        // The builder first, and whether or not there is anything to draw: an
        // empty room is exactly the room a builder is for, and a page that
        // waited for a piece before offering a palette could never place the
        // first one.
        match self.build.as_mut() {
            Some(build) => build.adopt(&rig),
            None => {
                // The same supply the backend solves with, fixture bundle
                // included: a builder that could not measure a housing would
                // preview every light half-buried in the truss it is being
                // dropped on.
                let fixtures_root = crate::library::fixtures_root().ok();
                self.build = luma_render::catalog::VenueSockets::load(
                    stage_render::meshes_root(fixtures_root.as_deref()),
                    std::sync::Arc::new(luma_lib::venue_graph::BundledFixtures(
                        fixtures_root.unwrap_or_default(),
                    )),
                )
                .ok()
                .and_then(|sockets| crate::stage::Build::new(&self.venue_id, &rig, sockets));
            }
        }
        // No early return for an empty rig. A venue with nothing in it is
        // exactly the venue the builder exists to fill, and returning here left
        // it with no scene — so no floor, no grid, and nothing to put a first
        // piece *on*. The room is built whether or not anything is patched;
        // an empty rig is no reason to draw an empty pane.
        let definitions: BTreeMap<_, _> = rig
            .definitions
            .iter()
            .map(|(path, def)| (path.clone(), stage_render::definition(def)))
            .collect();
        if !self.environment_edited {
            self.venue_environment = rig.environment;
        }
        self.render_lab.set_environment(self.environment());
        let scene = scene(&rig, &definitions, self.environment());
        self.framing = scene.framing(&definitions);
        self.camera = opening_camera(&self.framing, &self.view_finder());
        self.owes_opening_pose = true;
        // A selection names authored objects; a reloaded scene may no longer
        // have some of them. Dropping the dead names here is what makes a
        // deleted piece *deselected* rather than a slot for the next pick to
        // misresolve.
        self.selection.retain(|object| match object {
            EditorObject::Fixture(id) => scene.fixtures.iter().any(|f| f.id.as_str() == id),
            EditorObject::StagePiece(id) => scene.pieces.iter().any(|p| p.id.as_str() == id),
        });
        let mut stage = self.stage.borrow_mut();
        stage.definitions = definitions;
        stage.scene = Some(scene);
        // A new scene is a frame owed. The idle gate's key deliberately does
        // not carry scene contents, so a resting viewport handed a reloaded
        // rig would keep re-presenting the room it had — a deleted piece
        // stayed on screen until a click or the camera changed the key.
        stage.idle = None;
        drop(stage);
        self.status = Status::Live;
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
            pick_cache: PickCache::default(),
            haze_started_at: Instant::now(),
        });
        true
    }

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
    /// lands, so it does when it contains controls. An empty toolbar earns no
    /// inset. The frame-stats readout is a box in the top-left
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
        let toolbar = if self.bottom_bar_visible {
            (f32::from(TOOLBAR_OVERLAY_BOTTOM) + f32::from(OVERLAY_BAND)) / h
        } else {
            0.
        };
        Viewfinder::new(FOV_Y_DEG, w / h).inset(Insets::vertical(0.0, toolbar))
    }

    /// Replace the selection outright — the redistribute round trip lands new
    /// fixture ids and the old ones no longer exist to keep.
    pub(crate) fn replace_selection(&mut self, targets: impl IntoIterator<Item = EditorObject>) {
        self.selection.replace(targets);
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Replace the selection with one object — the headless pick's write.
    pub(crate) fn select_object(&mut self, object: EditorObject) {
        self.selection.click(object, false);
    }

    /// The pointer's ray through the stage pane, off the camera alone — the
    /// aim a headless run has, where the displayed frame's own `PickSnapshot`
    /// does not exist. World space, unit direction not guaranteed.
    pub(crate) fn stage_pick_ray(&self, at: Point<Pixels>) -> Option<luma_scene::Ray> {
        let pane = self.stage.borrow().pane;
        let point = Vec2::new(
            f32::from(at.x - pane.origin.x),
            f32::from(at.y - pane.origin.y),
        );
        let viewport = Vec2::new(f32::from(pane.size.width), f32::from(pane.size.height));
        if viewport.x <= 1.0 || viewport.y <= 1.0 {
            return None;
        }
        let ndc = Vec2::new(
            point.x / viewport.x * 2.0 - 1.0,
            1.0 - point.y / viewport.y * 2.0,
        );
        Some(self.camera.ray(ndc, viewport.x / viewport.y))
    }

    fn selection_card_position(&self, size: gpui::Size<Pixels>) -> Point<Pixels> {
        let viewport = Vec2::new(f32::from(size.width), f32::from(size.height));
        let stage = self.stage.borrow();
        let camera = stage
            .displayed_pick
            .as_ref()
            .map_or(self.camera, |pick| pick.camera);
        let mut lo = Vec2::splat(f32::INFINITY);
        let mut hi = Vec2::splat(f32::NEG_INFINITY);
        if viewport.min_element() > 1.0 {
            for object in self.selection.selected() {
                let (EditorObject::Fixture(id) | EditorObject::StagePiece(id)) = object;
                let bounds = stage
                    .displayed_pick
                    .as_ref()
                    .and_then(|pick| pick.geometry.bounds.get(object).copied())
                    .or_else(|| self.build.as_ref().and_then(|build| build.room_bounds(id)));
                if let Some(bounds) = bounds {
                    for corner in bounds.corners() {
                        if (corner - camera.position()).dot(camera.target - camera.position())
                            <= 0.0
                        {
                            continue;
                        }
                        let ndc = camera.project(corner, viewport.x / viewport.y);
                        let p = Vec2::new(
                            (ndc.x + 1.0) * viewport.x * 0.5,
                            (1.0 - ndc.y) * viewport.y * 0.5,
                        );
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                }
            }
        }
        let measured = stage.selection_card_size;
        let card = Vec2::new(
            f32::from(measured.width).max(280.0),
            f32::from(measured.height).max(1.0),
        );
        let at = selection_card_at(lo, hi, viewport, card);
        Point::new(px(at.x), px(at.y))
    }

    /// Fit the selected objects' combined geometry bounds into the usable viewport.
    pub(crate) fn focus_selection(&mut self) -> bool {
        let mut bounds = Aabb::EMPTY;
        {
            let stage = self.stage.borrow();
            for object in self.selection.selected() {
                let (EditorObject::Fixture(id) | EditorObject::StagePiece(id)) = object;
                if let Some(object_bounds) = stage
                    .displayed_pick
                    .as_ref()
                    .and_then(|pick| pick.geometry.bounds.get(object).copied())
                    .or_else(|| self.build.as_ref().and_then(|build| build.room_bounds(id)))
                {
                    bounds.union(&object_bounds);
                }
            }
        }
        if bounds.is_empty() {
            return false;
        }
        let framing = Framing::of([], [bounds]);
        let direction = self.camera.position() - self.camera.target;
        self.camera = framing.fit(bounds.center(), direction, &self.view_finder());
        true
    }

    /// Dolly by a factor, shared by the toolbar, wheel and middle-button drag.
    pub(crate) fn dolly(&mut self, factor: f32) {
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
        if self.presentation {
            self.editor_drag = Some(EditorDrag::ClickOrbit {
                gesture: ClickOrbit::new(at),
                shift: false,
                start: at,
            });
            return;
        }
        let viewport = self.viewport_size();
        // A hand in the air owns the click's *meaning* (place, or nothing),
        // and the gizmo must not intercept it — but the gesture is still a
        // ClickOrbit, so a left drag mid-placement orbits the camera.
        let placing = self
            .build
            .as_ref()
            .is_some_and(|build| build.hand.owns_pointer());
        let stage = self.stage.borrow();
        let Some(pick) = stage.displayed_pick.as_ref().filter(|_| !placing) else {
            drop(stage);
            self.editor_drag = Some(EditorDrag::ClickOrbit {
                gesture: ClickOrbit::new(at),
                shift,
                start: at,
            });
            return;
        };
        if let Some(pivot) = pick.gizmo_pivot {
            let scale = gizmo_scale(
                (pick.camera.position() - pivot).length(),
                pick.camera.fov_y_deg,
            );
            if let Some(hit) = pick.gizmo_space.hit(
                pick.ray(at, viewport),
                pivot,
                scale,
                pick.camera.position() - pivot,
                self.gizmo_mode,
            ) {
                // What the widget may actually grab: fixtures, and a piece
                // whose joint leaves it free — a bolted piece's pose is a
                // relation, and axes over a relation is a widget that lies.
                let build = self.build.as_ref();
                let originals: Vec<_> = self
                    .selection
                    .selected()
                    .iter()
                    .filter(|object| match object {
                        EditorObject::Fixture(_) => true,
                        EditorObject::StagePiece(id) => {
                            build.is_some_and(|build| build.gizmo_space(id).is_some())
                        }
                    })
                    .filter_map(|object| {
                        let pose = stage
                            .scene
                            .as_ref()
                            .and_then(|scene| object_pose(scene, object))?;
                        Some((object.clone(), pose.position, pose.rotation))
                    })
                    .collect();
                // With no movable targets, the press remains a camera gesture.
                if !originals.is_empty() {
                    // The grabbed handle stays lit for the whole drag: the
                    // hover is only recomputed on unbuttoned moves.
                    self.gizmo_hover = Some(hit.handle);
                    self.editor_drag = Some(EditorDrag::Gizmo {
                        handle: hit.handle,
                        start: at,
                        pivot,
                        camera: pick.camera,
                        space: pick.gizmo_space,
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

    /// The gizmo handle under the pointer, tested against the frame on
    /// screen — the same cast [`Self::editor_press`] makes, asked on every
    /// unbuttoned move so the widget lights the handle *before* it is
    /// grabbed. Analytic against a handful of primitives, so it is cheap
    /// enough to ask per move.
    fn hover_gizmo(&self, point: Point<Pixels>) -> Option<GizmoHandle> {
        if self.presentation {
            return None;
        }
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        let stage = self.stage.borrow();
        let pick = stage.displayed_pick.as_ref()?;
        let pivot = pick.gizmo_pivot?;
        let scale = gizmo_scale(
            (pick.camera.position() - pivot).length(),
            pick.camera.fov_y_deg,
        );
        pick.gizmo_space
            .hit(
                pick.ray(at, viewport),
                pivot,
                scale,
                pick.camera.position() - pivot,
                self.gizmo_mode,
            )
            .map(|hit| hit.handle)
    }

    fn selection_gizmo_space(&self) -> luma_scene::gizmo::GizmoSpace {
        if self.presentation || self.selection.selected().is_empty() {
            return luma_scene::gizmo::GizmoSpace::DISABLED;
        }
        if let Some(build) = self.build.as_ref() {
            let mut spaces = self.selection.selected().iter().map(|object| match object {
                EditorObject::StagePiece(id) => build.gizmo_space(id),
                EditorObject::Fixture(_) => None,
            });
            spaces
                .next()
                .flatten()
                .filter(|space| spaces.all(|next| next == Some(*space)))
                .unwrap_or(luma_scene::gizmo::GizmoSpace::DISABLED)
        } else {
            luma_scene::gizmo::GizmoSpace::default()
        }
    }

    /// Switch which transform widget the selection wears — the toolbar's two
    /// segments and the stage page's `W`/`E` land here together.
    pub(crate) fn set_gizmo_mode(&mut self, mode: GizmoMode) {
        self.gizmo_mode = mode;
        self.gizmo_hover = None;
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
            gizmo @ EditorDrag::Gizmo { .. } => self.apply_gizmo(gizmo, at, viewport),
        }
        self.editor_drag = Some(interaction);
    }

    /// Ends the press. What comes back is the verb the release owes — a
    /// placement click, or a pose commit for dragged pieces — which the
    /// caller runs on [`Luma`], because writing the graph is a verb and not a
    /// camera gesture.
    fn editor_release(&mut self, point: Point<Pixels>) -> Option<ReleaseAct> {
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        // A release ends the camera drag whatever else this function decides:
        // the selection paths below can bail before reaching the end.
        self.drag = None;
        let interaction = self.editor_drag.take()?;
        if self.presentation {
            return None;
        }
        // Set only by a plain click on a fixture — the gesture that selects a
        // row. See the expansion at the tail.
        let mut clicked_fixture: Option<String> = None;
        // Only the placing hand's click is a drop. A run's controls are its
        // own card, so a click on the room mid-extend aims nothing.
        let placing = self
            .build
            .as_ref()
            .is_some_and(|build| build.hand.aims_with_pointer());
        let holding = self
            .build
            .as_ref()
            .is_some_and(|build| build.hand.owns_pointer());
        if let EditorDrag::ClickOrbit { ref gesture, .. } = interaction {
            if gesture.clone().released() == ClickOrbitRelease::Click && holding {
                return placing.then_some(ReleaseAct::Place(point));
            }
        }
        let stage = self.stage.borrow();
        let Some(pick) = stage.displayed_pick.as_ref() else {
            return None;
        };
        match interaction {
            EditorDrag::ClickOrbit { gesture, shift, .. }
                if gesture.released() == ClickOrbitRelease::Click =>
            {
                if let Some(target) = pick.pick(at, viewport) {
                    if !shift {
                        if let EditorObject::Fixture(id) = &target {
                            clicked_fixture = Some(id.clone());
                        }
                    }
                    self.selection.click(target, shift);
                } else if !shift {
                    self.selection.clear();
                }
            }
            EditorDrag::Marquee(marquee) => self.selection.replace(pick.marquee(marquee, viewport)),
            // A finished gizmo drag on stage pieces owes the graph its poses:
            // the drag previewed by writing the scene, and the scene is not
            // where a piece's position lives.
            EditorDrag::Gizmo { originals, .. } => {
                let pieces: Vec<String> = originals
                    .iter()
                    .filter_map(|(object, _, _)| match object {
                        EditorObject::StagePiece(id) => Some(id.clone()),
                        EditorObject::Fixture(_) => None,
                    })
                    .collect();
                return (!pieces.is_empty()).then_some(ReleaseAct::CommitPose(pieces));
            }
            _ => return None,
        }
        // The builder selects the *node*, not the render object: a subtree, a
        // trim and a detach are all things the graph has names for — and the
        // render object's identity IS the node id, which is what lets the two
        // agree by construction. After *every* gesture that can move the
        // selection — click and marquee alike — so the builder never holds a
        // second, staler answer to "what is selected".
        // A fixture clicked out of a row selects the row: the fixtures of one
        // model chained on the same face are one thing to a rigger, and the
        // card that follows edits them as one. A marquee is deliberate
        // multi-select and is left exactly as swept.
        let expanded: Vec<String> = clicked_fixture
            .as_deref()
            .map(|id| {
                let stage = self.stage.borrow();
                let path_of = |node: &str| {
                    stage.scene.as_ref().and_then(|scene| {
                        scene
                            .fixtures
                            .iter()
                            .find(|fixture| fixture.id == node)
                            .map(|fixture| fixture.fixture_path.clone())
                    })
                };
                self.build
                    .as_ref()
                    .map(|build| build.distribution_of(id, &path_of))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        if expanded.len() > 1 {
            if let Some(primary) = clicked_fixture.clone() {
                let mut ordered: Vec<String> = expanded
                    .iter()
                    .filter(|id| **id != primary)
                    .cloned()
                    .collect();
                ordered.push(primary);
                self.selection
                    .replace(ordered.into_iter().map(EditorObject::Fixture));
            }
        }
        if let Some(build) = self.build.as_mut() {
            let primary = self.selection.primary().map(|object| match object {
                EditorObject::Fixture(id) | EditorObject::StagePiece(id) => id.clone(),
            });
            build.select(primary);
            build.distribution = expanded;
        }
        None
    }

    /// Update only transient scene positions; the builder owns persistence on release.
    pub(crate) fn preview_stage_positions(&mut self, positions: &[(String, [f64; 3])]) {
        let mut stage = self.stage.borrow_mut();
        let Some(scene) = stage.scene.as_mut() else {
            return;
        };
        for (id, position) in positions {
            let position = position.map(|value| value as f32);
            if let Some(piece) = scene.pieces.iter_mut().find(|piece| &piece.id == id) {
                piece.pos = position;
            }
            if let Some(fixture) = scene.fixtures.iter_mut().find(|fixture| &fixture.id == id) {
                fixture.pos = position;
            }
        }
        // Position changes are intentionally absent from IdleKey. Numeric drags
        // do not set editor_drag, so invalidate the cached image explicitly.
        stage.idle = None;
    }

    /// One dragged piece's current *previewed* pose, in the socket layer's
    /// frame — what the gizmo wrote into the scene, read back so the commit
    /// inverts exactly what is on screen.
    pub(crate) fn piece_pose_three(&self, id: &str) -> Option<glam::DMat4> {
        let stage = self.stage.borrow();
        let piece = stage
            .scene
            .as_ref()?
            .pieces
            .iter()
            .find(|piece| piece.id == id)?;
        Some(coords::three_pose_from_data(piece.pos, piece.rot).as_dmat4())
    }

    /// The eye the room is being drawn through — and therefore the one the
    /// builder projects sockets with, so a mark and a pointer are talking
    /// about the same pixel.
    pub(crate) fn camera(&self) -> Camera {
        self.camera
    }

    /// The viewport's own laid-out bounds. Laid out — and recorded — whether
    /// or not a renderer exists, which is what a headless aim depends on.
    pub(crate) fn stage_pane(&self) -> Bounds<Pixels> {
        self.stage.borrow().pane
    }

    pub(crate) fn prepare_presentation(&mut self) {
        self.settings_open = false;
        self.drag = None;
        // A shortcut can arrive before mouse-up. Cancel an unfinished pose
        // preview before parking the editor, so it cannot survive without a commit.
        if let Some(EditorDrag::Gizmo { originals, .. }) = self.editor_drag.take() {
            let mut stage = self.stage.borrow_mut();
            if let Some(scene) = stage.scene.as_mut() {
                for (object, position, rotation) in originals {
                    set_object_pose(scene, &object, position, rotation);
                }
            }
            stage.idle = None;
        }
        self.gizmo_hover = None;
    }

    /// Where the pointer is aiming in the room, in the socket layer's frame:
    /// the mesh face under it when a frame is on screen, the floor plane when
    /// none is — which is the state a headless run is always in.
    pub(crate) fn stage_cursor(
        &self,
        at: Point<Pixels>,
    ) -> Option<(glam::DVec3, Option<crate::stage::hand::SurfaceHit>)> {
        // The pane's own bounds, not the canvas's: the pane is laid out — and
        // recorded — whether or not a renderer exists, and it is the surface
        // the beads and marks are projected against, so aiming through it is
        // what keeps a pointer and a bead talking about the same pixel.
        let pane = self.stage.borrow().pane;
        let point = Vec2::new(
            f32::from(at.x - pane.origin.x),
            f32::from(at.y - pane.origin.y),
        );
        let viewport = Vec2::new(f32::from(pane.size.width), f32::from(pane.size.height));
        if viewport.x <= 1.0 || viewport.y <= 1.0 {
            return None;
        }
        let stage = self.stage.borrow();
        if let Some(pick) = stage.displayed_pick.as_ref() {
            let ray = pick.ray(point, viewport);
            let hit = pick
                .geometry
                .graph
                .raycast(ray, Default::default(), pick)
                .into_iter()
                .find_map(|hit| {
                    match pick.geometry.objects.get(hit.node.0 as usize)?.as_ref()? {
                        EditorObject::StagePiece(id) => Some(crate::stage::hand::SurfaceHit {
                            piece: id.clone(),
                            point: coords::three_from_world(hit.point).as_dvec3(),
                            normal: coords::three_from_world(hit.face_normal)
                                .as_dvec3()
                                .normalize_or_zero(),
                        }),
                        EditorObject::Fixture(_) => None,
                    }
                });
            if let Some(hit) = hit {
                return Some((hit.point, Some(hit)));
            }
            return crate::stage::hand::floor_point(&ray).map(|world| (world, None));
        }
        let ndc = Vec2::new(
            f32::from(point.x) / viewport.x * 2.0 - 1.0,
            1.0 - f32::from(point.y) / viewport.y * 2.0,
        );
        crate::stage::hand::floor_point(&self.camera.ray(ndc, viewport.x / viewport.y))
            .map(|world| (world, None))
    }

    fn apply_gizmo(&mut self, drag: &EditorDrag, end: Vec2, viewport: Vec2) {
        let EditorDrag::Gizmo {
            handle,
            start,
            pivot,
            camera,
            space,
            originals,
        } = drag
        else {
            return;
        };
        let Some((translation, rotation)) =
            luma_scene::gizmo::drag_delta(*handle, *camera, *start, end, *pivot, viewport, *space)
        else {
            return;
        };
        let mut stage = self.stage.borrow_mut();
        let Some(scene) = stage.scene.as_mut() else {
            return;
        };
        // Every target rotates about the widget's own pivot: a rotation about a
        // point the operator cannot see is a rotation they did not ask for.
        let targets: Vec<_> = originals
            .iter()
            .map(|(_, position, rotation)| TransformTarget {
                position: *position,
                rotation: *rotation,
                anchor: *position,
            })
            .collect();
        for ((object, _, _), target) in originals.iter().zip(targets) {
            let changed = apply_translation(
                apply_rotation(target, rotation, *pivot, PivotMode::Group),
                translation,
            );
            set_object_pose(scene, object, changed.position, changed.rotation);
        }
    }
}

/// A continuously integrated version of the sidebar spring. Camera updates
/// change its destination without restarting its acceleration from rest.
struct SelectionMotion {
    position: Vec2,
    velocity: Vec2,
    target: Vec2,
    since: Option<Instant>,
    reduced: bool,
}

impl SelectionMotion {
    fn new(reduced: bool) -> Self {
        Self {
            position: Vec2::ZERO,
            velocity: Vec2::ZERO,
            target: Vec2::ZERO,
            since: None,
            reduced,
        }
    }

    fn sample(&mut self, target: Vec2, now: Instant) -> Vec2 {
        let Some(previous) = self.since.filter(|_| !self.reduced) else {
            self.position = target;
            self.velocity = Vec2::ZERO;
            self.target = target;
            self.since = Some(now);
            return target;
        };
        let elapsed = now.saturating_duration_since(previous).as_secs_f32();
        let duration = luma_ui::motion::span(&luma_ui::motion::SURFACE).as_secs_f32();
        for axis in 0..2 {
            let (offset, velocity) = luma_ui::motion::ROOT.advance(
                self.position[axis] - self.target[axis],
                self.velocity[axis],
                elapsed,
                duration,
            );
            self.position[axis] = self.target[axis] + offset;
            self.velocity[axis] = velocity;
        }
        self.since = Some(now);
        self.target = target;
        self.position
    }
}

/// Prefer beside the object's silhouette, flip sides when space runs out,
/// and keep the whole scrollable card reachable at viewport edges.
fn selection_card_at(lo: Vec2, hi: Vec2, viewport: Vec2, card: Vec2) -> Vec2 {
    let inset = Vec2::splat(12.0);
    let max = (viewport - card - inset).max(inset);
    if !lo.is_finite() || !hi.is_finite() {
        return Vec2::new(max.x, inset.y);
    }
    let right = hi.x + 16.0;
    let left = lo.x - card.x - 16.0;
    let x = if right <= max.x {
        right
    } else if left >= inset.x {
        left
    } else if viewport.x - hi.x >= lo.x {
        right
    } else {
        left
    };
    Vec2::new(x, (lo.y + hi.y - card.y) * 0.5).clamp(inset, max)
}

/// Data space to the world the renderer draws in, and back.
///
/// [`coords::three_pose_from_data`] is the whole conversion; this is the
/// rotation that takes its result the last step, exactly as `build_frame_with`
/// does for every draw. Spelled here so the two functions below are the only
/// place in this screen that knows a stored triple is not a world pose.
fn to_world() -> Mat4 {
    Mat4::from_mat3(coords::three_to_world_basis())
}

/// Where an editor object *is*, in the renderer's world space.
///
/// This and [`set_object_pose`] are the single data↔world boundary for the
/// editor: everything a gizmo touches — the camera, `Draw::model`, the
/// overlays, a drag delta measured on screen — is world space, and a stored
/// pose is neither that nor a rotation away from it. The swap is a mirror
/// (`coords`, module note), so a pose that skips it lands at `(x, -y, z)`:
/// mirrored across the room, and moving the wrong way when dragged.
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
    let (_, rotation, position) =
        (to_world() * coords::three_pose_from_data(pos, rot)).to_scale_rotation_translation();
    Some(TransformTarget {
        position,
        rotation,
        anchor: position,
    })
}

/// Write a world pose back to the stored triple — the inverse of
/// [`object_pose`], through the same conversion.
///
/// For a stage piece this write is the drag's *preview* only: its pose is
/// derived from its joint, so the release inverts the mate and writes the
/// graph (`Luma::stage_commit_pose`), and the re-solve then overwrites what
/// was previewed here with the same numbers read back.
fn set_object_pose(
    scene: &mut scene_desc::Scene,
    object: &EditorObject,
    position: Vec3,
    rotation: Quat,
) {
    let (pos, rot) = coords::data_pose_of(
        to_world().inverse() * Mat4::from_rotation_translation(rotation, position),
    );
    match object {
        EditorObject::Fixture(id) => {
            if let Some(object) = scene.fixtures.iter_mut().find(|object| &object.id == id) {
                object.pos = pos;
                object.rot = rot;
            }
        }
        EditorObject::StagePiece(id) => {
            if let Some(object) = scene.pieces.iter_mut().find(|object| &object.id == id) {
                object.pos = pos;
                object.rot = rot;
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
///
/// Every pose here is read off the solved graph, through the same
/// [`stage_render::piece_of`] the offscreen path uses: the two differ in
/// render settings, never in where anything is.
pub(crate) fn scene(
    rig: &Rig,
    definitions: &BTreeMap<String, scene_desc::Definition>,
    environment: VenueEnvironment,
) -> scene_desc::Scene {
    // A fixture node's id *is* its `fixtures` row id, which is what makes the
    // patch and the placement two halves of one fixture without either half
    // storing the other's key.
    let placed: HashMap<&str, &_> = rig
        .venue
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    scene_desc::Scene {
        id: "live".into(),
        times: Vec::new(),
        camera: scene_desc::CameraPose {
            position: [0.0; 3],
            target: [0.0; 3],
        },
        editing: true,
        aim_arrows: false,
        // The one preset a venue is drawn under, with the haze resolution
        // reduced for the live path. The lab overwrites the fill dials each
        // frame; the house and the sky are re-read from this environment
        // there too, so the lamps hang from the first frame on.
        render: scene_desc::RenderSettings::room(
            environment,
            FOV_Y_DEG,
            luma_render::LIVE_HAZE_RESOLUTION,
        ),
        selected_fixture_ids: Vec::new(),
        editor: scene_desc::Editor::default(),
        // A fixture whose definition did not resolve has no mesh and no cone,
        // and one nobody has placed has nowhere to be, so both are left out
        // rather than drawn as a guess at the origin.
        fixtures: rig
            .fixtures
            .iter()
            .filter(|f| definitions.contains_key(&f.fixture_path))
            .filter_map(|f| {
                let node = placed.get(f.id.as_str())?;
                Some(scene_desc::Fixture {
                    id: f.id.clone(),
                    fixture_path: f.fixture_path.clone(),
                    mode_name: f.mode_name.clone(),
                    pos: node.position.map(|v| v as f32),
                    rot: node.rotation.map(|v| v as f32),
                })
            })
            .collect(),
        pieces: rig
            .venue
            .nodes
            .iter()
            .filter_map(stage_render::piece_of)
            .collect(),
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
    /// The hit-test geometry and its per-asset BVHs, kept across frames.
    pick_cache: PickCache,
    haze_started_at: Instant,
}

/// What the presentation seam did with one submitted frame.
#[derive(Clone, Copy, Default)]
struct Submission {
    serial: u64,
    haze_time_s: f32,
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
    serial: u64,
    timings_serial: Option<u64>,
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

/// Explicit diagnostic only: capture every prepaint, including submissions
/// that delivered nothing. Buffered writes avoid flushing the UI thread each frame.
fn trace_stage_frame(sample: &FrameSample) {
    use std::io::Write;
    thread_local! {
        static TRACE: std::cell::RefCell<Option<std::io::BufWriter<std::fs::File>>> =
            std::cell::RefCell::new(std::env::var_os("LUMA_STAGE_TRACE").map(|path| {
                std::io::BufWriter::new(std::fs::File::create(path).expect("create stage trace"))
            }));
    }
    TRACE.with_borrow_mut(|trace| {
        if let Some(trace) = trace {
            serde_json::to_writer(&mut *trace, sample).expect("write stage trace");
            writeln!(trace).expect("write stage trace newline");
        }
    });
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
    /// Identity of this row's requested camera, transport time and dimensions.
    submitted_serial: u64,
    /// Delivered image, which can belong to an earlier request.
    presented_serial: Option<u64>,
    /// Source of the cached GPU timings. Join to `submitted_serial` for its
    /// inputs and count each source once, even when several images reuse it.
    profiled_serial: Option<u64>,
    /// Air animation uses a wall clock independently of transport time.
    haze_time_s: f32,
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
    /// Transport time of `submitted_serial`, in track seconds. Delivered
    /// images and GPU timings can belong to older requests; use their serials
    /// to join to the corresponding input row before comparing content.
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
enum StageFrame {
    /// Uploaded into gpui's sprite atlas under [`STAGE_IMAGE_ID`], refreshed in
    /// place each frame.
    Image(Arc<RenderImage>),
    /// Memory the renderer and the compositor both address. Nothing is
    /// uploaded and nothing is copied; the compositor samples where the
    /// renderer wrote.
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
        // Fixture state/strobe uses transport time above; air keeps moving
        // while playback is paused. Offline captures retain their pinned time.
        frame.time = self.haze_started_at.elapsed().as_secs_f32();
        let haze_time_s = frame.time;
        self.work.build_ms = built.elapsed().as_secs_f32() * 1_000.0;
        let picked = std::time::Instant::now();
        let pick = PickSnapshot::from_frame(&frame, scene, camera, &mut self.pick_cache);
        self.work.pick_ms = picked.elapsed().as_secs_f32() * 1_000.0;
        let completed = self
            .viewport
            .take_latest()
            .transpose()
            .map_err(|error| format!("Could not render the frame: {error}"))?;
        let (serial, outcome, occupancy) = self.viewport.submit_numbered(frame, width, height);
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
fn stage_gpu_enabled() -> bool {
    luma_ui::runtime::Runtime::with(luma_ui::runtime::Runtime::stage_gpu_enabled)
}

// -- the screen --------------------------------------------------------------

/// What the stage should be showing, resolved from the shell's current subject.
///
/// A named triple rather than a bare tuple: the two `String`s are the same
/// type as each other, which is exactly when positional returns start getting
/// swapped by accident.
struct StageSubject {
    pub(crate) venue_id: String,
    venue_name: String,
    /// The score that lights the rig, when one does.
    lit: Option<Lit>,
    graph_preview: Option<crate::graph::preview::View>,
}

/// The score a stage is lit by.
///
/// The id is the answer to *which document*; the ordinal is only how a person
/// names it, carried alongside so stage automation can identify it without a
/// second read. Compared whole rather than by id, so a renumbering — a
/// sibling score deleted out from under this one — refreshes the readout
/// instead of leaving it naming a position the sidebar no longer uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lit {
    pub(crate) score: String,
    pub(crate) ordinal: i64,
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
        // The *score*, not the pair it sits on. A `(track, venue)` carries as
        // many scores as there are people who annotated it, and the editor is
        // the one thing that knows which of them is open — so the stage takes
        // that answer rather than looking one up and disagreeing.
        let lit = match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) if state.venue_id() == venue_id => state.lit(),
            Some(Body::TrackEditor(_) | Body::Graph(_) | Body::Patch(_)) | None => None,
        };
        let name = self
            .sidebar
            .as_ref()
            .filter(|browser| browser.venue_id() == venue_id)
            .map_or_else(
                || venue_id.clone(),
                |browser| browser.venue_name().to_string(),
            );
        let graph_preview = match self.workspace.active_body() {
            Some(Body::Graph(editor)) => editor.preview_view(),
            _ => None,
        };
        Some(StageSubject {
            venue_id,
            venue_name: name,
            lit,
            graph_preview,
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
            graph_preview,
        }) = self.stage_subject()
        else {
            // Dropping the state is what un-mounts the viewport, and
            // un-mounting is what stops its continuous redraw — see the
            // rendering note on [`visualizer`].
            if let Some(state) = self.visualizer.take() {
                if let Some(view) = state.graph_preview {
                    self.stop_graph_preview(&view.target, cx);
                }
                cx.notify();
            }
            return;
        };
        let previous_preview = self
            .visualizer
            .as_ref()
            .and_then(|state| state.graph_preview.as_ref())
            .map(|view| view.target.clone());
        if let Some(previous) = previous_preview
            .filter(|target| graph_preview.as_ref().map(|view| &view.target) != Some(target))
        {
            self.stop_graph_preview(&previous, cx);
        }
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
        if let Some(state) = visualizer {
            state.graph_preview = graph_preview;
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

pub(crate) enum Chrome {
    Embedded { venue_tools: Option<AnyElement> },
    Fullscreen { transport: Option<AnyElement> },
}

/// The stage pane with floating controls.
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
    chrome: Chrome,
    focus: &gpui::FocusHandle,
) -> impl IntoElement {
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
    state.presentation = matches!(&chrome, Chrome::Fullscreen { .. });
    let (venue_tools, transport) = match chrome {
        Chrome::Embedded { venue_tools } => (venue_tools, None),
        Chrome::Fullscreen { transport } => (None, transport),
    };
    let venue = venue_tools.is_some();
    let body = body(state, app, library);
    let floating = overlay_toolbar(state, app, venue_tools, transport);
    state.bottom_bar_visible = floating.is_some();
    let fps = fps_overlay(state, app);
    let pane = state.stage.borrow().pane;
    let builder = state
        .build
        .as_ref()
        .filter(|_| !state.presentation)
        .map(|build| {
            crate::stage::build_layer(
                build,
                &state.camera,
                pane.origin,
                (f32::from(pane.size.width), f32::from(pane.size.height)),
                // With no GPU there is no canvas and no window listener, so the
                // claim card is also the pointer's way in — the headless path.
                state.gpu_enabled,
                app,
            )
        });
    let measure = {
        let stage = Rc::clone(&state.stage);
        canvas(
            move |bounds, _, _| {
                stage.borrow_mut().pane = bounds;
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    };
    let selection_target = state.selection_card_position(pane.size);
    let selection = if venue {
        state
            .build
            .as_ref()
            .and_then(|build| crate::stage::selection_controls(build, app))
    } else {
        None
    };
    let selection_at = if selection.is_some() {
        let at = state.selection_motion.sample(
            Vec2::new(f32::from(selection_target.x), f32::from(selection_target.y)),
            Instant::now(),
        );
        Point::new(px(at.x), px(at.y))
    } else {
        state.selection_motion.since = None;
        selection_target
    };
    let mut keys = gpui::KeyContext::default();
    keys.add(crate::keymap::context::VISUALIZER);
    if venue {
        keys.add(crate::keymap::context::STAGE);
        keys.add(crate::keymap::context::PATCH);
    }
    div()
        .size_full()
        .key_context(keys)
        .track_focus(focus)
        .flex()
        .flex_col()
        .bg(ladder::background())
        .child(
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(body)
                .child(measure)
                .children(builder)
                .child(fps)
                .children(floating)
                .child(fullscreen_button(state.presentation, app))
                .when(matches!(state.status, Status::Live), |d| {
                    d.child(settings::trigger(state, app))
                })
                .children(selection.map(|controls| {
                    let stage = Rc::clone(&state.stage);
                    luma_ui::float::popover_card()
                        .occlude()
                        .w(px(280.))
                        .p(px(10.))
                        .child(
                            div()
                                .id("scene-object-controls")
                                .max_h(px(210.))
                                .overflow_y_scroll()
                                .child(controls),
                        )
                        .child(
                            canvas(
                                move |bounds, _, _| {
                                    stage.borrow_mut().selection_card_size = bounds.size;
                                },
                                |_, (), _, _| {},
                            )
                            .absolute()
                            .inset_0(),
                        )
                        .agent_node(Role::Card, "Selected object")
                        .map(|card| {
                            div()
                                .absolute()
                                .left(selection_at.x)
                                .top(selection_at.y)
                                .child(luma_ui::float::frosted_card(card))
                        })
                })),
        )
        .children(
            state
                .graph_preview
                .as_ref()
                .map(|view| crate::graph::preview::controls(view, app, library)),
        )
        .agent_node(
            Role::Card,
            state.lit.as_ref().map_or_else(
                || state.venue_name.clone(),
                |lit| format!("RIG SCORE #{}", lit.ordinal),
            ),
        )
}

fn view_controls(state: &Visualizer, app: &Entity<Luma>) -> impl IntoElement {
    let lab = &state.render_lab;
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(lab_toggle(
            state,
            app,
            "Haze",
            lab.haze_enabled,
            LabToggle::Haze,
        ))
        .child(lab_value(
            app,
            "Haze density",
            lab.haze_density,
            0.,
            MAX_HAZE_DENSITY,
            LabValue::HazeDensity,
        ))
        .child(lab_value(
            app,
            "Cloudiness",
            lab.haze_appearance.cloudiness,
            0.,
            1.,
            LabValue::Cloudiness,
        ))
        .child(lab_value(
            app,
            "Cloud size (m)",
            lab.haze_appearance.cloud_size,
            0.5,
            20.,
            LabValue::CloudSize,
        ))
        .child(lab_value(
            app,
            "Turbulence",
            lab.haze_appearance.turbulence,
            0.,
            1.,
            LabValue::Turbulence,
        ))
        .child(lab_value(
            app,
            "Wind speed (m/s)",
            lab.haze_appearance.wind_speed,
            0.,
            10.,
            LabValue::WindSpeed,
        ))
        .child(lab_value(
            app,
            "Wind direction (°)",
            lab.haze_appearance.wind_direction,
            0.,
            360.,
            LabValue::WindDirection,
        ))
        .child(lab_toggle(
            state,
            app,
            "Fixture shadows",
            lab.fixture_shadows,
            LabToggle::FixtureShadows,
        ))
        .child(lab_toggle(
            state,
            app,
            "Grid",
            lab.grid_enabled,
            LabToggle::Grid,
        ))
        .child(lab_toggle(
            state,
            app,
            "Gizmos",
            lab.gizmos_enabled,
            LabToggle::Gizmos,
        ))
}

fn lab_toggle(
    state: &Visualizer,
    app: &Entity<Luma>,
    label: &'static str,
    checked: bool,
    control: LabToggle,
) -> impl IntoElement {
    use luma_ui::node::AgentNode as _;
    let index = match control {
        LabToggle::Haze => 0,
        LabToggle::FixtureShadows => 1,
        LabToggle::Grid => 2,
        LabToggle::Gizmos => 3,
    };
    let t = state.settings_motion.borrow_mut().switches[index].sample(checked);
    let app = app.clone();
    div()
        .id(label)
        .flex()
        .items_center()
        .gap(px(8.))
        .justify_between()
        .h(px(28.))
        .cursor_pointer()
        .child(div().text_size(px(12.)).child(label))
        .child(
            div()
                .relative()
                .w(px(32.))
                .h(px(18.))
                .rounded_full()
                .bg(luma_ui::motion::mix(
                    luma_ui::glass::wash(0.15),
                    ladder::foreground().into(),
                    t,
                ))
                .child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .left(px(2. + 14. * t))
                        .size(px(14.))
                        .rounded_full()
                        .bg(luma_ui::motion::mix(
                            ladder::foreground_alpha(0.7),
                            ladder::background().into(),
                            t,
                        )),
                ),
        )
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.render_lab.toggle(control);
                }
                cx.notify();
            });
        })
        .agent_node(Role::Toggle, label)
        .agent_focused(checked)
}

/// A labelled value scrub on the floating settings surface.
fn lab_value(
    app: &Entity<Luma>,
    label: &'static str,
    value: f32,
    min: f32,
    max: f32,
    control: LabValue,
) -> Div {
    let app = app.clone();
    let (step, power) = if matches!(control, LabValue::HazeDensity) {
        (0.001, 2.0)
    } else {
        (0.01, 1.0)
    };
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(div().text_size(px(12.)).child(label))
        .child(
            luma_ui::float::scrub_with_power(
                label,
                value.into(),
                f64::from(min)..=f64::from(max),
                step,
                76.0,
                power,
                move |value, _, cx| {
                    app.update(cx, |this, cx| {
                        if let Some(state) = this.visualizer_mut() {
                            state.render_lab.set(control, value as f32);
                        }
                        cx.notify();
                    });
                },
            )
            .agent_node(Role::Slider, label),
        )
}

/// A persistent exit stays available even when the scene is loading or failed.
fn fullscreen_button(fullscreen: bool, app: &Entity<Luma>) -> impl IntoElement {
    let label = if fullscreen {
        "Exit fullscreen"
    } else {
        "Fullscreen visualizer"
    };
    let icon = if fullscreen {
        luma_ui::icons::IconName::Minimize
    } else {
        luma_ui::icons::IconName::Expand
    };
    let app = app.clone();
    div().absolute().top(px(16.)).right(px(16.)).child(
        luma_ui::button("", luma_ui::Enabled::Yes)
            .id("visualizer-fullscreen")
            .size(px(32.))
            .p_0()
            .occlude()
            .child(gpui_component::Icon::new(icon).size(px(18.)))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(label).build(window, cx)
            })
            .on_click(move |_, window, cx| {
                app.update(cx, |this, cx| this.toggle_visualizer_fullscreen(window, cx))
            })
            .agent_node(Role::Button, label),
    )
}

/// Only mount the floating surface when it has controls to display.
fn overlay_toolbar(
    state: &Visualizer,
    app: &Entity<Luma>,
    venue_tools: Option<AnyElement>,
    transport: Option<AnyElement>,
) -> Option<AnyElement> {
    let gizmo = !state.presentation
        && matches!(state.status, Status::Live)
        && state.selection_gizmo_space() != luma_scene::gizmo::GizmoSpace::DISABLED;
    let venue_tools = venue_tools.filter(|_| matches!(state.status, Status::Live));
    if venue_tools.is_none() && transport.is_none() && !gizmo {
        return None;
    }
    let current = state.gizmo_mode;
    let mode = |label: &'static str, mode: GizmoMode| {
        let app = app.clone();
        luma_ui::float::segment(label, current == mode, label)
            .id(label)
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(state) = this.visualizer_mut() {
                        state.gizmo_mode = mode;
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Toggle, label)
    };
    Some(
        div()
            .absolute()
            .bottom(TOOLBAR_OVERLAY_BOTTOM)
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                luma_ui::float::popover_card()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .p(px(4.))
                    // The bar, not the centring row it sits in: an opaque
                    // surface over the viewport owns the pointer it covers
                    // (see [`listen`]), and the row is air either side of it.
                    .occlude()
                    .children(venue_tools)
                    .children(transport)
                    // The two gizmo modes are one choice, so they share one
                    // track. Zoom is the wheel's (and `=`/`-`), not a button's
                    // — a camera verb with a pointer gesture needs no chrome.
                    //
                    // The mode switch uses the same joint freedoms as the
                    // handles, including mixed selections with no shared frame.
                    .when(gizmo, |bar| {
                        bar.child(
                            luma_ui::float::segmented()
                                .child(mode("Translate", GizmoMode::Translate))
                                .child(mode("Rotate", GizmoMode::Rotate)),
                        )
                    })
                    .map(luma_ui::float::frosted_card),
            )
            .agent_node(Role::Card, "Visualizer toolbar")
            .into_any_element(),
    )
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
    let live = matches!(state.status, Status::Live);
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
                .text_size(px(10.))
                .line_height(px(12.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ladder::foreground())
                .child(fps_text.clone())
                .agent_node(Role::Text, format!("FPS {fps_text}")),
        )
        .child(luma_ui::silkscreen("FPS"))
        .when(expanded, |el| {
            el.child(
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
            )
        });
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
        .right(px(10.))
        .when(live, |el| {
            el.child(
                div()
                    .id("fps-overlay")
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(4.))
                    .border_1()
                    .border_color(ladder::border())
                    .bg(ladder::apex())
                    // The card, not the absolute wrapper it hangs in: opaque
                    // over the viewport, so the pointer plane it covers is its
                    // own (see [`listen`]).
                    .occlude()
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
    // Graph evaluation and its errors remain available when GPU rendering is
    // disabled. Keep one sample for the eventual stage draw.
    let graph_sample = if let Some(view) = &state.graph_preview {
        let time = view.time(library);
        let sampled = std::time::Instant::now();
        match view.sample(time) {
            Ok(universe) => Some((
                time,
                Some(universe),
                sampled.elapsed().as_secs_f32() * 1_000.0,
            )),
            Err(error) => {
                return plate(error)
                    .agent_node(Role::Card, "Stage")
                    .into_any_element()
            }
        }
    } else {
        None
    };
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
                        "shared_surface": "true = zero-copy (an IOSurface on Metal, a texture on the compositor's own device on wgpu); false = CPU readback, whose copy and map sit inside draw_ms and are invisible to gpu_total_ms",
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
                    "reading": "All spans describe the same frame as interval_ms; they are paired by submission serial. Work the late frame in order: renders_in_gap 0 means nothing asked for a frame; ui_frame_gap_ms tracking interval_ms means the UI thread did not produce one, and request_to_prepaint_ms says how much of that belonged to another view before the walk reached the stage; if ui_frame_gap_ms stays at frame rate while interval_ms grows, the frames were made and something downstream held them. Then count the delivered=false rows between two deliveries: that is how many frames were made and thrown away, and on those rows slot_startable, the slot counts and replaced_undelivered say why.",
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
    state.status = Status::Live;

    // Only resolved values cross into the `'static` paint closure; the mutable
    // lab remains owned by the screen.
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
    let key_lab = state.render_lab.clone();
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
            // The render size, not the element's: past the pixel budget the
            // stage renders smaller and the compositor upscales it. Everything
            // downstream — the idle key, the renderer, the trace — sees this.
            let (width, height) = RenderScale::from_env().size((
                (f32::from(bounds.size.width) * scale).round().max(1.0) as u32,
                (f32::from(bounds.size.height) * scale).round().max(1.0) as u32,
            ));

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
                            selected_pieces: selected_piece_ids.clone(),
                            gizmo_mode,
                            gizmo_hover,
                            universe: universe.clone(),
                        };
                        let moving_haze = key_lab.haze_enabled
                            && key_lab.haze_density > 0.0
                            && key_lab.haze_appearance.cloudiness > 0.0
                            && (key_lab.haze_appearance.wind_speed > 0.0
                                || key_lab.haze_appearance.turbulence > 0.0);
                        let rest = if interacting || moving_haze {
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
                            scene.render = key_lab.settings(camera.fov_y_deg);
                            scene.selected_fixture_ids = selected_fixture_ids.clone();
                            scene.editor = scene_desc::Editor {
                                selected_piece_ids: selected_piece_ids.clone(),
                                gizmo_piece_ids: gizmo_piece_ids.clone(),
                                gizmo: gizmo_mode,
                                gizmo_space,
                                hover: gizmo_hover,
                                build: build_affordances.clone(),
                            };
                            match gpu.frame(LiveFrameInputs {
                                scene,
                                definitions: &stage.definitions,
                                state: universe.as_ref(),
                                time,
                                size: (width, height),
                                camera,
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
                                        submitted_serial: gpu.submission.serial,
                                        haze_time_s: gpu.submission.haze_time_s,
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
                                        // Submitted, nothing back. The row that
                                        // used to be missing entirely.
                                        _ => stage.previous.clone().map(|frame| (frame, None)),
                                    };
                                    trace_stage_frame(&sample);
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
/// lab panel, the toolbar, a seam grip. Both guards below therefore mean "no
/// surface in front of me has claimed this": `is_hovered` is false behind an
/// occluder, and `should_handle_scroll` is false behind a full one. That holds
/// only because every surface that floats over this one says so with
/// `occlude` / `block_mouse_except_scroll`. A new overlay that forgets is not
/// a bug in *this* function: pressing it would also orbit the camera, and a
/// wheel over it would also dolly.
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
                    state.drag = Some((drag, at));
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
            {
                let Some(state) = this.visualizer_mut() else {
                    return;
                };
                // The held button is the authority on whether a drag is live.
                // Press and release bookkeeping can only ever agree with it,
                // so a stale anchor cannot turn a hover into an orbit.
                match held {
                    None => {
                        state.drag = None;
                        // Nothing pressed: the move is a hover — over the
                        // gizmo, or aiming the held ghost.
                        let hover = over.then(|| state.hover_gizmo(at)).flatten();
                        if state.gizmo_hover != hover {
                            state.gizmo_hover = hover;
                            cx.notify();
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
                            state.editor_moved(at);
                            cx.notify();
                        }
                    }
                    Some(_) => {
                        if let Some((drag, was)) = state.drag {
                            state.drag = Some((drag, at));
                            state.dragged(at - was);
                            cx.notify();
                        }
                    }
                }
            }
            if aim {
                this.stage_aim_from_pointer(at, cx);
                cx.notify();
            }
        });
    });

    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
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
                    state.drag = None;
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
    fn view_controls_are_independent_and_bounded() {
        let mut lab = RenderLab::new(VenueEnvironment::default());
        let house = lab.house;
        lab.set(LabValue::HazeDensity, 9.0);
        assert_eq!(lab.haze_density, 0.5);
        lab.toggle(LabToggle::FixtureShadows);
        assert!(!lab.fixture_shadows);
        assert_eq!(lab.house, house);
    }
}

#[cfg(test)]
mod render_scale_tests {
    use super::RenderScale;

    #[test]
    fn an_ordinary_window_renders_native() {
        assert_eq!(RenderScale::Budget.size((1984, 1511)), (1984, 1511));
        assert_eq!(RenderScale::Budget.size((2227, 1391)), (2227, 1391));
        assert_eq!(RenderScale::Budget.size((1, 1)), (1, 1));
    }

    #[test]
    fn fullscreen_scales_to_the_budget_at_its_own_aspect() {
        let (width, height) = RenderScale::Budget.size((3600, 2260));
        let scale = f64::from(width) / 3600.0;
        assert!((scale - 0.617).abs() < 0.002, "scale {scale}");
        assert!(
            (f64::from(width) / f64::from(height) - 3600.0 / 2260.0).abs() < 0.002,
            "{width}x{height}"
        );
        let budget = super::RENDER_BUDGET_PIXELS;
        let pixels = f64::from(width) * f64::from(height);
        assert!(
            (pixels / budget - 1.0).abs() < 0.002,
            "{pixels} vs {budget}"
        );
    }

    #[test]
    fn overrides_parse_and_clamp() {
        assert_eq!(RenderScale::parse(None, None), RenderScale::Budget);
        assert_eq!(RenderScale::parse(None, Some("off")), RenderScale::Native);
        assert_eq!(
            RenderScale::parse(Some("0.5"), None),
            RenderScale::Fixed(0.5)
        );
        assert_eq!(
            RenderScale::parse(Some("0.1"), None),
            RenderScale::Fixed(0.25)
        );
        assert_eq!(
            RenderScale::parse(Some("3"), Some("off")),
            RenderScale::Fixed(1.0)
        );
        assert_eq!(
            RenderScale::parse(Some("soft"), Some("off")),
            RenderScale::Native
        );
        assert_eq!(RenderScale::parse(None, Some("on")), RenderScale::Budget);
        assert_eq!(RenderScale::Native.size((3600, 2260)), (3600, 2260));
        assert_eq!(RenderScale::Fixed(0.5).size((3600, 2260)), (1800, 1130));
        assert_eq!(RenderScale::Fixed(0.25).size((2, 2)), (1, 1));
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
mod selection_card_tests {
    use super::*;

    #[test]
    fn motion_uses_the_sidebar_curve_and_retargets_without_jumping() {
        let mut motion = SelectionMotion::new(false);
        let start = Instant::now();
        let duration = luma_ui::motion::span(&luma_ui::motion::SURFACE);
        let first = Vec2::new(600.0, 200.0);
        let flipped = Vec2::new(100.0, 240.0);
        assert_eq!(motion.sample(first, start), first);
        assert_eq!(motion.sample(flipped, start), first);
        let midway = start + duration / 2;
        let expected = first.lerp(flipped, luma_ui::motion::SURFACE.progress(0.5));
        let orbit_target = Vec2::new(130.0, 280.0);
        assert!(motion
            .sample(orbit_target, midway)
            .abs_diff_eq(expected, 1e-3));
        assert!(motion
            .sample(orbit_target, midway + duration * 3)
            .abs_diff_eq(orbit_target, 1e-3));
        motion.since = None;
        assert_eq!(motion.sample(first, midway + duration * 3), first);
    }

    #[test]
    fn jittering_targets_preserve_velocity_and_follow_without_stalling() {
        let mut motion = SelectionMotion::new(false);
        let start = Instant::now();
        motion.sample(Vec2::ZERO, start);
        motion.sample(Vec2::splat(500.0), start);
        for frame in 1..=30 {
            let now = start + Duration::from_millis(frame * 16);
            let target = Vec2::splat(if frame % 2 == 0 { 490.0 } else { 510.0 });
            let position = motion.sample(target, now);
            let velocity = motion.velocity;
            // An arbitrarily sharp reversal changes neither position nor
            // velocity at that instant; only subsequent acceleration changes.
            assert_eq!(motion.sample(-target, now), position);
            assert_eq!(motion.velocity, velocity);
            motion.sample(target, now);
        }
        assert!(
            motion.position.x > 450.0,
            "tracking stalled: {:?}",
            motion.position
        );
    }

    #[test]
    fn spring_tracking_is_independent_of_frame_partition() {
        let start = Instant::now();
        let mut whole = SelectionMotion::new(false);
        let mut frames = SelectionMotion::new(false);
        for motion in [&mut whole, &mut frames] {
            motion.sample(Vec2::ZERO, start);
            motion.sample(Vec2::splat(500.0), start);
        }
        let expected = whole.sample(Vec2::splat(500.0), start + Duration::from_millis(160));
        for frame in 1..=10 {
            frames.sample(
                Vec2::splat(500.0),
                start + Duration::from_millis(frame * 16),
            );
        }
        assert!(frames.position.abs_diff_eq(expected, 1e-3));
        assert!(frames.velocity.abs_diff_eq(whole.velocity, 1e-2));
    }

    #[test]
    fn reduced_motion_tracks_the_object_immediately() {
        let mut motion = SelectionMotion::new(true);
        let now = Instant::now();
        motion.sample(Vec2::ZERO, now);
        assert_eq!(
            motion.sample(Vec2::new(300.0, 200.0), now),
            Vec2::new(300.0, 200.0)
        );
    }

    #[test]
    fn card_follows_the_selection_and_flips_at_the_right_edge() {
        let viewport = Vec2::new(1200.0, 800.0);
        let right = selection_card_at(
            Vec2::new(400.0, 300.0),
            Vec2::new(500.0, 400.0),
            viewport,
            Vec2::new(280.0, 230.0),
        );
        assert_eq!(right.x, 516.0);
        let left = selection_card_at(
            Vec2::new(900.0, 300.0),
            Vec2::new(1000.0, 400.0),
            viewport,
            Vec2::new(280.0, 230.0),
        );
        assert_eq!(left.x, 604.0);
        assert_eq!(left.y, right.y);
        let edge = selection_card_at(
            Vec2::new(-100.0, 790.0),
            Vec2::new(1400.0, 1000.0),
            viewport,
            Vec2::new(280.0, 230.0),
        );
        assert!(edge.x >= 12.0 && edge.x <= 908.0);
        assert!(edge.y >= 12.0 && edge.y <= 558.0);
    }
}

#[cfg(test)]
mod orbit_selection_tests {
    use super::*;

    /// A snapshot with nothing in it: the camera is all these tests read.
    fn empty_pick(camera: Camera) -> PickSnapshot {
        PickSnapshot {
            camera,
            geometry: Arc::new(PickGeometry {
                graph: SceneGraph::new(),
                meshes: Vec::new(),
                objects: Vec::new(),
                ordered: Vec::new(),
                anchors: HashMap::new(),
                bounds: HashMap::new(),
                mesh_keys: Vec::new(),
                draws: Vec::new(),
            }),
            gizmo_pivot: None,
            gizmo_space: Default::default(),
        }
    }

    fn visualizer(build: Option<crate::stage::Build>, pick: PickSnapshot) -> Visualizer {
        let camera = pick.camera;
        Visualizer {
            venue_id: "venue".into(),
            venue_name: "Venue".into(),
            subject: None,
            lit: None,
            graph_preview: None,
            gpu_enabled: false,
            status: Status::Loading,
            camera,
            framing: Default::default(),
            owes_opening_pose: false,
            drag: None,
            editor_drag: None,
            selection: Default::default(),
            gizmo_mode: Default::default(),
            gizmo_hover: None,
            size: gpui::size(px(1200.0), px(800.0)),
            viewport_origin: Default::default(),
            venue_environment: Default::default(),
            render_lab: RenderLab::new(Default::default()),
            settings_open: false,
            presentation: false,
            bottom_bar_visible: false,
            settings_motion: RefCell::new(settings::DockMotion::new(true)),
            selection_motion: SelectionMotion::new(true),
            environment_error: None,
            environment_saving: false,
            environment_edited: false,
            environment_pending: Rc::default(),
            fps_expanded: false,
            build,
            stage: Rc::new(RefCell::new(Stage {
                displayed_pick: Some(pick),
                ..Default::default()
            })),
        }
    }

    #[test]
    fn height_preview_invalidates_a_settled_viewport_before_release() {
        let camera = Camera::default();
        let pick = empty_pick(camera);
        let mut view = visualizer(None, pick);
        let lab = view.render_lab.clone();
        let key = || IdleKey {
            time_bits: 0.0_f32.to_bits(),
            camera,
            size: (1200, 800),
            lab: lab.clone(),
            selected: Vec::new(),
            selected_pieces: vec!["deck".into()],
            gizmo_mode: Default::default(),
            gizmo_hover: None,
            universe: None,
        };
        view.stage.borrow_mut().scene = Some(scene_desc::Scene {
            id: "height-preview".into(),
            times: vec![0.0],
            editing: true,
            aim_arrows: false,
            camera: scene_desc::CameraPose {
                position: [4.0, 3.0, 5.0],
                target: [0.0; 3],
            },
            render: scene_desc::RenderSettings::dark_stage(50.0, 0.5),
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
            state: BTreeMap::new(),
            pieces: vec![scene_desc::Piece {
                id: "deck".into(),
                geometry: scene_desc::Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                kind: "floor".into(),
                pos: [0.0; 3],
                rot: [0.0; 3],
                scale: 1.0,
            }],
            fixtures: vec![scene_desc::Fixture {
                id: "child".into(),
                fixture_path: "Luma/Mover.qxf".into(),
                mode_name: "Default".into(),
                pos: [0.0, 0.0, 1.0],
                rot: [0.0; 3],
            }],
        });
        // Numeric scrubbing has neither a camera drag nor a gizmo drag to
        // hold the idle gate open. Each intermediate position must owe a frame.
        for height in [0.1, 0.2, 0.3] {
            view.stage.borrow_mut().idle = Some((key(), 0));
            assert!(view.editor_drag.is_none() && view.drag.is_none());
            view.preview_stage_positions(&[
                ("deck".into(), [0.0, 0.0, height]),
                ("child".into(), [0.0, 0.0, height + 1.0]),
            ]);
            let stage = view.stage.borrow();
            assert!(
                stage.idle.is_none(),
                "preview reused the image from before the drag"
            );
            let scene = stage.scene.as_ref().unwrap();
            assert_eq!(scene.pieces[0].pos[2], height as f32);
            assert_eq!(scene.fixtures[0].pos[2], (height + 1.0) as f32);
        }
    }

    #[test]
    fn the_pick_geometry_is_reused_until_a_draw_moves() {
        let camera = Camera::default();
        let mut scene = scene_desc::Scene {
            id: "reuse".into(),
            times: vec![0.0],
            editing: true,
            aim_arrows: false,
            camera: scene_desc::CameraPose {
                position: coords::three_from_world(camera.position()).to_array(),
                target: coords::three_from_world(camera.target).to_array(),
            },
            render: scene_desc::RenderSettings::dark_stage(50.0, 0.5),
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
            fixtures: Vec::new(),
            state: std::collections::BTreeMap::new(),
            pieces: vec![scene_desc::Piece {
                id: "deck".into(),
                geometry: scene_desc::Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                kind: "floor".into(),
                pos: [1.0, 2.0, 3.0],
                rot: [0.0, 0.0, 0.6],
                scale: 1.0,
            }],
        };
        let mut library = assets::Library::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
        );
        let mut cache = PickCache::default();
        let mut snapshot = |scene: &scene_desc::Scene, cache: &mut PickCache| {
            let frame =
                build_frame_with(scene, &Default::default(), &|_, _| None, 0.0, &mut library)
                    .unwrap();
            PickSnapshot::from_frame(&frame, scene, camera, cache)
        };
        let first = snapshot(&scene, &mut cache);
        let again = snapshot(&scene, &mut cache);
        assert!(
            Arc::ptr_eq(&first.geometry, &again.geometry),
            "an unchanged draw list must not rebuild the hit-test geometry"
        );
        scene.pieces[0].pos[0] = 4.0;
        let moved = snapshot(&scene, &mut cache);
        assert!(
            !Arc::ptr_eq(&again.geometry, &moved.geometry),
            "a moved piece must rebuild it"
        );
        let object = EditorObject::StagePiece("deck".into());
        assert_ne!(
            again.geometry.bounds[&object].min, moved.geometry.bounds[&object].min,
            "the rebuilt bounds must follow the piece"
        );
    }

    #[test]
    fn panel_and_focus_use_the_rendered_objects_world_bounds_once() {
        let camera = Camera {
            target: Vec3::new(1.0, -2.0, 3.0),
            ..Default::default()
        };
        let scene = scene_desc::Scene {
            id: "bounds".into(),
            times: vec![0.0],
            editing: true,
            aim_arrows: false,
            camera: scene_desc::CameraPose {
                position: coords::three_from_world(camera.position()).to_array(),
                target: coords::three_from_world(camera.target).to_array(),
            },
            render: scene_desc::RenderSettings::dark_stage(50.0, 0.5),
            selected_fixture_ids: Vec::new(),
            editor: scene_desc::Editor {
                selected_piece_ids: vec!["deck".into()],
                ..Default::default()
            },
            fixtures: Vec::new(),
            state: std::collections::BTreeMap::new(),
            pieces: vec![scene_desc::Piece {
                id: "deck".into(),
                geometry: scene_desc::Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                kind: "floor".into(),
                pos: [1.0, 2.0, 3.0],
                rot: [0.0, 0.0, 0.6],
                scale: 1.0,
            }],
        };
        let mut library = assets::Library::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
        );
        let frame =
            build_frame_with(&scene, &Default::default(), &|_, _| None, 0.0, &mut library).unwrap();
        let cage = frame
            .overlays
            .iter()
            .find(|overlay| frame.meshes[overlay.mesh].key.starts_with("::piece-cage:"))
            .unwrap();
        let expected = Aabb::from_points(
            frame.meshes[cage.mesh]
                .vertices
                .iter()
                .map(|vertex| cage.model.transform_point3(Vec3::from(vertex.position))),
        );
        let pick = PickSnapshot::from_frame(&frame, &scene, camera, &mut PickCache::default());
        let object = EditorObject::StagePiece("deck".into());
        let actual = pick.geometry.bounds[&object];
        assert!(
            actual.min.abs_diff_eq(expected.min, 1e-4),
            "{actual:?} != {expected:?}"
        );
        assert!(actual.max.abs_diff_eq(expected.max, 1e-4));
        let mut state = visualizer(None, pick);
        state.selection.replace([object]);
        state.stage.borrow_mut().selection_card_size = gpui::size(px(280.0), px(80.0));
        let at = state.selection_card_position(state.size);
        let points: Vec<_> = expected
            .corners()
            .map(|corner| camera.project(corner, 1.5))
            .into_iter()
            .collect();
        let top = points
            .iter()
            .map(|p| (1.0 - p.y) * 400.0)
            .fold(f32::INFINITY, f32::min);
        let bottom = points
            .iter()
            .map(|p| (1.0 - p.y) * 400.0)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!((f32::from(at.y) + 40.0 - (top + bottom) * 0.5).abs() < 1e-3);
        assert!(state.focus_selection());
        for corner in expected.corners() {
            let ndc = state.camera.project(corner, 1.5);
            assert!(
                ndc.x.abs() < 1.0 && ndc.y.abs() < 1.0,
                "F clipped the selected object: {ndc:?}"
            );
        }
        assert!(
            state.camera.target.z > 3.0,
            "F must frame the raised object, not the floor"
        );
    }

    #[test]
    fn fullscreen_clicks_and_shift_drags_preserve_selection() {
        let camera = Camera::default();
        let pick = empty_pick(camera);
        let mut state = visualizer(None, pick);
        let selected = EditorObject::Fixture("selected".into());
        state.selection.replace([selected.clone()]);
        state.prepare_presentation();
        state.presentation = true;
        assert_eq!(state.camera, camera);
        assert_eq!(
            state.selection_gizmo_space(),
            luma_scene::gizmo::GizmoSpace::DISABLED
        );
        let start = gpui::point(px(100.), px(100.));
        state.editor_press(start, false);
        assert!(state.editor_release(start).is_none());
        assert_eq!(state.selection.selected(), std::slice::from_ref(&selected));
        state.editor_press(start, true);
        let end = gpui::point(px(200.), px(150.));
        state.editor_moved(end);
        assert_ne!(state.camera, camera, "Shift-drag should still orbit");
        assert!(state.editor_release(end).is_none());
        assert_eq!(state.selection.selected(), &[selected]);
    }

    #[test]
    fn releasing_a_camera_orbit_preserves_the_selected_distribution() {
        use luma_lib::models::venue_graph::{VenueGraphRows, VenueNode};
        let rig = crate::library::Rig {
            fixtures: Vec::new(),
            venue: Default::default(),
            definitions: HashMap::new(),
            environment: Default::default(),
            rows: VenueGraphRows {
                nodes: vec![VenueNode {
                    id: "venue".into(),
                    venue_id: "venue".into(),
                    kind: "venue".into(),
                    catalog_ref: None,
                    label: None,
                }],
                ..Default::default()
            },
        };
        let sockets = luma_render::catalog::VenueSockets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
            Arc::new(luma_render::catalog::NoFixtures),
        )
        .unwrap();
        let mut build = crate::stage::Build::new("venue", &rig, sockets).unwrap();
        build.selected = Some("first".into());
        build.distribution = vec!["first".into(), "second".into()];
        let camera = Camera::default();
        let pick = empty_pick(camera);
        let mut state = visualizer(Some(build), pick);
        state.selection.replace([
            EditorObject::Fixture("second".into()),
            EditorObject::Fixture("first".into()),
        ]);
        state.editor_press(gpui::point(px(100.0), px(100.0)), false);
        state.editor_moved(gpui::point(px(200.0), px(150.0)));
        assert_ne!(state.camera, camera);
        assert!(state
            .editor_release(gpui::point(px(200.0), px(150.0)))
            .is_none());
        let build = state.build.as_ref().unwrap();
        assert_eq!(build.selected.as_deref(), Some("first"));
        assert_eq!(build.distribution, ["first", "second"]);
        assert_eq!(state.selection.selected().len(), 2);
    }
}
