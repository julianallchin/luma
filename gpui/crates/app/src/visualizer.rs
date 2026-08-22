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
//! gpui at the pinned rev has no zero-copy texture handoff (spec §4.1), so a
//! renderer worker reads BGRA8 back asynchronously, the completed bytes become
//! a [`RenderImage`], and `Window::paint_image` draws it. The worker's bounded
//! three-slot seam drops obsolete work rather than blocking prepaint. A future
//! IOSurface path replaces that seam without changing the screen.
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
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use glam::{Mat3, Mat4, Vec3};
use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Context, Corners, DispatchPhase, Div, Entity,
    Hitbox, HitboxBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, RenderImage, ScrollWheelEvent, Window,
};
use luma_lib::models::fixtures::FixtureDefinition;
use luma_lib::models::stage::StagePiece;
use luma_lib::models::universe::UniverseState;
use luma_render::{assets, build_frame_with, coords, scene_desc, AsyncViewport, FrameTimings};
use luma_scene::Camera;
use luma_ui::node::{agent_paint_node, Instrument, Role};
use luma_ui::{ladder, Enabled};

use crate::library::Rig;
use crate::shell::Body;
use crate::tabs::Target;
use crate::track_editor::Transition;
use crate::{Library, LibraryError, Luma};

/// The three.js `<Canvas camera>` the web visualizer mounts with, in three
/// space: `position [0, 1, 3]`, target at the origin, 50° vertical field.
const WEB_CAMERA_POSITION: Vec3 = Vec3::new(0.0, 1.0, 3.0);
const FOV_Y_DEG: f32 = 50.0;

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

/// The 3D view's screen state.
pub(crate) struct Visualizer {
    venue_name: String,
    status: Status,
    camera: Camera,
    /// The rig's extent, which is what the camera is framed and clamped
    /// against. Set when the rig lands; [`Framing::default`] until then.
    framing: Framing,
    /// The button held, and where the pointer last was — `MouseMoveEvent`
    /// carries no delta.
    drag: Option<(Drag, Point<Pixels>)>,
    /// The element's size as the last frame laid it out. Three's orbit rates
    /// are all per element *height*, and a rate needs the height a frame
    /// earlier than prepaint can supply it.
    size: gpui::Size<Pixels>,
    render_lab: RenderLab,
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
    /// The image the previous frame painted, released only once the next one
    /// is on screen: gpui caches sprite-atlas entries by image id, and
    /// dropping the current one would leave the view blank for a frame.
    previous: Option<Arc<RenderImage>>,
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
}

/// Runtime renderer controls. They belong to the viewport, not persisted venue
/// data, so experimentation cannot silently rewrite a show.
struct RenderLab {
    open: bool,
    sun_enabled: bool,
    sun_azimuth_deg: f32,
    sun_elevation_deg: f32,
    sun_intensity: f32,
    sun_color: [f32; 3],
    sun_shadows: bool,
    environment_enabled: bool,
    background_color: [f32; 3],
    ambient_color: [f32; 3],
    ambient_intensity: f32,
    probe_enabled: bool,
    probe_intensity: f32,
    probe_rotation_deg: f32,
    probe_visible: bool,
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
            environment_enabled: editor_lit,
            background_color: scene_desc::Environment::EDITOR.background,
            ambient_color: scene_desc::Environment::EDITOR.ambient_color,
            ambient_intensity: if editor_lit { 0.2 } else { 0.0 },
            probe_enabled: false,
            probe_intensity: 0.8,
            probe_rotation_deg: 0.0,
            probe_visible: false,
            haze_enabled: true,
            haze_density: 0.8,
            haze_steps: 8,
            haze_resolution: luma_render::LIVE_HAZE_RESOLUTION,
            grid_enabled: editor_lit,
            debug_view: scene_desc::DebugView::Pbr,
        }
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
    Haze,
    Grid,
}

#[derive(Clone, Copy)]
enum LabValue {
    Azimuth,
    Elevation,
    SunIntensity,
    SunColor(usize),
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
            LabToggle::Haze => &mut self.haze_enabled,
            LabToggle::Grid => &mut self.grid_enabled,
        };
        *value = !*value;
    }

    fn adjust(&mut self, control: LabValue, delta: f32) {
        match control {
            LabValue::Azimuth => {
                self.sun_azimuth_deg =
                    (self.sun_azimuth_deg + delta + 180.0).rem_euclid(360.0) - 180.0;
            }
            LabValue::Elevation => {
                self.sun_elevation_deg = (self.sun_elevation_deg + delta).clamp(-85.0, 85.0);
            }
            LabValue::SunIntensity => {
                self.sun_intensity = (self.sun_intensity + delta).clamp(0.0, 10.0);
            }
            LabValue::SunColor(channel) => {
                self.sun_color[channel] = (self.sun_color[channel] + delta).clamp(0.0, 1.0);
            }
            LabValue::BackgroundColor(channel) => {
                self.background_color[channel] =
                    (self.background_color[channel] + delta).clamp(0.0, 1.0);
            }
            LabValue::AmbientColor(channel) => {
                self.ambient_color[channel] = (self.ambient_color[channel] + delta).clamp(0.0, 1.0);
            }
            LabValue::Ambient => {
                self.ambient_intensity = (self.ambient_intensity + delta).clamp(0.0, 2.0);
            }
            LabValue::ProbeIntensity => {
                self.probe_intensity = (self.probe_intensity + delta).clamp(0.0, 4.0);
            }
            LabValue::ProbeRotation => {
                self.probe_rotation_deg =
                    (self.probe_rotation_deg + delta + 180.0).rem_euclid(360.0) - 180.0;
            }
            LabValue::HazeDensity => {
                self.haze_density = (self.haze_density + delta).clamp(0.0, 2.0);
            }
            LabValue::HazeSteps => {
                self.haze_steps = ((self.haze_steps as f32 + delta).round() as u32).clamp(1, 64);
            }
            LabValue::HazeResolution => {
                self.haze_resolution = (self.haze_resolution + delta).clamp(0.25, 1.0);
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
        let composite = subject.map(|(track, venue)| library.composite_track(&track, &venue));
        let target = Target::Visualizer {
            venue: venue_id.to_string(),
        };
        cx.spawn(async move |this, cx| {
            // The composite first: it is what makes the sample non-empty, and
            // a rig that appeared before its light would flash dark.
            if let Some(composite) = composite {
                composite.await.ok();
            }
            let loaded = rig.await;
            this.update(cx, |this, cx| {
                // Addressed to the venue's tab, wherever it sits in the strip:
                // a rig landing late must not paint into another tab.
                if let Some(Body::Visualizer(state)) = this.workspace.body_mut(&target) {
                    state.rig_loaded(loaded);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();

        Self {
            venue_name,
            status: Status::Loading,
            camera: Framing::default().opening_camera(),
            framing: Framing::default(),
            drag: None,
            size: gpui::Size::default(),
            render_lab: RenderLab::new(editor_lit),
            stage: Rc::default(),
        }
    }

    /// The venue on screen, for the window title.
    pub(crate) fn venue_name(&self) -> &str {
        &self.venue_name
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
            .map(|(path, def)| (path.clone(), definition(def)))
            .collect();
        let scene = scene(&rig, &definitions);
        self.framing = Framing::of(&scene);
        self.camera = self.framing.opening_camera();
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
        if self.stage.borrow().gpu.is_some() {
            return true;
        }
        self.stage.borrow_mut().gpu = Some(Gpu {
            viewport: AsyncViewport::new(),
            assets: assets::Library::new(meshes_root()),
        });
        true
    }

    /// Dolly by a factor: the web toolbar's one zoom verb, and what both the
    /// wheel and the middle-button drag reduce to.
    ///
    /// The near bound is the rig's own extent: a camera closer than that is
    /// inside the beams, where every pixel is one saturated colour.
    fn dolly(&mut self, factor: f32) {
        let (near, far) = self.framing.radius_bounds();
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
                self.camera.polar =
                    (self.camera.polar - turn * dy).clamp(Framing::MIN_POLAR, Framing::MAX_POLAR);
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
}

/// three's `getZoomScale`: exponential in the scroll distance, so ten small
/// notches and one big flick land in the same place.
fn zoom_scale(distance: f32) -> f32 {
    ZOOM_BASE.powf(ZOOM_SPEED * distance * 0.05)
}

/// What the camera has to fit, and what its orbit limits are measured against:
/// a bounding sphere over the rig **and the floor under it**, in render-world
/// space.
///
/// The floor projection of every fixture is part of the extent because a beam's
/// pool is as much of the picture as the fixture that casts it — fitting the
/// hardware alone frames a bank of movers and cuts off everything they light.
///
/// The web has no equivalent: its camera sits three metres from the origin
/// whatever the venue contains, which is inside the beams of any real rig. That
/// is what [`Visualizer::open`] used to inherit, and it is the whole of the
/// "orbit walks into a flat red field" failure — a camera immersed in a cone
/// sees one blown-out slab of colour, with the falloff and the pool off screen.
#[derive(Clone, Copy)]
struct Framing {
    /// Centre of the bounding sphere; the orbit target.
    target: Vec3,
    /// Its radius, never zero — a one-fixture rig still needs a scale.
    radius: f32,
}

impl Framing {
    /// Keeps the eye off the +Z pole, where `look_at`'s Z up vector degenerates.
    const MIN_POLAR: f32 = 0.12;
    /// Keeps the eye above the target's horizon, and so out of the floor.
    const MAX_POLAR: f32 = std::f32::consts::FRAC_PI_2 - 0.03;
    /// Margin between the fitted sphere and the edges of the frame.
    const FIT_MARGIN: f32 = 1.15;
    /// How far outside the sphere a dolly may come. Inside it is inside the
    /// beams.
    const NEAR_MARGIN: f32 = 1.25;
    /// Furthest out a dolly may go, as a multiple of the fit distance.
    const FAR_MULTIPLE: f32 = 6.0;

    fn of(scene: &scene_desc::Scene) -> Self {
        let fixtures = scene
            .fixtures
            .iter()
            .map(|f| coords::world_from_data(Vec3::from(f.pos)));
        let points: Vec<Vec3> = fixtures
            .clone()
            // Where each fixture's light lands, so the pools are in frame.
            .map(|p| p.with_z(0.0))
            .chain(fixtures)
            .chain(
                scene
                    .pieces
                    .iter()
                    .map(|p| coords::world_from_data(Vec3::from(p.pos))),
            )
            .collect();
        let Some(&first) = points.first() else {
            return Self {
                target: Vec3::ZERO,
                radius: 1.0,
            };
        };
        // Centre of the AABB rather than the centroid: a rig with twenty
        // fixtures on one truss and one on the far wall is still one rig, and
        // the centroid would frame the truss and lose the wall.
        let (min, max) = points
            .iter()
            .fold((first, first), |(lo, hi), &p| (lo.min(p), hi.max(p)));
        let target = (min + max) * 0.5;
        let radius = points
            .iter()
            .fold(0.0f32, |r, &p| r.max((p - target).length()))
            .max(1.0);
        Self { target, radius }
    }

    /// Orbit distance that puts the whole sphere inside the vertical field.
    ///
    /// Vertical because the viewport is a wide centre pane; a pane narrower
    /// than it is tall would clip the rig horizontally, and zooming out is the
    /// answer to that rather than a fit that reflows on every resize.
    fn fit_distance(&self) -> f32 {
        self.radius * Self::FIT_MARGIN / (FOV_Y_DEG.to_radians() / 2.0).sin()
    }

    /// The camera pose this rig opens at: the web's viewing direction, at a
    /// distance that fits.
    fn opening_camera(&self) -> Camera {
        let eye = coords::world_from_three(WEB_CAMERA_POSITION);
        Camera {
            target: self.target,
            radius: self.fit_distance(),
            azimuth: eye.y.atan2(eye.x),
            polar: (eye.z / eye.length())
                .acos()
                .clamp(Self::MIN_POLAR, Self::MAX_POLAR),
            fov_y_deg: FOV_Y_DEG,
            znear: 0.1,
        }
    }

    /// Radii a dolly may reach: never inside the rig, never so far out that the
    /// scene is a speck with no way back.
    fn radius_bounds(&self) -> (f32, f32) {
        (
            (self.radius * Self::NEAR_MARGIN).max(0.5),
            self.fit_distance() * Self::FAR_MULTIPLE,
        )
    }
}

impl Default for Framing {
    /// The scale to use before a rig has loaded — nothing is drawn at that
    /// point, so only the units matter.
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            radius: 4.0,
        }
    }
}

// -- the venue, in the renderer's vocabulary ---------------------------------

/// One venue as a scene description: geometry in data space, the render dials
/// the web's dark-stage view pins, and an **empty** state map — head state
/// arrives per frame through [`luma_render::StateSource`] instead.
fn scene(rig: &Rig, definitions: &BTreeMap<String, scene_desc::Definition>) -> scene_desc::Scene {
    scene_desc::Scene {
        id: "live".into(),
        times: Vec::new(),
        camera: scene_desc::CameraPose {
            position: [0.0; 3],
            target: [0.0; 3],
        },
        editing: false,
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
        pieces: flatten_pieces(&rig.pieces),
        state: BTreeMap::new(),
    }
}

/// Resolve every piece's parent chain into a world pose.
///
/// A `StagePiece` with a `parent_piece_id` holds its pose in *parent-local*
/// space, but [`scene_desc::Piece`] has no parent link — the golden dump
/// flattens the chain on the TypeScript side (`stage/lib/tree.ts`). Flattening
/// here rather than teaching the renderer about parents is what keeps its scene
/// flat, and keeps transform composition in one place instead of two.
///
/// A dangling or cyclic parent leaves the piece at its local pose rather than
/// dropping it: a deck in the wrong place is debuggable, a deck that vanished
/// is not.
fn flatten_pieces(pieces: &[StagePiece]) -> Vec<scene_desc::Piece> {
    let by_id: std::collections::HashMap<&str, &StagePiece> =
        pieces.iter().map(|p| (p.id.as_str(), p)).collect();
    pieces
        .iter()
        .map(|piece| {
            let mut model = local_matrix(piece);
            let mut scale = piece.scale as f32;
            let mut parent = piece.parent_piece_id.as_deref();
            // A chain can be no longer than the list; anything more is a cycle.
            let mut budget = pieces.len();
            while let Some(id) = parent {
                let (Some(up), 1..) = (by_id.get(id), budget) else {
                    break;
                };
                budget -= 1;
                model = local_matrix(up) * model;
                scale *= up.scale as f32;
                parent = up.parent_piece_id.as_deref();
            }
            let (_, rotation, translation) = model.to_scale_rotation_translation();
            let euler = coords::euler_xyz_of(Mat3::from_quat(rotation));
            scene_desc::Piece {
                id: piece.id.clone(),
                mesh_path: piece.mesh_path.clone(),
                pos: translation.to_array(),
                rot: euler.to_array(),
                scale,
            }
        })
        .collect()
}

/// One piece's own transform, in its parent's space — or the world's, with no
/// parent. Data space throughout; `frame::build` applies the swap to render
/// space.
fn local_matrix(piece: &StagePiece) -> Mat4 {
    Mat4::from_translation(Vec3::new(
        piece.pos_x as f32,
        piece.pos_y as f32,
        piece.pos_z as f32,
    )) * Mat4::from_mat3(coords::euler_xyz(
        piece.rot_x as f32,
        piece.rot_y as f32,
        piece.rot_z as f32,
    )) * Mat4::from_scale(Vec3::splat(piece.scale as f32))
}

/// The QLC+ subset the renderer reads, out of the QLC+ model Luma parsed.
///
/// `luma_lib::models::fixtures` and [`scene_desc`] are the same concept — a
/// `.qxf` file — declared twice, and this function is the standing cost of
/// that. It should collapse to one set of types that the golden JSON also
/// deserialises into.
fn definition(def: &FixtureDefinition) -> scene_desc::Definition {
    scene_desc::Definition {
        kind: def.type_.clone(),
        modes: def
            .modes
            .iter()
            .map(|mode| scene_desc::Mode {
                name: mode.name.clone(),
                // Only the length is read (`Definition::head_count`); the
                // renderer's head is deliberately opaque.
                heads: mode.heads.iter().map(|_| serde_json::Value::Null).collect(),
            })
            .collect(),
        physical: def.physical.as_ref().map(|p| scene_desc::Physical {
            dimensions: p.dimensions.as_ref().map(|d| scene_desc::Dimensions {
                width: d.width,
                height: d.height,
                depth: d.depth,
            }),
            layout: p.layout.as_ref().map(|l| scene_desc::Layout {
                width: l.width,
                height: l.height,
            }),
            // Zero is the renderer's "unknown", which is what an absent QLC+
            // attribute means.
            lens: p.lens.as_ref().map(|l| scene_desc::Lens {
                degrees_min: l.degrees_min.unwrap_or(0.0),
                degrees_max: l.degrees_max.unwrap_or(0.0),
            }),
        }),
    }
}

// -- the GPU, and the presentation seam --------------------------------------

/// The renderer and the meshes it has loaded.
struct Gpu {
    viewport: AsyncViewport,
    assets: assets::Library,
}

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
    fn frame(
        &mut self,
        scene: &scene_desc::Scene,
        definitions: &BTreeMap<String, scene_desc::Definition>,
        state: Option<&UniverseState>,
        time: f32,
        width: u32,
        height: u32,
    ) -> Result<Option<(Arc<RenderImage>, f32, Option<FrameTimings>)>, String> {
        let frame = build_frame_with(
            scene,
            definitions,
            &|id, head| lookup(state, id, head),
            time,
            &mut self.assets,
        )
        .map_err(|error| format!("Could not assemble the frame: {error}"))?;
        let completed = self
            .viewport
            .take_latest()
            .transpose()
            .map_err(|error| format!("Could not render the frame: {error}"))?;
        self.viewport.submit(frame, width, height);
        let Some(presented) = completed else {
            return Ok(None);
        };
        let draw_ms = presented.draw_time.as_secs_f32() * 1_000.0;
        let buffer = image::RgbaImage::from_raw(
            presented.width,
            presented.height,
            presented.pixels.as_ref().to_vec(),
        )
        .ok_or_else(|| "readback was not width * height * 4 bytes".to_string())?;
        Ok(Some((
            Arc::new(RenderImage::new([image::Frame::new(buffer)])),
            draw_ms,
            presented.timings,
        )))
    }
}

/// One head's state out of an evaluated universe.
///
/// The eval engine keys a single-head fixture by its bare id and a multi-head
/// one by `"<id>:<head>"`; the renderer asks for `(id, head)` and does not know
/// which it has. Answering both here is what lets it stay ignorant.
fn lookup(
    state: Option<&UniverseState>,
    id: &str,
    head: usize,
) -> Option<scene_desc::PrimitiveState> {
    let state = state?;
    let p = state
        .primitives
        .get(&format!("{id}:{head}"))
        .or_else(|| state.primitives.get(id))?;
    Some(scene_desc::PrimitiveState {
        dimmer: p.dimmer,
        color: p.color,
        strobe: p.strobe,
        position: p.position,
        // Fixture wheel slots are not in `UniverseState` yet. Open is the
        // explicit identity until that evaluated-state contract grows them.
        gobo: 0,
        gobo_rotation: 0.0,
    })
}

/// Where the stage GLBs live. Mirrors `library`'s fixtures-root resolution: the
/// repo's own copy in a dev build, overridable for a fixture with its own.
fn meshes_root() -> PathBuf {
    if let Some(path) = std::env::var_os("LUMA_MESHES_ROOT") {
        return PathBuf::from(path);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map_or_else(
            || PathBuf::from("resources/meshes"),
            |repo| repo.join("resources/meshes"),
        )
}

// -- the screen --------------------------------------------------------------

/// The 3D view's transitions, kept with the screen the way every other one is
/// (`settings::open_settings`, `track_editor::open_track`), so `lib.rs` stays
/// the list of what exists rather than the list of how each screen is reached.
impl Luma {
    /// Open the venue's 3D view as a workspace tab — a singleton per venue,
    /// which target-keyed identity gives for free.
    ///
    /// Where the venue comes from is what decides whether the rig is lit: a
    /// visible track editor knows a `(track, venue)` whose score can be
    /// composited, the sidebar knows only its venue. With neither there is no
    /// venue in hand and the action is a no-op.
    pub(crate) fn open_visualizer(&mut self, cx: &mut Context<Self>) {
        let opened = match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) => state.subject().map(|(track, venue, _)| {
                let title = state.track_name().to_string();
                (venue.clone(), title, Some((track, venue)))
            }),
            _ => self.sidebar.as_ref().map(|browser| {
                (
                    browser.venue_id().to_string(),
                    browser.venue_name().to_string(),
                    None,
                )
            }),
        };
        let Some((venue_id, title, subject)) = opened else {
            return;
        };
        let target = Target::Visualizer {
            venue: venue_id.clone(),
        };
        if self.workspace.body_mut(&target).is_some() {
            self.workspace.select(&target);
            cx.notify();
            return;
        }
        let state = Visualizer::open(&self.library, &venue_id, title, subject, cx);
        self.open_tab(target, move || Body::Visualizer(Box::new(state)), cx);
    }

    /// The 3D view's state, when it is the visible tab. Every pointer handler
    /// and toolbar button goes through here, so none of them can act on a tab
    /// that is not on screen.
    pub(crate) fn visualizer_mut(&mut self) -> Option<&mut Visualizer> {
        match self.workspace.active_body_mut() {
            Some(Body::Visualizer(state)) => Some(state),
            _ => None,
        }
    }
}

/// The whole screen: chrome above, viewport below.
pub(crate) fn visualizer(
    state: &mut Visualizer,
    app: &Entity<Luma>,
    library: &Library,
    window: &mut Window,
) -> Div {
    // Continuous redraw: asking at the top of a render is what makes the next
    // one happen, and CVDisplayLink paces it (spec §4.3).
    window.request_animation_frame();
    let chrome = toolbar(state, app, library);
    let floating = overlay_toolbar(state, app);
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
        .child(transport(app, library))
        .child(renderer_lab_trigger(state, app))
        .child(luma_ui::silkscreen(readout))
        .child(luma_ui::silkscreen(
            state
                .stage
                .borrow()
                .last_draw_ms
                .map_or_else(|| "DRAW —".into(), |ms| format!("DRAW {ms:.1} MS")),
        ))
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

fn renderer_lab(state: &Visualizer, app: &Entity<Luma>) -> Div {
    let lab = &state.render_lab;
    div()
        .absolute()
        .top(px(12.))
        .right(px(12.))
        .w(px(360.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .border_1()
        .border_color(ladder::border())
        .bg(ladder::apex())
        .child(luma_ui::silkscreen("RENDERER LAB"))
        .child(lab_toggle(app, "Sun", lab.sun_enabled, LabToggle::Sun))
        .child(lab_value(
            app,
            "Sun azimuth",
            lab.sun_azimuth_deg,
            -180.0,
            180.0,
            15.0,
            LabValue::Azimuth,
        ))
        .child(lab_value(
            app,
            "Sun elevation",
            lab.sun_elevation_deg,
            -85.0,
            85.0,
            5.0,
            LabValue::Elevation,
        ))
        .child(lab_value(
            app,
            "Sun intensity",
            lab.sun_intensity,
            0.0,
            10.0,
            0.2,
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
            0.05,
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
            0.1,
            LabValue::ProbeIntensity,
        ))
        .child(lab_value(
            app,
            "Probe rotation",
            lab.probe_rotation_deg,
            -180.0,
            180.0,
            15.0,
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
        .child(lab_value(
            app,
            "Haze density",
            lab.haze_density,
            0.0,
            2.0,
            0.1,
            LabValue::HazeDensity,
        ))
        .child(lab_value(
            app,
            "Haze steps",
            lab.haze_steps as f32,
            1.0,
            64.0,
            1.0,
            LabValue::HazeSteps,
        ))
        .child(lab_value(
            app,
            "Haze resolution",
            lab.haze_resolution,
            0.25,
            1.0,
            0.05,
            LabValue::HazeResolution,
        ))
        .child(lab_toggle(
            app,
            "Editor grid",
            lab.grid_enabled,
            LabToggle::Grid,
        ))
        .child(debug_view_button(app, lab.debug_label()))
        .child(
            div()
                .text_size(px(10.))
                .text_color(ladder::muted_foreground())
                .child({
                    let stage = state.stage.borrow();
                    match (stage.last_cpu_ms, stage.last_gpu_ms) {
                        (Some(cpu), Some(gpu)) => format!("CPU {cpu:.2} ms · GPU {gpu:.2} ms"),
                        _ => "CPU/GPU timing unavailable".into(),
                    }
                })
                .agent_node(Role::Text, "Renderer CPU and GPU timing"),
        )
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
        lab_value(
            app,
            channel_label,
            color[index],
            0.0,
            1.0,
            0.05,
            control(index),
        )
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

#[allow(clippy::too_many_arguments)]
fn lab_value(
    app: &Entity<Luma>,
    label: &'static str,
    value: f32,
    min: f32,
    max: f32,
    step: f32,
    control: LabValue,
) -> Div {
    let button = |button_label: String, delta: f32| {
        let app = app.clone();
        luma_ui::luma_button(&button_label, Enabled::Yes)
            .id(button_label.clone())
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(state) = this.visualizer_mut() {
                        state.render_lab.adjust(control, delta);
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Button, button_label)
    };
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().w(px(104.)).text_size(px(10.)).child(label))
        .child(luma_ui::luma_slider(value, min, max, 140.0).agent_node(Role::Slider, label))
        .child(button(format!("Decrease {label}"), -step))
        .child(button(format!("Increase {label}"), step))
}

/// Play/pause and the clock, over the *host's* transport rather than the track
/// editor's.
///
/// The editor keeps a playhead of its own because it draws one; this view draws
/// none — every frame reads [`Library::render_time`] afresh — so what it needs
/// is the host running, and nothing in between. That is why this is two calls
/// and not a share of `track_editor`'s transport, which is mostly the machinery
/// for keeping a local playhead in step with a remote one.
fn transport(app: &Entity<Luma>, library: &Library) -> Div {
    let host = library.transport();
    let (label, playing) = if host.is_playing {
        ("Pause", true)
    } else {
        ("Play", false)
    };
    let toggled = app.clone();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            luma_ui::luma_button(
                label,
                if host.is_loaded {
                    Enabled::Yes
                } else {
                    Enabled::No
                },
            )
            .id("transport")
            .on_click(move |_, _, cx| {
                toggled.update(cx, |this, cx| {
                    let step: Transition = if playing {
                        Box::pin(this.library.pause())
                    } else {
                        Box::pin(this.library.play())
                    };
                    cx.spawn(async move |_, _| {
                        step.await.ok();
                    })
                    .detach();
                    cx.notify();
                });
            })
            .agent_node(Role::Button, label),
        )
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
    div()
        .absolute()
        .bottom(px(16.))
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
                    .child(zoom("Zoom In", DOLLY_IN))
                    .child(zoom("Zoom Out", DOLLY_OUT)),
            )
        })
}

/// The viewport itself, or the reason there isn't one.
fn body(state: &mut Visualizer, app: &Entity<Luma>, library: &Library) -> AnyElement {
    // A frame that failed drew nothing and left its reason behind; adopt it
    // before deciding what this frame shows.
    if let Some(error) = state.stage.borrow_mut().error.take() {
        state.status = Status::Empty(error);
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

    // The one live read. `render_time` and `sample_universe` are synchronous
    // because a frame's inputs must be this frame's — see `Library`.
    let time = library.render_time();
    let universe = library.sample_universe(time);
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
    let environment_enabled = state.render_lab.environment_enabled;
    let ambient_intensity = state.render_lab.ambient_intensity;
    let ambient_color = state.render_lab.ambient_color;
    let background_color = state.render_lab.background_color;
    let probe_enabled = state.render_lab.probe_enabled;
    let probe_intensity = state.render_lab.probe_intensity;
    let probe_rotation_deg = state.render_lab.probe_rotation_deg;
    let probe_visible = state.render_lab.probe_visible;
    let haze_enabled = state.render_lab.haze_enabled;
    let haze_density = state.render_lab.haze_density;
    let haze_steps = state.render_lab.haze_steps;
    let haze_resolution = state.render_lab.haze_resolution;
    let grid_enabled = state.render_lab.grid_enabled;
    let debug_view = state.render_lab.debug_view;
    let sized = app.clone();

    canvas(
        move |bounds: Bounds<Pixels>, window, cx| {
            // The size the next drag will be scaled by. Written here because
            // prepaint is where a laid-out size first exists.
            sized.update(cx, |this, _| {
                if let Some(state) = this.visualizer_mut() {
                    state.size = bounds.size;
                }
            });
            let scale = window.scale_factor();
            let width = (f32::from(bounds.size.width) * scale).round().max(1.0) as u32;
            let height = (f32::from(bounds.size.height) * scale).round().max(1.0) as u32;

            let image = {
                let mut stage = stage.borrow_mut();
                let stage = &mut *stage;
                match (stage.scene.as_mut(), stage.gpu.as_mut()) {
                    (Some(scene), Some(gpu)) => {
                        scene.camera.position =
                            coords::three_from_world(camera.position()).to_array();
                        scene.camera.target = coords::three_from_world(camera.target).to_array();
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
                        scene.render.sun = sun_enabled.then_some(scene_desc::DirectionalLight {
                            direction: sun_direction,
                            color: sun_color,
                            intensity: sun_intensity,
                            shadows: sun_shadows,
                        });
                        scene.render.haze.enabled = haze_enabled;
                        scene.render.haze.density = haze_density;
                        scene.render.haze.steps = haze_steps;
                        scene.render.haze.resolution = haze_resolution;
                        scene.render.show_grid = grid_enabled;
                        scene.render.debug_view = debug_view;
                        match gpu.frame(
                            scene,
                            &stage.definitions,
                            universe.as_ref(),
                            time,
                            width,
                            height,
                        ) {
                            Ok(Some((image, draw_ms, timings))) => {
                                stage.last_draw_ms = Some(draw_ms);
                                if let Some(timings) = timings {
                                    stage.last_cpu_ms = Some(timings.cpu_encode_submit_ms as f32);
                                    stage.last_gpu_ms = Some(timings.gpu_total_ms as f32);
                                }
                                Some(image)
                            }
                            Ok(None) => stage.previous.clone(),
                            Err(error) => {
                                stage.error = Some(error);
                                None
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
            move |bounds, (image, hitbox): (Option<Arc<RenderImage>>, Hitbox), window, cx| {
                if let Some(image) = image {
                    window
                        .paint_image(
                            bounds,
                            bounds,
                            Corners::default(),
                            Arc::clone(&image),
                            0,
                            false,
                        )
                        .ok();
                    let mut stage = stage.borrow_mut();
                    let already_presented = stage
                        .previous
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &image));
                    if !already_presented {
                        if let Some(old) = stage.previous.replace(image) {
                            window.drop_image(old).ok();
                        }
                    }
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
        let Some(drag) = Drag::of(event.button) else {
            return;
        };
        let at = event.position;
        pressed.update(cx, |this, cx| {
            if let Some(state) = this.visualizer_mut() {
                state.drag = Some((drag, at));
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
        dragged.update(cx, |this, cx| {
            let Some(state) = this.visualizer_mut() else {
                return;
            };
            let Some((drag, was)) = state.drag else {
                return;
            };
            state.drag = Some((drag, at));
            state.dragged(at - was);
            cx.notify();
        });
    });

    let released = app.clone();
    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        released.update(cx, |this, cx| {
            if let Some(state) = this.visualizer_mut() {
                state.drag = None;
                cx.notify();
            }
        });
    });

    let zoomed = app.clone();
    let over = hitbox.clone();
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !over.is_hovered(window) {
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
    fn render_lab_controls_are_independent_and_bounded() {
        let mut lab = RenderLab::new(true);
        let original_background = lab.background_color;

        lab.adjust(LabValue::SunColor(0), -0.35);
        lab.adjust(LabValue::BackgroundColor(2), 0.2);
        lab.adjust(LabValue::AmbientColor(1), -0.4);
        lab.adjust(LabValue::ProbeIntensity, 10.0);
        lab.adjust(LabValue::ProbeRotation, 225.0);
        lab.toggle(LabToggle::Probe);
        lab.toggle(LabToggle::ProbeVisible);
        lab.adjust(LabValue::HazeSteps, 100.0);
        lab.adjust(LabValue::HazeResolution, -10.0);

        assert!((lab.sun_color[0] - 0.65).abs() < f32::EPSILON);
        assert!((lab.background_color[2] - (original_background[2] + 0.2)).abs() < f32::EPSILON);
        assert!((lab.ambient_color[1] - 0.6).abs() < f32::EPSILON);
        assert_eq!(lab.background_color[..2], original_background[..2]);
        assert_eq!(lab.probe_intensity, 4.0);
        assert_eq!(lab.probe_rotation_deg, -135.0);
        assert!(lab.probe_enabled);
        assert!(lab.probe_visible);
        assert_eq!(lab.haze_steps, 64);
        assert_eq!(lab.haze_resolution, 0.25);

        lab.adjust(LabValue::HazeSteps, -100.0);
        lab.adjust(LabValue::HazeResolution, 10.0);
        assert_eq!(lab.haze_steps, 1);
        assert_eq!(lab.haze_resolution, 1.0);
    }
}
