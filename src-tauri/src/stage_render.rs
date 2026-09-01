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
//! The parent chain is no longer flattened here, or anywhere else in this
//! crate: [`crate::venue_graph`] solves the venue and this module reads poses
//! off the result. `gpui/crates/app/src/visualizer.rs` shares
//! [`VenueGeometry::scene`]'s inputs through the same path, so there is one
//! answer to "where is the booth" rather than two.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};

use glam::Vec3;
use luma_render::scene_desc::{self, RenderSettings, VenueEnvironment};
use luma_render::{assets, build_frame_with, coords, Renderer, DEFAULT_SUBFRAMES};
use luma_scene::venue::ResolvedVenue;
use luma_scene::{Camera, View, Viewfinder};

use crate::database::local;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::models::universe::{PrimitiveState, UniverseState};
use crate::models::venue_graph::ResolvedNode;
use crate::services::groups::ResolvedFixture;

/// Vertical field of view every derived camera uses. Matches the desktop
/// viewport, so `view="front"` offscreen is the pose the operator would see.
pub const FOV_Y_DEG: f32 = 50.0;

/// Haze march resolution for offscreen frames. Full rate: an offscreen render
/// is not on a frame budget, and the haze is most of what a beam *is*.
const HAZE_RESOLUTION: f32 = 1.0;

/// Render settings one offscreen frame of `environment` uses.
///
/// The room is lit by the venue's own environment and by nothing else. There
/// used to be an "editor work light" here — a lit preset switched on because an
/// offscreen frame is read by someone who was not in the room and a dark stage
/// hides everything the score does not light. That reasoning was right and the
/// mechanism was a second lighting system: the same picture now comes out of
/// the *default* environment, indoor with the house at full, which is what
/// every venue has unless someone turned it down on purpose.
fn offscreen_render(environment: VenueEnvironment) -> RenderSettings {
    RenderSettings::room(environment, FOV_Y_DEG, HAZE_RESOLUTION)
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
    /// Every patched fixture, in patch order. The **patch**: what exists, how
    /// it is addressed, what mode it is in. Where it is comes from `venue`.
    pub fixtures: Vec<PatchedFixture>,
    /// The venue graph, solved. Every pose in the room, derived — see
    /// [`crate::venue_graph`].
    pub venue: ResolvedVenue,
    /// Keyed by `fixture_path`. A path whose bundle no longer resolves is
    /// absent rather than an error: a venue can outlive a fixture bundle.
    pub definitions: HashMap<String, FixtureDefinition>,
    /// What lights this room.
    ///
    /// Read off the venue record with the rest of it, so one snapshot answers
    /// where everything is *and* what it is seen by. A caller taking a picture
    /// under different light overwrites this before calling [`Self::scene`] —
    /// that is a camera setting, not an edit, and it never reaches the record.
    pub environment: VenueEnvironment,
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
        let environment = local::venues::get_venue(access).await?.environment;
        let fixtures = local::fixtures::get_patched_fixtures(access).await?;
        let venue = crate::venue_graph::resolved(access, fixtures_root).await?;
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
            venue,
            definitions,
            environment,
        })
    }

    /// Whether there is anything at all to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fixtures.is_empty() && self.venue.poses().count() <= 1
    }

    /// The renderer's description of this room, plus the definition table
    /// [`build_frame_with`] resolves fixture geometry through.
    ///
    /// The camera pose is left at the origin and unused: a derived view is a
    /// function of the framing and of the frame's aspect ratio, so it is
    /// resolved by [`render_rgba`] and installed on the built frame.
    ///
    /// The room is lit by [`Self::environment`] — see [`offscreen_render`].
    #[must_use]
    pub fn scene(&self) -> (scene_desc::Scene, BTreeMap<String, scene_desc::Definition>) {
        let render = offscreen_render(self.environment);
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
            aim_arrows: false,
            render,
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
            // A fixture whose definition did not resolve has no mesh and no
            // cone, and one nobody has placed has nowhere to be, so both are
            // left out rather than drawn as a guess at the origin.
            fixtures: self
                .fixtures
                .iter()
                .filter(|f| definitions.contains_key(&f.fixture_path))
                .filter_map(|f| {
                    let (pos, rot) = self.venue.pose(&f.id)?.data_pose();
                    Some(scene_desc::Fixture {
                        id: f.id.clone(),
                        fixture_path: f.fixture_path.clone(),
                        mode_name: f.mode_name.clone(),
                        pos: pos.map(|v| v as f32),
                        rot: rot.map(|v| v as f32),
                    })
                })
                .collect(),
            pieces: self.pieces(),
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
        BOOTH_KINDS.iter().find_map(|kind| {
            let pose = self
                .venue
                .poses()
                .find(|pose| catalog_kind(pose.catalog_ref.as_deref()) == *kind)?;
            let (pos, _) = pose.data_pose();
            Some(coords::world_from_data(Vec3::new(
                pos[0] as f32,
                pos[1] as f32,
                pos[2] as f32,
            )))
        })
    }

    /// The renderer's flat piece list, read off the solved venue.
    #[must_use]
    pub fn pieces(&self) -> Vec<scene_desc::Piece> {
        self.venue
            .poses()
            .filter_map(|pose| piece_of(&ResolvedNode::from(pose)))
            .collect()
    }
}

/// One solved node as the piece the renderer draws, or `None` when the pose is
/// a frame rather than an object — the room, a fixture (drawn from
/// [`scene_desc::Scene::fixtures`]), an array anchor, a node with no geometry.
/// Which of those it is, is [`luma_scene::venue::NodePose::is_set_piece`]'s
/// answer, carried across the boundary as [`ResolvedNode::set_piece`] so this
/// path and the agent binding and the React store cannot disagree.
///
/// Takes the wire projection rather than a [`luma_scene::venue::NodePose`]
/// because the desktop viewport only ever sees a venue from the far side of
/// the dispatch boundary, and "what a solved node looks like to the renderer"
/// has to have one answer for the viewport and the offscreen path both. The
/// scene *settings* differ between them; the geometry must not.
#[must_use]
pub fn piece_of(node: &ResolvedNode) -> Option<scene_desc::Piece> {
    if !node.set_piece {
        return None;
    }
    let catalog_ref = node.catalog_ref.as_deref()?;
    let params: luma_scene::venue::Params = node
        .params
        .iter()
        .map(|(key, value)| (key.clone(), *value))
        .collect();
    Some(scene_desc::Piece {
        id: node.id.clone(),
        geometry: geometry_of(catalog_ref, &params),
        kind: catalog_kind(Some(catalog_ref)).to_string(),
        pos: node.position.map(|v| v as f32),
        rot: node.rotation.map(|v| v as f32),
        // Uniform, and the catalog has no scaled variant: a piece that is not
        // its own size is not something the palette can describe.
        scale: 1.0,
    })
}

#[must_use]
/// The palette taxonomy of a catalog entry — `floor`, `truss`, `cdj`, ... —
/// which is what `scene_desc` and the booth search key on.
///
/// Read off [`luma_scene::catalog`] rather than stored: it was a column on
/// `stage_pieces` and drifted from the catalog it was copied out of.
pub fn catalog_kind(catalog_ref: Option<&str>) -> &'static str {
    catalog_ref
        .and_then(luma_scene::catalog::piece)
        .map_or("", |p| p.kind.as_str())
}

/// A node's geometry: a bundled GLB, or a generator standing at the node's own
/// parameters.
fn geometry_of(catalog_ref: &str, params: &luma_scene::venue::Params) -> scene_desc::Geometry {
    match luma_scene::catalog::piece(catalog_ref).map(|p| p.geometry) {
        Some(luma_scene::catalog::Geometry::Procedural(family)) => {
            scene_desc::Geometry::Procedural(luma_render::catalog::node_params(family, params))
        }
        // A `catalog_ref` the catalog has dropped still names a mesh on disk,
        // and drawing it is better than drawing nothing: the four ripped truss
        // GLBs left the palette but not the venues that already used them.
        // An assembly is named, not pathed: falling through to the mesh arm
        // would send the renderer looking for a GLB called `assembly/...`.
        Some(luma_scene::catalog::Geometry::Assembly(_)) => {
            scene_desc::Geometry::Assembly(catalog_ref.to_string())
        }
        _ => scene_desc::Geometry::mesh(catalog_ref.to_string()),
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
        // No chrome over a headless frame, so the fit gets the whole of it —
        // except outdoors, where the top of the frame belongs to the sky.
        let viewfinder = Viewfinder::new(FOV_Y_DEG, size.0 as f32 / size.1 as f32)
            .open_air(scene.render.sky.is_some());
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
    use luma_scene::venue::NodeKind as VenueNodeKind;

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
            address_pinned: false,
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

    /// An array node is the seat, not a copy. The solve gives it a pose so a
    /// successful `array(...)` has a placement to report and a frame for a
    /// child to hang off; the renderer must still draw only the members, or
    /// there is a speaker standing inside the middle one.
    #[test]
    fn an_array_anchor_is_not_drawn_but_its_members_are() {
        use luma_scene::venue::{Edge, Node, Params, VenueGraph, FLOOR_SOCKET};

        let sockets = crate::venue_graph::sockets(Path::new("resources/fixtures"))
            .expect("the catalog resolves");
        let mut graph = VenueGraph::new(Node {
            id: "venue".into(),
            kind: VenueNodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        });
        let mut params = Params::default();
        params.set("count", 3.0);
        params.set("span", 4.0);
        graph.insert(Node {
            id: "wall".into(),
            kind: VenueNodeKind::Array,
            catalog_ref: Some("stage_lab/speaker_dbr15.glb".into()),
            label: None,
            params,
        });
        graph
            .attach(
                "wall",
                Edge {
                    parent: "venue".into(),
                    my_socket: "mount".into(),
                    their_socket: FLOOR_SOCKET.into(),
                    roll: 0.0,
                },
                sockets,
            )
            .expect("speakers stand on the floor");
        let solved = luma_scene::venue::resolve(&graph, sockets);

        assert!(solved.pose("wall").is_some(), "the anchor is placed");
        let drawn: Vec<String> = solved
            .poses()
            .filter_map(|pose| piece_of(&ResolvedNode::from(pose)))
            .map(|piece| piece.id)
            .collect();
        assert_eq!(drawn, ["wall#0", "wall#1", "wall#2"]);
    }

    /// A venue holding one node per `(id, catalog piece, floor position)`,
    /// solved through the real resolver against the real catalog.
    ///
    /// Building it the long way rather than by hand is the point: `booth()`
    /// reads poses, and a hand-made pose would be testing the test.
    fn venue(pieces: &[(&str, &str, [f64; 3])]) -> ResolvedVenue {
        use luma_scene::venue::{Edge, Node, Params, VenueGraph, FLOOR_SOCKET};

        let sockets = crate::venue_graph::sockets(Path::new("resources/fixtures"))
            .expect("the catalog resolves");
        let mut graph = VenueGraph::new(Node {
            id: "venue".into(),
            kind: VenueNodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        });
        for (id, catalog_ref, pos) in pieces {
            let mut params = Params::default();
            // `v` is the floor socket's bitangent, which is data `-Y`.
            params.set("u", pos[0]);
            params.set("v", -pos[1]);
            params.set("trim", pos[2]);
            graph.insert(Node {
                id: (*id).into(),
                kind: VenueNodeKind::Piece,
                catalog_ref: Some((*catalog_ref).into()),
                label: None,
                params,
            });
            graph
                .attach(
                    id,
                    Edge {
                        parent: "venue".into(),
                        my_socket: "mount".into(),
                        their_socket: FLOOR_SOCKET.into(),
                        roll: 0.0,
                    },
                    sockets,
                )
                .expect("equipment sits on the floor");
        }
        luma_scene::venue::resolve(&graph, sockets)
    }

    fn geometry(venue: ResolvedVenue) -> VenueGeometry {
        VenueGeometry {
            fixtures: Vec::new(),
            venue,
            definitions: HashMap::new(),
            environment: VenueEnvironment::default(),
        }
    }

    #[test]
    fn booth_prefers_decks_over_the_mixer() {
        let geometry = geometry(venue(&[
            ("m", "stage_lab/mixer_djm_a9.glb", [1.0, 0.0, 0.0]),
            ("d", "stage_lab/cdj_3000x.glb", [2.0, 3.0, 1.0]),
        ]));
        // Data space (x, y, z) into world space is (x, -y, z). The pose is the
        // piece's *origin*, and a GLB's pivot is wherever the modeller left it
        // — a decimetre or so off its mount socket — so this asserts which
        // piece won, not a pose the catalog does not promise.
        let booth = geometry.booth().expect("a cdj is a booth");
        assert!(booth.distance(Vec3::new(2.0, -3.0, 1.0)) < 0.5, "{booth:?}");
    }

    #[test]
    fn booth_is_none_without_decks_or_a_mixer() {
        let geometry = geometry(venue(&[(
            "s",
            "stage_lab/speaker_dbr15.glb",
            [0.0, 0.0, 0.0],
        )]));
        assert_eq!(geometry.booth(), None);
    }

    /// An empty venue is one node — the room — and nothing to draw.
    #[test]
    fn an_empty_venue_has_no_pieces() {
        let geometry = geometry(venue(&[]));
        assert!(geometry.is_empty());
        assert!(geometry.pieces().is_empty());
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
