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
//! gpui at the pinned rev has no zero-copy texture handoff (spec §4.1), so the
//! path is: our own wgpu device renders into a texture, the texture is read
//! back as BGRA8, the bytes become a [`RenderImage`], and `Window::paint_image`
//! draws it into the element's bounds. All of that is behind
//! [`luma_render::Viewport`]; when the zero-copy path of spec §4.2 lands it
//! replaces [`Stage::frame`] and nothing else here moves.
//!
//! The render happens in a [`canvas`] *prepaint*, because prepaint is the first
//! phase that knows the element's bounds — rendering during `render` would draw
//! this frame at last frame's size.
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
use luma_render::{assets, build_frame_with, coords, scene_desc, Viewport};
use luma_scene::Camera;
use luma_ui::node::{agent_paint_node, Instrument, Role};
use luma_ui::{ladder, Enabled};

use crate::library::Rig;
use crate::track_editor::Transition;
use crate::{Library, LibraryError, Luma, Screen};

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
    /// The button held, and where the pointer last was — `MouseMoveEvent`
    /// carries no delta.
    drag: Option<(Drag, Point<Pixels>)>,
    /// The element's size as the last frame laid it out. Three's orbit rates
    /// are all per element *height*, and a rate needs the height a frame
    /// earlier than prepaint can supply it.
    size: gpui::Size<Pixels>,
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
        let composite = subject.map(|(track, venue)| library.composite_track(&track, &venue));
        cx.spawn(async move |this, cx| {
            // The composite first: it is what makes the sample non-empty, and
            // a rig that appeared before its light would flash dark.
            if let Some(composite) = composite {
                composite.await.ok();
            }
            let loaded = rig.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
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
            camera: web_camera(),
            drag: None,
            size: gpui::Size::default(),
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
        self.camera.target = frame_target(&scene);
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

    /// Acquire a device, once, on the first frame that has something to draw.
    ///
    /// Lazy rather than at construction so that a machine with no GPU reaches a
    /// screen that says so, instead of a window that never opened. Returns
    /// whether there is one; the caller shows [`Status::Empty`] if not.
    fn gpu_ready(&mut self) -> bool {
        if self.stage.borrow().gpu.is_some() {
            return true;
        }
        match Viewport::new() {
            Ok(viewport) => {
                self.stage.borrow_mut().gpu = Some(Gpu {
                    viewport,
                    assets: assets::Library::new(meshes_root()),
                });
                true
            }
            Err(error) => {
                self.status = Status::Empty(format!("No GPU for the 3D view — {error}"));
                false
            }
        }
    }

    /// Dolly by a factor: the web toolbar's one zoom verb, and what both the
    /// wheel and the middle-button drag reduce to.
    fn dolly(&mut self, factor: f32) {
        self.camera.radius = (self.camera.radius * factor).clamp(0.05, 500.0);
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
                self.camera.polar =
                    (self.camera.polar - turn * dy).clamp(1e-3, std::f32::consts::PI - 1e-3);
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

/// The camera the web app mounts with, in render-world space.
fn web_camera() -> Camera {
    let eye = coords::world_from_three(WEB_CAMERA_POSITION);
    Camera {
        target: Vec3::ZERO,
        radius: eye.length(),
        azimuth: eye.y.atan2(eye.x),
        polar: (eye.z / eye.length()).acos(),
        fov_y_deg: FOV_Y_DEG,
        znear: 0.1,
    }
}

/// Where to point the camera at a rig: the centroid of everything in it, in
/// render-world space.
///
/// The web has no equivalent — its camera sits at the origin whatever the venue
/// contains — so a venue built away from the origin opens off screen there.
/// Framing what is actually present is the smaller surprise.
fn frame_target(scene: &scene_desc::Scene) -> Vec3 {
    let points = scene
        .fixtures
        .iter()
        .map(|f| f.pos)
        .chain(scene.pieces.iter().map(|p| p.pos));
    let (sum, n) = points.fold((Vec3::ZERO, 0.0f32), |(sum, n), p| {
        (sum + coords::world_from_data(Vec3::from(p)), n + 1.0)
    });
    if n == 0.0 {
        Vec3::ZERO
    } else {
        sum / n
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
        // `use-render-settings-store.ts`'s defaults, verbatim. `dark_stage`
        // is the one that moves per frame — see the assignment in `body`.
        render: scene_desc::RenderSettings {
            dark_stage: true,
            volumetric_haze: true,
            haze_steps: 8,
            haze_resolution: 1.0,
            haze_density: 0.8,
            fov: FOV_Y_DEG,
        },
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
    viewport: Viewport,
    assets: assets::Library,
}

impl Gpu {
    /// Draw one frame and hand back an image gpui can paint.
    ///
    /// The whole of the spec §4.2 v1 readback is here: the renderer produces
    /// BGRA8 rows and they are copied once into the `image::Frame` a
    /// [`RenderImage`] owns. A zero-copy replacement swaps this body and keeps
    /// the signature.
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
    ) -> Result<Arc<RenderImage>, String> {
        let frame = build_frame_with(
            scene,
            definitions,
            &|id, head| lookup(state, id, head),
            time,
            &mut self.assets,
        )
        .map_err(|error| format!("Could not assemble the frame: {error}"))?;
        let presented = self
            .viewport
            .draw(&frame, width, height)
            .map_err(|error| format!("Could not render the frame: {error}"))?;
        let buffer = image::RgbaImage::from_raw(
            presented.width,
            presented.height,
            presented.pixels.to_vec(),
        )
        .ok_or_else(|| "readback was not width * height * 4 bytes".to_string())?;
        Ok(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
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
    /// Open the 3D view over whatever is showing.
    ///
    /// Where the venue comes from is what decides whether the rig is lit: a
    /// track editor knows a `(track, venue)` whose score can be composited, a
    /// track browser knows only a venue. Anywhere else there is no venue in
    /// hand and the action is a no-op — the same shape every screen-scoped
    /// verb in this app has.
    pub(crate) fn open_visualizer(&mut self, cx: &mut Context<Self>) {
        let opened = match &self.screen {
            Screen::TrackEditor { state, .. } => state.subject().map(|(track, venue, _)| {
                let title = state.track_name().to_string();
                (venue.clone(), title, Some((track, venue)))
            }),
            Screen::Tracks(state) => Some((
                state.venue_id().to_string(),
                state.venue_name().to_string(),
                None,
            )),
            _ => None,
        };
        let Some((venue_id, title, subject)) = opened else {
            return;
        };
        let state = Visualizer::open(&self.library, &venue_id, title, subject, cx);
        let previous = std::mem::replace(
            &mut self.screen,
            Screen::Welcome {
                venues: Vec::new(),
                error: None,
            },
        );
        self.screen = Screen::Visualizer {
            state: Box::new(state),
            previous: Box::new(previous),
        };
        cx.notify();
    }

    /// Leave for the screen the 3D view was opened from, restored whole — the
    /// contract every stacked screen here keeps.
    pub(crate) fn close_visualizer(&mut self, cx: &mut Context<Self>) {
        if let Screen::Visualizer { previous, .. } = std::mem::replace(
            &mut self.screen,
            Screen::Welcome {
                venues: Vec::new(),
                error: None,
            },
        ) {
            self.screen = *previous;
        }
        cx.notify();
    }

    /// The 3D view's state, when it is the screen that is up. Every pointer
    /// handler and toolbar button goes through here, so none of them can act
    /// on a screen that has since been left.
    pub(crate) fn visualizer_mut(&mut self) -> Option<&mut Visualizer> {
        match &mut self.screen {
            Screen::Visualizer { state, .. } => Some(state),
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
                .child(floating),
        )
}

fn toolbar(state: &Visualizer, app: &Entity<Luma>, library: &Library) -> Div {
    let back = app.clone();
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
            luma_ui::luma_button("Back", Enabled::Yes)
                .id("back")
                .on_click(move |_, _, cx| back.update(cx, |this, cx| this.back(cx)))
                .agent_node(Role::Button, "Back"),
        )
        .child(
            div()
                .text_size(px(12.))
                .child(state.venue_name.clone())
                .agent_node(Role::Text, state.venue_name.clone()),
        )
        .child(transport(app, library))
        .child(luma_ui::silkscreen(readout))
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

    // Only these three cross into the `'static` paint closure: a handle to the
    // stage, this frame's camera pose (`Camera` is `Copy`), and its light.
    let stage = Rc::clone(&state.stage);
    let camera = state.camera;
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
                        // A dark stage shows beams and nothing else, which is
                        // the right picture only when there are beams. With no
                        // scene installed the rig itself is the subject, so the
                        // stage lights up — the same call
                        // `universe-designer.tsx` makes with `forceLightStage`.
                        scene.render.dark_stage = universe.is_some();
                        match gpu.frame(
                            scene,
                            &stage.definitions,
                            universe.as_ref(),
                            time,
                            width,
                            height,
                        ) {
                            Ok(image) => Some(image),
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
                    if let Some(old) = stage.borrow_mut().previous.replace(image) {
                        window.drop_image(old).ok();
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
