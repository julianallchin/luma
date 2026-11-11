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

mod camera_export;
mod chrome;
mod editing;
mod fps_readout;
mod gpu;
mod navigation;
mod pick;
mod render_controls;
mod render_scale;
mod rig;
mod settings;
mod subject;
mod telemetry;
#[cfg(test)]
mod test_support;
pub(crate) mod tour;
mod view_panel;
mod viewport;

pub(crate) use chrome::{visualizer, Chrome};
pub use gpu::warm_renderer;
pub(crate) use navigation::{ZOOM_IN, ZOOM_OUT};
pub(crate) use rig::scene;
pub(crate) use settings::{environment_panel, ENVIRONMENT_PANEL_WIDTH};
pub(crate) use subject::Lit;

use editing::*;
use fps_readout::*;
use gpu::*;
use navigation::*;
use pick::*;
use render_controls::*;
use render_scale::*;
use telemetry::*;
use view_panel::*;
use viewport::*;

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
    assets, build_frame_at, frame::EditorObject, house, scene_desc, AsyncViewport, FrameTimings,
    MetricSummary, Moment, SubmitOutcome,
};
use luma_scene::{
    apply_rotation, apply_translation, bvh::MeshSource, coords, gizmo_scale, navigate, Aabb,
    Camera, ClickOrbit, ClickOrbitRelease, ClickOrbitUpdate, FlyKeys, Framing, GizmoHandle,
    GizmoMode, Insets, Marquee, MaterialHandle, MeshHandle, NodeContent, NodeFlags, PivotMode,
    SceneGraph, Selection, Surface, SurfaceHit, Transform, TransformTarget, TriMesh, View,
    Viewfinder, ZoomLimits,
};
use luma_scene::{environment::VenueEnvironment, strobe::FixtureClock};
use luma_ui::ladder;
use luma_ui::node::{agent_paint_node, Instrument, Role};

use crate::library::Rig;
use crate::saved_cameras::Pose;
use crate::shell::Body;
use crate::{Library, LibraryError, Luma};

/// The stage camera's vertical field of view, in degrees.
pub(crate) const FOV_Y_DEG: f32 = 50.0;

/// Frame shape to fit against before the viewport has been laid out once.
const DEFAULT_ASPECT: f32 = 16.0 / 9.0;

/// How far the controls in the stage's top corners sit in from its edges. They
/// are boxes in the corners, so this is a layout number and nothing else — see
/// [`Visualizer::view_finder`] for why it buys no camera distance.
const CORNER_INSET: Pixels = px(16.);
/// How tall the controls in the stage's top corners are: the FPS readout on
/// the left, the fullscreen button on the right.
const CORNER_CONTROL: Pixels = px(32.);
/// How far the floating toolbar sits in from the viewport's bottom edge. Read
/// by the overlay that draws it *and* by the camera fit, so the two cannot
/// drift: a rig framed to the whole pane is framed partly under this chrome.
const TOOLBAR_OVERLAY_BOTTOM: Pixels = px(16.);

/// Height of one control slab plus the hairline trim around it — the vertical
/// span the toolbar occupies, and so the band the fit keeps clear.
const OVERLAY_BAND: Pixels = px(30.);

/// Why the viewport is not showing a lit rig.
enum Status {
    Loading,
    Live,
    /// No GPU, a venue with nothing patched, or a load that failed. Shown
    /// verbatim.
    Empty(String),
}

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
    /// A preset the track editor's browser is playing in place of the score,
    /// while the pointer is over its tile.
    audition: Option<crate::track_editor::Audition>,
    /// The playing track's bass, which shakes the footage look's camera.
    bass: Option<Bass>,
    /// Where the preview draws each head while its motors turn toward the
    /// score's pan and tilt.
    motors: luma_lib::motors::Motors,
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
    /// Where this venue's camera was last left on this device, if anywhere.
    /// The opening pose is this when there is one — see [`crate::saved_cameras`].
    saved_pose: Option<Pose>,
    /// The opening pose has been taken. Until then the camera is a stand-in
    /// for a rig that has not landed, and saving it would overwrite the pose
    /// the venue is about to open on.
    posed: bool,
    /// The button held, and where the pointer last was — `MouseMoveEvent`
    /// carries no delta.
    drag: Option<(Drag, Point<Pixels>)>,
    /// The held right button's fly. Beside [`Self::drag`] rather than in it,
    /// because keys change it and a pointer anchor does not.
    fly: Option<Fly>,
    /// The camera walkthrough, while it is up — see [`tour`].
    tour: Option<tour::Tour>,
    /// Whether this view holds the window's pointer lock — see
    /// [`Self::sync_pointer_lock`].
    pointer_locked: bool,
    /// The operator's fly speed multiple, which the wheel sets during a fly.
    /// Kept from one fly to the next.
    fly_speed: f32,
    /// Where the camera last pivoted, and when: the dot that shows it for
    /// [`PIVOT_DOT`].
    pivot_dot: Option<(Vec3, Instant)>,
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
    render_controls: RenderControls,
    /// The view-settings popover beside the dock.
    pub(crate) settings: luma_ui::float::MenuVisibility,
    /// The page of it that is open.
    settings_page: settings::Page,
    presentation: bool,
    bottom_bar_visible: bool,
    settings_motion: RefCell<settings::DockMotion>,
    environment_saving: bool,
    environment_edited: bool,
    environment_pending: Rc<RefCell<Option<VenueEnvironment>>>,
    /// The same four, for the room's haze — venue truth on the same row, so it
    /// needs the same guard. A dragged density emits a value per pointer move,
    /// and one IPC write per frame is not a thing to do to a database.
    haze_saving: bool,
    haze_edited: bool,
    haze_pending: Rc<RefCell<Option<luma_scene::environment::VenueHaze>>>,
    /// And a coalescing pair for the scrubbed device settings — the render
    /// percent and the camera look. They are device settings rather than venue
    /// data but are scrubbed by the same kind of control, and
    /// `Library::set_setting` keeps ordering, not a bound on how many writes
    /// are queued behind it. The newest value per key wins.
    view_setting_saving: bool,
    view_setting_pending: Rc<RefCell<std::collections::BTreeMap<&'static str, String>>>,
    /// Whether the FPS readout's frame-stats panel is open, or playing its exit.
    fps_panel: luma_ui::float::MenuVisibility,
    /// The motion policy at construction, for the popovers' exits: the stage
    /// render that reaps them has no `App` to read it from.
    reduced_motion: bool,
    /// A show export is rendering. The stage draws nothing meanwhile, so the
    /// export has the GPU to itself.
    pub(crate) exporting: bool,
    /// The builder. `None` until the rig lands, and the one place a position
    /// is editable in this app — see [`crate::stage`].
    pub(crate) build: Option<crate::stage::Build>,
    /// The Rig page is under the stage: the room is shown under work light.
    pub(crate) rigging: bool,
    /// Where the work light's dial is, as a fraction of its travel.
    pub(crate) work_light: f32,
    stage: Rc<RefCell<Stage>>,
}

/// The renderer's first asset library, loading off the UI thread.
enum Warming {
    Loading,
    Ready(assets::Library),
}

/// Everything that outlives a frame and must be reachable from a `'static`
/// paint closure.
#[derive(Default)]
struct Stage {
    /// The rig. `None` until the load lands.
    scene: Option<scene_desc::Scene>,
    definitions: BTreeMap<String, luma_scene::definition::Definition>,
    /// The light the rig's fixtures make of a universe.
    lights: luma_lib::stage_render::StageLights,
    /// The rig's fixtures that strobe on their own clock, and that clock.
    strobes: FixtureStrobes,
    /// The rig's laser projectors and zones, solved each frame.
    lasers: luma_lib::laser::LaserRig,
    /// Acquired lazily on the first frame that has something to draw — see
    /// [`Visualizer::gpu_ready`].
    gpu: Option<Gpu>,
    /// The rig's meshes, floor and sky, read and decoded off the UI thread
    /// for the renderer's first frame, which waits for them. `None` once the
    /// renderer has them, or when it already had a library of its own.
    assets: Option<Warming>,
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
    /// The device pixels the last prepaint asked for, after both the pixel
    /// budget and the operator's percent. Written where that size is first
    /// known and read by the View panel's caption — a percent on its own cannot
    /// say what it actually bought.
    render_size: Option<(u32, u32)>,
    /// End-to-end renderer wall time from the previous completed frame.
    last_draw_ms: Option<f32>,
    /// CPU scene encoding and queue submission time from the previous frame.
    last_cpu_ms: Option<f32>,
    /// GPU scene-through-composite time from the previous frame.
    last_gpu_ms: Option<f32>,
    /// The exposure the stage's camera had reached at the last frame that
    /// said, in stops: where it stays when its meter is switched off.
    exposure_ev: Option<f32>,
    /// Cold cluster rebuild time; zero on topology cache hits.
    last_cluster_ms: Option<f32>,
    /// Fixture shadow maps redrawn by the previous frame. Zero is the healthy
    /// steady state; a sustained non-zero run is tenancy churning.
    last_shadow_maps: Option<u32>,
    /// The share of its pixels the quality level's frame rate leaves it.
    dynamic: DynamicBudget,
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
    /// The worker's retired-frame count over the readout's window.
    rendered: RenderedRate,
    /// A hitch report waiting for a caller that can reach the library. The
    /// paint closure notices the hitch but has no `Library`; `body` has one and
    /// runs every frame, so it drains this.
    pending_hitch: Option<Vec<FrameSample>>,
    /// The inputs of the last submitted frame, with the settle countdown the
    /// renderer still needs on them; `None` while inputs keep changing.
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
    /// The camera export in flight, and its result.
    exports: camera_export::Exports,
    /// The free-running clock the air and the strobes run on.
    clock: StageClock,
}

/// A track's audio, which the footage look's bass shake listens to.
pub(crate) type Bass = luma_lib::host_audio::TrackAudio;
