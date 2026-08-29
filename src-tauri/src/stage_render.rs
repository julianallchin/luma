//! One venue, one moment, one PNG — the offscreen path behind
//! `luma.venue.render(...)`.
//!
//! # Why this lives here and not in the visualizer
//!
//! A venue's 3D description is *database shape brought into renderer shape*:
//! [`PatchedFixture`] and [`StagePiece`] rows become [`scene_desc::Fixture`] and
//! [`scene_desc::Piece`], a parent chain gets flattened, and a
//! [`UniverseState`] key becomes a head's [`scene_desc::PrimitiveState`]. That
//! translation belongs next to the models it reads, so the desktop viewport and
//! an agent's offscreen frame describe the same room the same way rather than
//! twice.
//!
//! **Known duplication.** `gpui/crates/app/src/visualizer.rs` still carries its
//! own `scene`, `flatten_pieces`, `local_matrix`, `definition`, `lookup` and
//! `meshes_root` — this module is a copy of those, made because the visualizer
//! is being reworked concurrently and could not be edited. The visualizer
//! should adopt these and delete its private copies; until it does, a fix to
//! either one has to be made twice, which is exactly the change amplification
//! this module exists to remove.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};

use glam::{Mat4, Vec3};
use luma_render::scene_desc::{self, RenderSettings};
use luma_render::{assets, build_frame_with, coords, Renderer, DEFAULT_SUBFRAMES};
use luma_scene::{Camera, View, Viewfinder};

use crate::database::local;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::models::stage::StagePiece;
use crate::models::universe::{PrimitiveState, UniverseState};
use crate::services::groups::ResolvedFixture;

/// Vertical field of view every derived camera uses. Matches the desktop
/// viewport, so `view="front"` offscreen is the pose the operator would see.
pub const FOV_Y_DEG: f32 = 50.0;

/// Haze march resolution for offscreen frames. Full rate: an offscreen render
/// is not on a frame budget, and the haze is most of what a beam *is*.
const HAZE_RESOLUTION: f32 = 1.0;

/// Render settings every offscreen frame uses.
///
/// Always the editor's lit environment: an offscreen frame is read by someone
/// who was not in the room, and a dark stage hides everything the score does
/// not happen to light.
fn offscreen_render() -> RenderSettings {
    RenderSettings::editor_lit(FOV_Y_DEG, HAZE_RESOLUTION)
}

/// Largest offscreen frame an agent may ask for, per side. Mirrors the figure
/// cap in `luma_exec.figures`, so a rendered stage and a matplotlib figure are
/// bounded by the same number.
pub const MAX_DIMENSION: u32 = 2000;

/// The stage-piece kinds that stand in for "where the operator is", best first.
/// Only [`View::Dj`] reads it.
const BOOTH_KINDS: [&str; 2] = ["cdj", "mixer"];

/// One venue's 3D contents, as the database holds them.
///
/// The three halves are only meaningful together — fixtures without their
/// definitions are not a partial rig, they are positions with no geometry — so
/// they are loaded and carried as one value.
pub struct VenueGeometry {
    /// Every patched fixture, in patch order.
    pub fixtures: Vec<PatchedFixture>,
    /// Every stage piece, parent links not yet resolved.
    pub pieces: Vec<StagePiece>,
    /// Keyed by `fixture_path`. A path whose bundle no longer resolves is
    /// absent rather than an error: a venue can outlive a fixture bundle.
    pub definitions: HashMap<String, FixtureDefinition>,
}

impl VenueGeometry {
    /// Read one venue's fixtures, pieces and fixture definitions.
    ///
    /// # Errors
    /// Fails if the venue is not readable.
    pub async fn load(
        pool: &sqlx::SqlitePool,
        fixtures_root: &Path,
        venue_id: &str,
    ) -> Result<Self, String> {
        let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id))
            .await
            .map_err(|error| format!("the venue is not available: {error}"))?;
        Self::read(&mut access, fixtures_root).await
    }

    /// [`Self::load`] against an access the caller already holds, so a reader
    /// that also resolves a selection sees one venue snapshot rather than two.
    ///
    /// # Errors
    /// Fails if the venue's fixtures or pieces cannot be read.
    pub async fn read(
        access: &mut VenueAccess<'_, Read>,
        fixtures_root: &Path,
    ) -> Result<Self, String> {
        let fixtures = local::fixtures::get_patched_fixtures(access).await?;
        let pieces = local::stage::get_stage_pieces(access).await?;
        let mut definitions = HashMap::new();
        for path in fixtures
            .iter()
            .map(|fixture| fixture.fixture_path.clone())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let Some(relative) = confined(&path) else {
                continue;
            };
            if let Ok(definition) =
                crate::services::fixtures::get_fixture_definition(fixtures_root, relative)
            {
                definitions.insert(path, definition);
            }
        }
        Ok(Self {
            fixtures,
            pieces,
            definitions,
        })
    }

    /// Whether there is anything at all to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fixtures.is_empty() && self.pieces.is_empty()
    }

    /// The renderer's description of this room, plus the definition table
    /// [`build_frame_with`] resolves fixture geometry through.
    ///
    /// The camera pose is left at the origin and unused: a derived view is a
    /// function of the framing and of the frame's aspect ratio, so it is
    /// resolved by [`render_rgba`] and installed on the built frame.
    ///
    /// The environment is [`offscreen_render`]'s, not the editor viewport's.
    #[must_use]
    pub fn scene(&self) -> (scene_desc::Scene, BTreeMap<String, scene_desc::Definition>) {
        let render = offscreen_render();
        let definitions: BTreeMap<String, scene_desc::Definition> = self
            .definitions
            .iter()
            .map(|(path, def)| (path.clone(), definition(def)))
            .collect();
        let scene = scene_desc::Scene {
            id: "venue".into(),
            times: Vec::new(),
            camera: scene_desc::CameraPose {
                position: [0.0; 3],
                target: [0.0; 3],
            },
            editing: false,
            render,
            selected_fixture_ids: Vec::new(),
            // A fixture whose definition did not resolve has no mesh and no
            // cone, so it is left out rather than drawn as a guess.
            fixtures: self
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
            pieces: flatten_pieces(&self.pieces),
            state: BTreeMap::new(),
        };
        (scene, definitions)
    }

    /// World position of the DJ booth, if this room has one.
    ///
    /// A `cdj` beats a `mixer` because a booth with decks in it is where the
    /// operator stands; a mixer alone is the fallback. Returned in render-world
    /// space (Z-up), which is the space [`Camera::for_view`] works in.
    #[must_use]
    pub fn booth(&self) -> Option<Vec3> {
        let flat = flatten_pieces(&self.pieces);
        BOOTH_KINDS.iter().find_map(|kind| {
            let index = self.pieces.iter().position(|piece| piece.kind == *kind)?;
            Some(coords::world_from_data(Vec3::from(flat[index].pos)))
        })
    }
}

/// One head's state out of an evaluated universe.
///
/// The eval engine keys a single-head fixture by its bare id and a multi-head
/// one by `"<id>:<head>"`; the renderer asks for `(id, head)` and does not know
/// which it has. Answering both here is what lets it stay ignorant.
#[must_use]
pub fn primitive_state(
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

/// Resolve every piece's parent chain into a world pose.
///
/// A [`StagePiece`] with a `parent_piece_id` holds its pose in *parent-local*
/// space, but [`scene_desc::Piece`] has no parent link. Flattening here rather
/// than teaching the renderer about parents is what keeps its scene flat.
///
/// A dangling or cyclic parent leaves the piece at its local pose rather than
/// dropping it: a deck in the wrong place is debuggable, a deck that vanished
/// is not. The result is index-aligned with `pieces`.
#[must_use]
pub fn flatten_pieces(pieces: &[StagePiece]) -> Vec<scene_desc::Piece> {
    let by_id: HashMap<&str, &StagePiece> = pieces.iter().map(|p| (p.id.as_str(), p)).collect();
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
            let (pos, rot) = coords::data_pose_of(model);
            scene_desc::Piece {
                id: piece.id.clone(),
                geometry: scene_desc::Geometry::mesh(piece.mesh_path.clone()),
                kind: piece.kind.clone(),
                pos,
                rot,
                scale,
            }
        })
        .collect()
}

/// One piece's own transform, in its parent's space — or the world's, with no
/// parent.
///
/// Built in **three space**, because that is the space these poses compose in:
/// three.js nests each piece's group inside its parent's, and the `(x, z, y)`
/// swap between the two spaces is a mirror. Composing a chain in data space
/// with the stored Euler triple applied as-is lands each attached piece
/// reflected across its parent's Y axis.
fn local_matrix(piece: &StagePiece) -> Mat4 {
    coords::three_pose_from_data(
        [piece.pos_x as f32, piece.pos_y as f32, piece.pos_z as f32],
        [piece.rot_x as f32, piece.rot_y as f32, piece.rot_z as f32],
    ) * Mat4::from_scale(Vec3::splat(piece.scale as f32))
}

/// The QLC+ subset the renderer reads, out of the QLC+ model Luma parsed.
///
/// [`crate::models::fixtures`] and [`scene_desc`] are the same concept — a
/// `.qxf` file — declared twice, and this function is the standing cost of
/// that. It should collapse to one set of types that the golden JSON also
/// deserialises into.
#[must_use]
pub fn definition(def: &FixtureDefinition) -> scene_desc::Definition {
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

/// A head lit for a highlight: open white, pointed home, not strobing.
const MARKED: PrimitiveState = PrimitiveState {
    dimmer: 1.0,
    color: [1.0, 1.0, 1.0],
    strobe: 0.0,
    position: [0.0, 0.0],
    speed: 0.0,
};

/// The universe that *is* the answer to "what does this selection resolve to":
/// every matched head open and white, every other head dark.
///
/// A highlight needs nothing from the renderer because it is not an annotation
/// drawn over a frame — it is a frame. The same editor-lit path that shows what
/// a score does at `t` shows what a selection covers, so there is one way a
/// venue is pictured, not two.
///
/// A whole-fixture match is stored under the bare fixture id, which is
/// [`primitive_state`]'s "every head of this fixture" key — so no head count is
/// needed to expand one. An unmatched head has no entry at all, and a head with
/// no state is drawn dark.
#[must_use]
pub fn highlight_state(resolved: &[ResolvedFixture]) -> UniverseState {
    let mut primitives = HashMap::new();
    for entry in resolved {
        match &entry.heads {
            None => {
                primitives.insert(entry.fixture.id.clone(), MARKED);
            }
            Some(heads) => {
                for head in heads {
                    primitives.insert(format!("{}:{head}", entry.fixture.id), MARKED);
                }
            }
        }
    }
    UniverseState { primitives }
}

/// Everything one offscreen frame needs that the scene itself does not carry.
///
/// Owned rather than borrowed because the frame is built and rendered on the
/// renderer's own thread — see [`render_png`].
pub struct Shot {
    /// Named camera position.
    pub view: View,
    /// World position of the DJ booth; only [`View::Dj`] reads it.
    pub booth: Option<Vec3>,
    /// Evaluated light state. `None` draws the rig unlit.
    pub state: Option<UniverseState>,
    /// Clock value handed to the renderer, in seconds.
    pub time: f32,
    /// Frame size in pixels. Both sides are clamped to [`MAX_DIMENSION`].
    pub size: (u32, u32),
}

/// Render one frame of `scene` as tightly packed RGBA8, plus its true size.
///
/// Installs the camera: the view is fitted against the scene's own
/// [`scene_desc::Scene::framing`] at this frame's aspect ratio, so the same
/// `view` frames a small rig and a large one the same way.
///
/// Blocks until the frame is back. Call it from a blocking context.
///
/// # Errors
/// Fails if the GPU device cannot be created, if a referenced mesh is missing
/// from `meshes_root`, or if the frame cannot be read back.
pub fn render_rgba(
    scene: scene_desc::Scene,
    definitions: BTreeMap<String, scene_desc::Definition>,
    shot: Shot,
    meshes_root: PathBuf,
) -> Result<(Vec<u8>, u32, u32), String> {
    let sequence = Sequence::install(
        scene,
        definitions,
        meshes_root,
        shot.view,
        shot.booth,
        shot.size,
    )?;
    let (width, height) = sequence.size();
    // A still is one moment; nothing before it is its predecessor.
    let rgba = sequence.frame(
        shot.state.as_ref(),
        shot.time,
        DEFAULT_SUBFRAMES,
        Continuity::Cut,
    )?;
    Ok((rgba, width, height))
}

/// Whether a frame carries on from the one before it.
///
/// The renderer's haze march is progressive — consecutive frames blend into a
/// shared history, which is how the live viewport gets a clean image out of two
/// samples per frame. A still, and any frame that must be a function of its own
/// `t` alone, is a [`Continuity::Cut`]. This picks between the renderer's two
/// entries; it is not an ordering rule the caller has to keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Continuity {
    /// This frame follows its predecessor in time; accumulate.
    Next,
    /// This frame stands alone; discard whatever came before.
    Cut,
}

/// A venue installed on the render thread, ready to be lit repeatedly.
///
/// One still is one install and one [`frame`](Self::frame); a recording is one
/// install and nine thousand. The distinction matters because the scene
/// description and the definition table are the large half of a frame request
/// and neither depends on `t` — sending them per frame would clone the whole
/// room nine thousand times.
///
/// The camera is fitted at install: framing is a function of the geometry and
/// the aspect ratio, not of the clock.
///
/// Dropping uninstalls. Several sequences may be installed at once, so an
/// agent's still does not evict a recording in progress — though it does wait
/// its turn, one frame deep.
pub struct Sequence {
    id: u64,
    size: (u32, u32),
}

impl Sequence {
    /// Install `scene` at a fixed size and view.
    ///
    /// Both sides of `size` are clamped to [`MAX_DIMENSION`]; the size actually
    /// installed is [`Sequence::size`].
    ///
    /// # Errors
    /// Fails if the render thread has stopped.
    pub fn install(
        scene: scene_desc::Scene,
        definitions: BTreeMap<String, scene_desc::Definition>,
        meshes_root: PathBuf,
        view: View,
        booth: Option<Vec3>,
        size: (u32, u32),
    ) -> Result<Self, String> {
        let size = (
            size.0.clamp(1, MAX_DIMENSION),
            size.1.clamp(1, MAX_DIMENSION),
        );
        let framing = scene.framing(&definitions);
        // No chrome over a headless frame, so the fit gets the whole of it.
        let viewfinder = Viewfinder::new(FOV_Y_DEG, size.0 as f32 / size.1 as f32);
        let camera = Camera::for_view(view, &framing, booth, &viewfinder);

        let id = NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        jobs()
            .send(Job::Install(Box::new(Installed {
                id,
                scene,
                definitions,
                camera,
                size,
                meshes_root,
            })))
            .map_err(|_| "the offscreen renderer stopped".to_string())?;
        Ok(Self { id, size })
    }

    /// The size frames come back at, after clamping.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// One frame, tightly packed sRGB RGBA8. Blocks until it is back.
    ///
    /// `continuity` says whether this frame carries on from the last one drawn
    /// through *any* sequence on the render thread — the haze history is the
    /// renderer's, not the sequence's. Getting it wrong costs image quality,
    /// never correctness: the renderer re-checks size, camera, cone geometry
    /// and clock, and drops a history that could not be this frame's past.
    ///
    /// # Errors
    /// Fails if the GPU device cannot be created, if a referenced mesh is
    /// missing, or if the frame cannot be read back.
    pub fn frame(
        &self,
        state: Option<&UniverseState>,
        time: f32,
        subframes: u32,
        continuity: Continuity,
    ) -> Result<Vec<u8>, String> {
        let (reply, answer) = mpsc::sync_channel(1);
        jobs()
            .send(Job::Frame {
                id: self.id,
                // Cloned rather than borrowed: the frame is built on the
                // renderer's own thread, which outlives this call's stack.
                state: state.cloned(),
                time,
                subframes,
                continuity,
                reply,
            })
            .map_err(|_| "the offscreen renderer stopped".to_string())?;
        answer
            .recv()
            .map_err(|_| "the offscreen renderer stopped mid-frame".to_string())?
    }
}

impl Drop for Sequence {
    fn drop(&mut self) {
        let _ = jobs().send(Job::Uninstall(self.id));
    }
}

/// [`render_rgba`], encoded as a PNG.
///
/// Encoding happens here rather than on the render thread: deflate is CPU work,
/// and the device should be free for the next frame while it runs.
///
/// # Errors
/// As [`render_rgba`], plus a frame the encoder rejects.
pub fn render_png(
    scene: scene_desc::Scene,
    definitions: BTreeMap<String, scene_desc::Definition>,
    shot: Shot,
    meshes_root: PathBuf,
) -> Result<Vec<u8>, String> {
    let (rgba, width, height) = render_rgba(scene, definitions, shot, meshes_root)?;
    luma_render::image_out::encode(&rgba, width, height)
        .map_err(|error| format!("could not encode the frame: {error}"))
}

// ---------------------------------------------------------------------------
// the render thread
// ---------------------------------------------------------------------------

/// A venue parked on the render thread, and everything about a frame of it
/// that does not depend on the clock.
struct Installed {
    id: u64,
    scene: scene_desc::Scene,
    /// Where the frame is seen from, in render-world space.
    ///
    /// Installed on the built [`luma_render::frame::Frame`] rather than on
    /// `scene.camera`: [`scene_desc::CameraPose`] is the Y-up three.js pose the
    /// golden catalogue is written in, and a derived camera has no business
    /// making a round trip through a mirror space to say where it is.
    camera: Camera,
    definitions: BTreeMap<String, scene_desc::Definition>,
    size: (u32, u32),
    meshes_root: PathBuf,
}

/// What the render thread takes.
///
/// wgpu's Metal surface types are neither `Send` nor `Sync`, so a [`Renderer`]
/// cannot be shared, moved between threads, or parked in a `static`. It is
/// therefore pinned to one thread that owns it for the life of the process, and
/// this is the message that thread takes. Creating a device and compiling the
/// pipelines is the entire cold cost of a render, so an agent asking for six
/// views in a row pays it once.
enum Job {
    /// Boxed because a `Frame` is a handful of words next to a whole venue, and
    /// the channel's element size is the larger of the two.
    Install(Box<Installed>),
    Frame {
        id: u64,
        state: Option<UniverseState>,
        time: f32,
        subframes: u32,
        continuity: Continuity,
        reply: mpsc::SyncSender<Result<Vec<u8>, String>>,
    },
    Uninstall(u64),
}

static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

static JOBS: OnceLock<mpsc::SyncSender<Job>> = OnceLock::new();

/// The render thread's inbox, starting the thread on first use.
///
/// Bounded at one: a second concurrent render waits rather than queueing
/// unboundedly behind a stuck GPU.
fn jobs() -> &'static mpsc::SyncSender<Job> {
    JOBS.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new()
            .name("luma-stage-render".into())
            .spawn(move || render_loop(&rx))
            .expect("the offscreen render thread could not be started");
        tx
    })
}

/// Own the device and the loaded meshes; answer jobs until the sender is gone.
///
/// The device is created on the first job rather than at spawn so that a
/// machine with no usable adapter reports it to the agent that asked for a
/// picture, and can be retried.
fn render_loop(rx: &mpsc::Receiver<Job>) {
    let mut stage: Option<(Renderer, assets::Library, PathBuf)> = None;
    let mut installed: HashMap<u64, Installed> = HashMap::new();
    while let Ok(job) = rx.recv() {
        let (id, state, time, subframes, continuity, reply) = match job {
            Job::Install(scene) => {
                installed.insert(scene.id, *scene);
                continue;
            }
            Job::Uninstall(id) => {
                installed.remove(&id);
                continue;
            }
            Job::Frame {
                id,
                state,
                time,
                subframes,
                continuity,
                reply,
            } => (id, state, time, subframes, continuity, reply),
        };
        let Some(scene) = installed.get(&id) else {
            // Only reachable if the handle outlived its own `Drop`, which it
            // cannot; answering rather than hanging the caller regardless.
            let _ = reply.send(Err("that sequence is no longer installed".into()));
            continue;
        };
        if stage
            .as_ref()
            .is_none_or(|(_, _, root)| *root != scene.meshes_root)
        {
            match Renderer::new() {
                Ok(renderer) => {
                    stage = Some((
                        renderer,
                        assets::Library::new(&scene.meshes_root),
                        scene.meshes_root.clone(),
                    ));
                }
                Err(error) => {
                    let _ = reply.send(Err(format!(
                        "no GPU device for offscreen rendering: {error}"
                    )));
                    continue;
                }
            }
        }
        let (renderer, library, _) = stage.as_mut().expect("the stage was just installed");
        let _ = reply.send(one_frame(
            renderer,
            library,
            scene,
            state.as_ref(),
            time,
            subframes,
            continuity,
        ));
    }
}

fn one_frame(
    renderer: &mut Renderer,
    library: &mut assets::Library,
    scene: &Installed,
    state: Option<&UniverseState>,
    time: f32,
    subframes: u32,
    continuity: Continuity,
) -> Result<Vec<u8>, String> {
    let (width, height) = scene.size;
    let mut frame = build_frame_with(
        &scene.scene,
        &scene.definitions,
        &|id, head| primitive_state(state, id, head),
        time,
        library,
    )
    .map_err(|error| format!("could not assemble the frame: {error}"))?;
    frame.camera = luma_render::frame::Camera {
        eye: scene.camera.position(),
        target: scene.camera.target,
        fov_y_deg: scene.camera.fov_y_deg,
    };
    match continuity {
        Continuity::Next => renderer.render_next(&frame, width, height, subframes),
        Continuity::Cut => renderer.render(&frame, width, height, subframes),
    }
    .map_err(|error| format!("could not render the frame: {error}"))
}

/// Where the stage GLBs live.
///
/// `LUMA_MESHES_ROOT` wins, so a test or a fixture can supply its own tree.
/// Otherwise the meshes are found *through* the fixtures root rather than by a
/// second search: a fixtures root is always `<resources>/fixtures/<version>`,
/// so its grandparent's `meshes/` is the sibling tree, in the repo and in a
/// bundle alike (`tauri.conf.json` ships both `../resources/fixtures/**/*` and
/// `../resources/meshes/**/*`, which land side by side under `_up_/resources`).
/// The repo's own copy is the last resort, and the only answer a caller with no
/// fixtures root (the desktop viewport) gets.
#[must_use]
pub fn meshes_root(fixtures_root: Option<&Path>) -> PathBuf {
    if let Some(path) = std::env::var_os("LUMA_MESHES_ROOT") {
        return PathBuf::from(path);
    }
    let sibling = fixtures_root
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|resources| resources.join("meshes"));
    match sibling {
        Some(path) if path.is_dir() => path,
        _ => Path::new(env!("CARGO_MANIFEST_DIR")).parent().map_or_else(
            || PathBuf::from("resources/meshes"),
            |repo| repo.join("resources/meshes"),
        ),
    }
}

/// A fixture path that stays inside the fixtures root, or nothing.
fn confined(path: &str) -> Option<&Path> {
    let relative = Path::new(path);
    let escapes = relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    (!escapes).then_some(relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::universe::PrimitiveState;

    fn patched(id: &str) -> PatchedFixture {
        PatchedFixture {
            id: id.into(),
            uid: None,
            venue_id: "venue".into(),
            universe: 1,
            address: 1,
            num_channels: 8,
            manufacturer: "test".into(),
            model: "test".into(),
            mode_name: "Standard".into(),
            fixture_path: "test.qxf".into(),
            label: None,
            pos_x: 0.0,
            pos_y: 0.0,
            pos_z: 0.0,
            rot_x: 0.0,
            rot_y: 0.0,
            rot_z: 0.0,
        }
    }

    /// A whole-fixture match keys the bare id, which is what makes every head
    /// of it read as lit without the resolver ever naming a head count.
    #[test]
    fn a_whole_fixture_match_lights_every_head_it_has() {
        let state = highlight_state(&[ResolvedFixture {
            fixture: patched("mover"),
            heads: None,
        }]);
        for head in [0, 1, 7] {
            let lit = primitive_state(Some(&state), "mover", head)
                .expect("a whole-fixture match answers for every head");
            assert!((lit.dimmer - 1.0).abs() < f32::EPSILON);
            assert_eq!(lit.color, [1.0, 1.0, 1.0]);
            assert!(lit.strobe.abs() < f32::EPSILON);
        }
        assert!(primitive_state(Some(&state), "other", 0).is_none());
    }

    /// A partial match lights exactly the heads it names; the rest of the
    /// fixture stays dark, which is the whole point of drawing it.
    #[test]
    fn a_partial_match_leaves_the_unnamed_heads_dark() {
        let state = highlight_state(&[ResolvedFixture {
            fixture: patched("bar"),
            heads: Some(vec![1, 3]),
        }]);
        for head in [1, 3] {
            assert!(primitive_state(Some(&state), "bar", head).is_some());
        }
        for head in [0, 2, 4] {
            assert!(primitive_state(Some(&state), "bar", head).is_none());
        }
    }

    /// An expression that matched nothing draws an unlit room rather than
    /// falling back to the score — the answer "no heads" is a picture too.
    #[test]
    fn an_empty_match_lights_nothing() {
        let state = highlight_state(&[]);
        assert!(state.primitives.is_empty());
        assert!(primitive_state(Some(&state), "anything", 0).is_none());
    }

    fn piece(id: &str, kind: &str, pos: [f64; 3]) -> StagePiece {
        StagePiece {
            id: id.into(),
            uid: None,
            venue_id: "venue".into(),
            mesh_path: "stage_lab/stage_praticavel_2x1x1.glb".into(),
            kind: kind.into(),
            label: None,
            pos_x: pos[0],
            pos_y: pos[1],
            pos_z: pos[2],
            rot_x: 0.0,
            rot_y: 0.0,
            rot_z: 0.0,
            scale: 1.0,
            parent_piece_id: None,
        }
    }

    #[test]
    fn booth_prefers_decks_over_the_mixer() {
        let geometry = VenueGeometry {
            fixtures: Vec::new(),
            pieces: vec![
                piece("m", "mixer", [1.0, 0.0, 0.0]),
                piece("d", "cdj", [2.0, 3.0, 1.0]),
            ],
            definitions: HashMap::new(),
        };
        // Data space (x, y, z) into world space is (x, -y, z).
        assert_eq!(geometry.booth(), Some(Vec3::new(2.0, -3.0, 1.0)));
    }

    #[test]
    fn booth_is_none_without_decks_or_a_mixer() {
        let geometry = VenueGeometry {
            fixtures: Vec::new(),
            pieces: vec![piece("t", "truss", [0.0, 0.0, 4.0])],
            definitions: HashMap::new(),
        };
        assert_eq!(geometry.booth(), None);
    }

    #[test]
    fn head_state_falls_back_to_the_bare_fixture_id() {
        let mut state = UniverseState::default();
        state.primitives.insert(
            "par".into(),
            PrimitiveState {
                dimmer: 0.5,
                color: [1.0, 0.0, 0.0],
                strobe: 0.0,
                position: [0.0, 0.0],
                speed: 1.0,
            },
        );
        assert_eq!(
            primitive_state(Some(&state), "par", 0).map(|p| p.dimmer),
            Some(0.5)
        );
        assert!(primitive_state(Some(&state), "other", 0).is_none());
        assert!(primitive_state(None, "par", 0).is_none());
    }
}

#[cfg(test)]
mod stage_piece_tests {
    use super::*;
    use luma_render::scene_desc;
    use std::f32::consts::FRAC_PI_2;

    fn piece(id: &str, pos: [f64; 3], rot: [f64; 3], parent: Option<&str>) -> StagePiece {
        StagePiece {
            id: id.into(),
            uid: None,
            venue_id: "v".into(),
            mesh_path: "stage_lab/truss_q30_box.glb".into(),
            kind: "truss".into(),
            label: None,
            pos_x: pos[0],
            pos_y: pos[1],
            pos_z: pos[2],
            rot_x: rot[0],
            rot_y: rot[1],
            rot_z: rot[2],
            scale: 1.0,
            parent_piece_id: parent.map(Into::into),
        }
    }

    fn find<'a>(out: &'a [scene_desc::Piece], id: &str) -> &'a scene_desc::Piece {
        out.iter()
            .find(|p| p.id == id)
            .expect("piece should survive flattening")
    }

    fn close(got: [f32; 3], want: [f32; 3]) {
        assert!(
            Vec3::from(got).abs_diff_eq(Vec3::from(want), 1e-5),
            "got {got:?}, want {want:?}"
        );
    }

    #[test]
    fn an_unattached_piece_keeps_its_stored_pose() {
        let out = flatten_pieces(&[piece("deck", [1.0, 2.0, 0.5], [0.0, 0.0, 0.7], None)]);
        close(find(&out, "deck").pos, [1.0, 2.0, 0.5]);
        close(find(&out, "deck").rot, [0.0, 0.0, 0.7]);
    }

    #[test]
    fn an_attached_piece_lands_where_threejs_nesting_puts_it() {
        // The regression. Parent yawed a quarter turn about stored Z, child one
        // metre along the parent's local +X.
        //
        // three.js builds the parent group as `rotation.set(rotX, rotZ, rotY)`,
        // so a stored Z of +90 degrees is `Ry(+90)` in three space, which takes
        // the child's local +X to three `-Z` — data `-Y`. The child therefore
        // sits one metre *toward the audience* of the parent, at (1, 1, 0).
        //
        // Composing the chain in data space instead (the old bug) applies
        // `Rz(+90)` and sends the child to (1, 3, 0): mirrored across the
        // parent's Y axis, two metres out. A truss attached to a rotated deck
        // was landing there.
        let out = flatten_pieces(&[
            piece("deck", [1.0, 2.0, 0.0], [0.0, 0.0, FRAC_PI_2 as f64], None),
            piece("truss", [1.0, 0.0, 0.0], [0.0; 3], Some("deck")),
        ]);
        close(find(&out, "truss").pos, [1.0, 1.0, 0.0]);
    }

    #[test]
    fn a_parents_rotation_carries_into_the_child() {
        // Position is not the only half that composes: an unrotated child of a
        // rotated parent must come out carrying the parent's rotation, or the
        // truss is in the right place pointing the wrong way.
        //
        // Asserted as a *pose* rather than as an Euler triple. A stored Z of 90
        // degrees is `Ry(90)` in three space, which is exactly `euler_xyz_of`'s
        // gimbal singularity: the triple it recovers there rebuilds the right
        // matrix but is only good to about four digits, so comparing triples
        // would be testing the round-trip's precision, not the composition.
        // The residual here is ~3e-4 rad (0.02 degrees) — invisible on a truss,
        // but it is why the tolerance is not tighter.
        let out = flatten_pieces(&[
            piece("deck", [0.0; 3], [0.0, 0.0, FRAC_PI_2 as f64], None),
            piece("truss", [0.0; 3], [0.0; 3], Some("deck")),
        ]);
        let truss = find(&out, "truss");
        let got = coords::three_pose_from_data(truss.pos, truss.rot);
        let want = coords::three_pose_from_data([0.0; 3], [0.0, 0.0, FRAC_PI_2]);
        let probe = Vec3::new(1.0, 0.0, 0.0);
        assert!(
            got.transform_point3(probe)
                .abs_diff_eq(want.transform_point3(probe), 1e-3),
            "child pose {:?} does not carry the parent's rotation",
            got.transform_point3(probe)
        );
    }

    #[test]
    fn a_parents_scale_scales_the_childs_offset_and_its_own_scale() {
        let mut deck = piece("deck", [0.0; 3], [0.0; 3], None);
        deck.scale = 2.0;
        let out = flatten_pieces(&[
            deck,
            piece("truss", [1.0, 0.0, 0.0], [0.0; 3], Some("deck")),
        ]);
        close(find(&out, "truss").pos, [2.0, 0.0, 0.0]);
        assert!((find(&out, "truss").scale - 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_chain_composes_through_every_ancestor() {
        // Two quarter turns about stored Z compose to a half turn, so the
        // grandchild's local +X points back along the root's -X.
        let out = flatten_pieces(&[
            piece("a", [0.0; 3], [0.0, 0.0, FRAC_PI_2 as f64], None),
            piece("b", [0.0; 3], [0.0, 0.0, FRAC_PI_2 as f64], Some("a")),
            piece("c", [1.0, 0.0, 0.0], [0.0; 3], Some("b")),
        ]);
        close(find(&out, "c").pos, [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn a_cyclic_parent_leaves_the_piece_visible_at_its_local_pose() {
        // Documented behaviour: a deck in the wrong place is debuggable, a deck
        // that vanished is not.
        let out = flatten_pieces(&[
            piece("a", [1.0, 0.0, 0.0], [0.0; 3], Some("b")),
            piece("b", [0.0, 1.0, 0.0], [0.0; 3], Some("a")),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn a_dangling_parent_leaves_the_piece_at_its_local_pose() {
        let out = flatten_pieces(&[piece("truss", [1.0, 2.0, 3.0], [0.0; 3], Some("gone"))]);
        close(find(&out, "truss").pos, [1.0, 2.0, 3.0]);
    }
}

/// Every named view, rendered for real on the GPU.
///
/// The check is deliberately weak on *content* and strict on *not nothing*: a
/// camera that ended up inside a wall, behind the rig, or at the origin
/// produces a uniform frame, and that is the failure this catches for all six
/// views at once. Pixel-exact framing is the goldens' job.
#[cfg(test)]
mod gpu_view_tests {
    use super::*;
    use crate::models::universe::PrimitiveState;
    use luma_render::Catalogue;
    use std::time::Instant;

    const WIDTH: u32 = 240;
    const HEIGHT: u32 = 160;

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits one level below the repo root")
            .to_path_buf()
    }

    fn catalogue() -> Catalogue {
        Catalogue::load(&repo().join("gpui/crates/render/goldens/scenes.json"))
            .expect("the tracked golden catalogue loads")
    }

    /// The catalogue's pinned per-head state as an evaluated universe, which is
    /// the shape the production path actually takes.
    fn universe(scene: &scene_desc::Scene) -> UniverseState {
        let mut state = UniverseState::default();
        for (key, pinned) in &scene.state {
            state.primitives.insert(
                key.clone(),
                PrimitiveState {
                    dimmer: pinned.dimmer,
                    color: pinned.color,
                    strobe: pinned.strobe,
                    position: pinned.position,
                    speed: 1.0,
                },
            );
        }
        state
    }

    fn copy<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
        serde_json::from_value(serde_json::to_value(value).expect("serializes"))
            .expect("round trips")
    }

    #[test]
    fn gpu_renders_every_named_view_of_a_lit_rig() {
        let catalogue = catalogue();
        let source = catalogue
            .scenes
            .iter()
            .find(|scene| scene.id == "dense-venue")
            .expect("the catalogue keeps a whole-venue scene");
        let state = universe(source);
        let meshes = meshes_root(None);

        for view in View::ALL {
            let mut scene: scene_desc::Scene = copy(source);
            // The settings the agent path installs, so this is a test of the
            // frames agents actually get.
            scene.render = offscreen_render();
            let started = Instant::now();
            let (rgba, width, height) = render_rgba(
                scene,
                copy(&catalogue.definitions),
                Shot {
                    view,
                    booth: None,
                    state: Some(state.clone()),
                    time: 1.37,
                    size: (WIDTH, HEIGHT),
                },
                meshes.clone(),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", view.name()));
            let elapsed = started.elapsed();

            assert_eq!((width, height), (WIDTH, HEIGHT));
            assert_eq!(rgba.len(), WIDTH as usize * HEIGHT as usize * 4);
            let brightest = rgba
                .chunks_exact(4)
                .flat_map(|pixel| pixel[..3].iter().copied())
                .max()
                .expect("the frame has pixels");
            assert!(
                brightest > 24,
                "{} is black: brightest channel {brightest}",
                view.name()
            );
            let first = &rgba[..4];
            assert!(
                rgba.chunks_exact(4).any(|pixel| pixel != first),
                "{} is a single flat colour",
                view.name()
            );
            println!(
                "{:<14} {WIDTH}x{HEIGHT} {:>7.0} ms",
                view.name(),
                elapsed.as_secs_f64() * 1e3
            );
        }
    }

    #[test]
    fn gpu_render_png_is_a_decodable_png_of_the_requested_size() {
        let catalogue = catalogue();
        let source = catalogue
            .scenes
            .iter()
            .find(|scene| scene.id == "dense-venue")
            .expect("the catalogue keeps a whole-venue scene");
        let png = render_png(
            copy(source),
            copy(&catalogue.definitions),
            Shot {
                view: View::Front,
                booth: None,
                state: Some(universe(source)),
                time: 1.37,
                size: (WIDTH, HEIGHT),
            },
            meshes_root(None),
        )
        .expect("the front view renders");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR width/height, big-endian, immediately after the 8-byte signature
        // and the chunk's own length + type.
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), WIDTH);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), HEIGHT);
    }
}
