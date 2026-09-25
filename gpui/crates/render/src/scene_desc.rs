//! The golden-scene catalogue in `goldens/scenes.json`.
//!
//! The catalogue is frozen data. It describes what each golden frame contains,
//! and this module is its deserialiser. Edit the JSON directly when a scene
//! must change, then recapture the goldens with `render-goldens`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// The whole golden-scene catalogue, as `goldens/scenes.json` stores it.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalogue {
    /// Jitter subframes the original capture settled through before shooting.
    /// The renderer averages a fixed number of subframes instead (spec §6), so
    /// the output is deterministic.
    pub warmup_frames: u32,
    /// Canvas size the goldens were captured at.
    pub viewport: Viewport,
    /// Device pixel ratio the capture pinned.
    pub device_scale_factor: f32,
    /// Fixture definitions keyed by `fixturePath`.
    pub definitions: BTreeMap<String, Definition>,
    /// The eight golden scenes, in catalogue order.
    pub scenes: Vec<Scene>,
}

/// CSS pixels; multiply by `device_scale_factor` for the captured buffer.
#[derive(Debug, Deserialize, Serialize)]
pub struct Viewport {
    /// Width in CSS pixels.
    pub width: u32,
    /// Height in CSS pixels.
    pub height: u32,
}

/// One golden scene: a complete description of a frame's inputs.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    /// Stable id; also the filename stem of every frame.
    pub id: String,
    /// Clock values, in seconds, this scene is captured at.
    pub times: Vec<f32>,
    /// Fixed camera pose.
    pub camera: CameraPose,
    /// Whether the editor affordances (gizmo, selection) are mounted.
    pub editing: bool,
    /// Draw each fixture's rest aim as an arrow out of its mount point.
    ///
    /// Sits beside `editing` rather than inside [`Editor`] because it is a
    /// property of the *picture asked for*, not of what a running editor is
    /// doing: an agent's venue render carries it and has no editor at all.
    #[serde(default)]
    pub aim_arrows: bool,
    /// Every render dial the frame depends on, pinned.
    pub render: RenderSettings,
    /// Drives the selection outline, and its first entry is the primary.
    pub selected_fixture_ids: Vec<String>,
    /// Live editor state: what the app's selection and gizmo are doing right
    /// now. Never captured — the catalogue describes a picture, and this is
    /// what a *running* editor draws over one. `selected_fixture_ids` predates
    /// it and stays where the goldens write it, so the selection is spelled in
    /// two fields and `overlay::pivot` is the one reader of both.
    #[serde(skip)]
    pub editor: Editor,
    /// Patched fixtures, in submission order.
    pub fixtures: Vec<Fixture>,
    /// Stage pieces (decks, trusses, speakers).
    pub pieces: Vec<Piece>,
    /// Fixed primitive state per `"<fixtureId>:<head>"` key.
    pub state: BTreeMap<String, PrimitiveState>,
}

/// What the editor is doing to the scene it is drawn over.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Editor {
    /// Selected stage pieces. Fixtures are in [`Scene::selected_fixture_ids`].
    pub selected_piece_ids: Vec<String>,
    /// The subset of [`Self::selected_piece_ids`] the transform gizmo may
    /// stand on. Selection and grab-ability are two facts: a snapped piece is
    /// selected and highlighted like anything else, but its pose is a relation
    /// and the widget must not offer axes over one — so the highlight reads
    /// the full list and [`crate::overlay::pivot`] reads only this one.
    pub gizmo_piece_ids: Vec<String>,
    /// Which widget the transform gizmo shows.
    pub gizmo: luma_scene::GizmoMode,
    /// UVZ frame and joint freedoms used by every handle.
    pub gizmo_space: luma_scene::gizmo::GizmoSpace,
    /// The handle under the pointer (or being dragged), lit so the hand knows
    /// what it is about to grab before it grabs it.
    pub hover: Option<luma_scene::GizmoHandle>,
    /// What the builder is about to do, drawn over the room it would change.
    /// Empty on every screen that is not the stage page.
    pub build: Build,
}

/// The builder's uncommitted intent: the piece under the cursor, the run being
/// measured, and the joints worth pointing at.
///
/// Poses are in data space, the convention [`Piece::pos`]/[`Piece::rot`] use, so
/// the drawer converts them with `three_pose_from_data` exactly as it does a
/// placed piece. Everything here is derived state — the resolver's answer for
/// the current pointer — and is rebuilt each frame rather than edited.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Build {
    /// Placements not yet committed: the piece under the cursor, or every body
    /// of a row being fitted to a face.
    ///
    /// A list rather than one, because a row is previewed the same way a single
    /// piece is — the operator adjusting a count is asking "what would eight
    /// look like", and one ghost could only answer it one light at a time.
    pub ghosts: Vec<Ghost>,
    /// A run being measured — the extend ray's two ends.
    pub measure: Option<Measure>,
    /// Sockets worth pointing at while something is held.
    pub sockets: Vec<SocketMark>,
    /// The piece the current landing would attach to, lit so the hand knows
    /// what it is placing *onto* before it commits — the constraint-first
    /// answer to "where is this going".
    pub host: Option<String>,
}

/// A placement preview: the held piece's own geometry, at the pose the resolver
/// would commit it to.
#[derive(Debug, Clone, PartialEq)]
pub struct Ghost {
    /// The held piece's shape, drawn flat rather than lit.
    pub geometry: Geometry,
    /// When the held thing is a light: its definition key, so the overlay can
    /// draw the *housing* the commit will draw ([`crate::frame`]'s
    /// `housing_draws`) instead of standing `geometry` in for it. `geometry`
    /// stays the fallback for a definition the catalogue has not loaded.
    pub fixture: Option<String>,
    /// Data-space position.
    pub pos: [f32; 3],
    /// Data-space Euler triple.
    pub rot: [f32; 3],
    /// Uniform scale of `geometry`, as [`Piece::scale`]. A light's housing
    /// ignores it and is drawn at the definition's own size.
    pub scale: f32,
    /// A ghost the resolver would refuse — drawn red rather than white. Only
    /// the extend run raises it: its refusal is a length past a measured gap,
    /// and the red ghost is the measurement answering. A refused *placement*
    /// pushes no ghost at all — the preview vanishing is that refusal.
    pub refused: bool,
}

/// The extend ray a run is being measured along.
///
/// Both ends are data-space points; the metres readout that belongs beside them
/// is a gpui element the app projects, never geometry from this layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Measure {
    /// Where the run starts.
    pub from: [f32; 3],
    /// Where the pointer has dragged it to.
    pub to: [f32; 3],
    /// The run does not fit — the same refusal the ghost shows.
    pub refused: bool,
}

/// One joint on a piece already in the room, marked while something is held.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SocketMark {
    /// Data-space position of the joint.
    pub pos: [f32; 3],
    /// Outward normal in data space, so the mark can lean the way the joint
    /// faces.
    pub normal: [f32; 3],
    /// How the mark reads against what is held.
    pub state: SocketMarkState,
}

/// How prominent a socket mark is, from "there is a joint here" to "release and
/// it lands on this one".
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SocketMarkState {
    /// A joint that exists; nothing is aiming at it.
    #[default]
    Open,
    /// The held piece could mate here.
    Compatible,
    /// The held piece is snapped to it right now.
    Latched,
}

/// Y-up camera space, the space the catalogue stores camera poses in.
#[derive(Debug, Deserialize, Serialize)]
pub struct CameraPose {
    /// Eye position.
    pub position: [f32; 3],
    /// Look-at point.
    pub target: [f32; 3],
}

/// The render settings the renderer reads. There is no bloom (spec §2.5), and
/// the device pixel ratio belongs to the capture, not to the renderer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderSettings {
    /// Background and image-based ambient approximation. It is independent of
    /// both the sun and fixture haze, so a black venue can still have either.
    pub environment: Environment,
    /// Analytic participating medium used by fixture beams.
    pub haze: HazeSettings,
    /// The editor's directional key light. `None` means genuinely off.
    pub sun: Option<DirectionalLight>,
    /// Whether the fading editor ground grid is drawn.
    pub show_grid: bool,
    /// Whether the venue's ground plane is drawn. Off for renders whose
    /// subject is a single piece (the palette thumbnails), on everywhere a
    /// room is the subject.
    pub show_floor: bool,
    /// Whether flown pieces are drawn hanging on rigging cables. Room chrome,
    /// like the two above: off for a render whose subject is one piece, on
    /// wherever the subject is a rig.
    pub show_cables: bool,
    /// Whether unlit editor gizmos are drawn — today the downstage compass
    /// arrow on the floor (`shaders/grid.wgsl`), which is the one piece of
    /// chrome the renderer paints without being asked for it.
    ///
    /// One flag for the class, not one per glyph: a gizmo is display UI that
    /// stands *in* the scene, and every one of them is wrong for the same
    /// reason at the same moment — a beauty render, a thumbnail, a plate for
    /// print. A second gizmo joins this flag. Aim arrows are not gizmos in
    /// this sense: `Scene::aim_arrows` is an explicit request for a diagram,
    /// and keeps its own switch.
    pub show_gizmos: bool,
    /// The venue environment this room is lit by, when the subject *is* a room.
    ///
    /// `environment` and `sun` above are this value's bounds-free half, already
    /// resolved by [`crate::house::fill`]; this field is what the frame builder
    /// needs to hang the house rig, which it cannot do until it knows how big
    /// the room is. `None` means the subject is not a room — a palette
    /// thumbnail, or a tracked contract capture — and no house is hung.
    pub house: Option<VenueEnvironment>,
    /// The physically based sky, when the room is open air.
    ///
    /// `None` — the default everywhere, including every tracked contract
    /// capture — means no atmosphere: the background is
    /// [`Environment::background`] and the probe, if any, is an authored HDR.
    /// `Some` replaces all three of the background, the ambient probe and the
    /// directional light with one self-consistent atmosphere; see
    /// [`SkyParams`].
    pub sky: Option<SkyParams>,
    /// Renderer diagnostic output. `Pbr` is the authored display path.
    pub debug_view: DebugView,
    /// The camera: exposure, tone curve and glare. Every constructor starts at
    /// [`Look::NEUTRAL`], the picture the tracked images were captured with;
    /// the live stage sets its own.
    pub look: Look,
    /// Whether fixture cones contribute punctual light to opaque surfaces.
    pub fixture_surface_lighting: bool,
    /// Whether opaque venue geometry casts shadows into fixture light and haze.
    pub fixture_shadows: bool,
    /// Retained shadow maps for all fixtures instead of the 16-map budget.
    pub geometry_shadows: bool,
    /// Paint cluster occupancy instead of authored PBR shading.
    pub cluster_debug: bool,
    /// The local reflection probes: a view choice for this session, never
    /// saved. Defaults from `LUMA_PROBES` and `LUMA_PROBE_DEBUG`.
    #[serde(skip)]
    pub probes: ProbeView,
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// How much work the renderer may spend per frame. A device choice, not
    /// the room's look; see [`Quality`]. Omitted when high, so descriptors
    /// written before it existed serialize unchanged.
    #[serde(skip_serializing_if = "Quality::is_high")]
    pub quality: Quality,
    /// Positional shadow anchor of the golden catalogue's directional light.
    /// Not serialized; only the golden catalogue adapter sets it, so the
    /// orthographic shadow projection of those captures stays byte-exact.
    #[serde(skip)]
    pub(crate) golden_shadow_eye: Option<[f32; 3]>,
}

/// The local reflection probes (`probes.rs`), as the view shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProbeView {
    /// Light surfaces from the probes over the stage, not the sky probe
    /// alone.
    pub enabled: bool,
    /// Draw each probe as a small ball showing its cube.
    pub debug: bool,
}

impl ProbeView {
    /// On, balls off: the constant constructors' view, which no start-up
    /// variable changes.
    pub const PLAIN: Self = Self {
        enabled: true,
        debug: false,
    };
}

impl Default for ProbeView {
    /// On, balls off, unless `LUMA_PROBES=0` or `LUMA_PROBE_DEBUG=1` say
    /// otherwise at start-up.
    fn default() -> Self {
        static DEFAULT: std::sync::OnceLock<ProbeView> = std::sync::OnceLock::new();
        *DEFAULT.get_or_init(|| Self {
            enabled: std::env::var_os("LUMA_PROBES").is_none_or(|value| value != "0"),
            debug: std::env::var_os("LUMA_PROBE_DEBUG").is_some_and(|value| value == "1"),
        })
    }
}

/// The renderer's cost level.
///
/// `High` is the reference picture. `Low` is for a laptop GPU: it lowers
/// multisampling, the far-field fog grid's resolution and the haze shadow
/// walk's detail, and it coarsens haze shadows further while the camera
/// moves. Callers lower the haze and output resolutions themselves, because
/// those are already their own settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    /// The reference picture.
    #[default]
    High,
    /// A laptop GPU's budget.
    Low,
}

impl Quality {
    /// Whether this is [`Self::High`]; serde's skip test.
    #[must_use]
    pub fn is_high(&self) -> bool {
        *self == Self::High
    }

    /// The haze target's fraction of the output resolution.
    #[must_use]
    pub fn haze_resolution(self) -> f32 {
        match self {
            Self::High => crate::LIVE_HAZE_RESOLUTION,
            Self::Low => 0.5,
        }
    }

    /// Stochastic samples per gobo beam.
    #[must_use]
    pub fn haze_steps(self) -> u32 {
        match self {
            Self::High => 8,
            Self::Low => 4,
        }
    }

    /// Most pixels a live viewport renders before the compositor upscales,
    /// when this level sets its own limit.
    #[must_use]
    pub fn pixel_budget(self) -> Option<f64> {
        match self {
            Self::High => None,
            Self::Low => Some(1.0e6),
        }
    }
}

/// Runtime-selectable renderer diagnostic output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DebugView {
    /// Lit PBR surfaces plus fixture haze through the AgX display transform.
    #[default]
    Pbr,
    /// Linear base colour after factor and sRGB texture decode.
    BaseColor,
    /// Final world-space shading normal, remapped to zero-to-one.
    Normals,
    /// Metallic factor after the linear metallic-roughness map.
    Metallic,
    /// Perceptual roughness after the linear metallic-roughness map.
    Roughness,
    /// Directional shadow visibility.
    Shadow,
    /// Full-resolution scene depth.
    Depth,
    /// Fixture volumetric accumulation without opaque surfaces.
    VolumetricAccumulation,
}

impl DebugView {
    /// Numeric shader selector shared by the scene and composite passes.
    #[must_use]
    pub const fn shader_code(self) -> u32 {
        match self {
            Self::Pbr => 0,
            Self::BaseColor => 1,
            Self::Normals => 2,
            Self::Metallic => 3,
            Self::Roughness => 4,
            Self::Shadow => 5,
            Self::Depth => 6,
            Self::VolumetricAccumulation => 7,
        }
    }
}

/// How the camera turns scene light into a picture: exposure, the tone curve
/// and the glare around hot sources.
///
/// [`Self::NEUTRAL`] is the single-pass picture every tracked image was
/// captured with: fixed exposure, AgX, nothing added. Anything else runs the
/// post chain in `post.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Look {
    /// Display transform from exposed scene light to the display.
    pub tone: ToneCurve,
    /// How bright the scene is taken to be.
    pub exposure: Exposure,
    /// Bloom, star streaks and lens glow around hot sources.
    pub glare: Glare,
}

impl Look {
    /// Fixed exposure, AgX, no glare: the look before the post chain existed.
    pub const NEUTRAL: Self = Self {
        tone: ToneCurve::Agx,
        exposure: Exposure {
            auto: false,
            ev: 0.0,
            min_ev: Exposure::STAGE.min_ev,
            max_ev: Exposure::STAGE.max_ev,
        },
        glare: Glare::OFF,
    };

    /// The live stage's default: metered exposure, a punchier curve and a
    /// strong glare, so a light at full reads as blinding.
    pub const STAGE: Self = Self {
        tone: ToneCurve::AgxPunchy,
        exposure: Exposure::STAGE,
        glare: Glare::STAGE,
    };

    /// Whether this look needs the post chain rather than the single
    /// composite pass.
    #[must_use]
    pub fn needs_post(&self) -> bool {
        *self != Self::NEUTRAL
    }
}

impl Default for Look {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// The display transform. All three keep scene-linear 1.0 near the same
/// display value; they differ in contrast and in how colour goes to white.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToneCurve {
    /// three's AgX: soft contrast, bright colours go to white.
    #[default]
    Agx,
    /// AgX with Blender's "Punchy" look: more contrast and saturation.
    AgxPunchy,
    /// Narkowicz's fit of the ACES filmic curve, per channel: hard contrast,
    /// saturated colours stay saturated.
    Aces,
}

impl ToneCurve {
    /// Shader selector, shared with `tone.wgsl`.
    #[must_use]
    pub const fn shader_code(self) -> u32 {
        match self {
            Self::Agx => 0,
            Self::AgxPunchy => 1,
            Self::Aces => 2,
        }
    }
}

/// Exposure in stops. With `auto` the renderer meters each frame and `ev` is
/// the compensation added to the metered value; without it `ev` is the
/// exposure itself (0 = the fixed exposure the transport was tuned at).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exposure {
    /// Meter the frame and adapt to it over time.
    pub auto: bool,
    /// Compensation (auto) or exposure (manual), in stops.
    pub ev: f32,
    /// Lowest metered exposure, in stops. A frame full of blinders cannot
    /// push the rest of the stage below this.
    pub min_ev: f32,
    /// Highest metered exposure, in stops. A dark room with a few beams
    /// cannot lift the room above this.
    pub max_ev: f32,
}

impl Exposure {
    /// Metered, no compensation, two and a half stops down and one up. One
    /// up is what keeps a dim cue dim: a dark room meters at this clamp.
    pub const STAGE: Self = Self {
        auto: true,
        ev: 0.0,
        min_ev: -2.5,
        max_ev: 1.0,
    };
    /// The range the exposure control offers, in stops.
    pub const RANGE: std::ops::RangeInclusive<f32> = -4.0..=4.0;
}

/// Glare around hot sources, added after the tone curve so full white stays
/// white and the halo spreads past it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Glare {
    /// Overall strength; 0 turns the glare and lens glow off.
    pub strength: f32,
    /// Exposed scene light above which a pixel glares, as a multiple of
    /// diffuse white.
    pub threshold: f32,
    /// The diffraction pattern's weight in the glare kernel (see
    /// [`GlareStyle`]), 0 to 1: 0.5 is physical, 0 leaves the veil alone.
    pub star: f32,
    /// What the diffraction looks like. Absent in a look stored before
    /// there was a choice, which means the default.
    #[serde(default)]
    pub style: GlareStyle,
}

/// The shape of the glare around a hot source. Every style is physical:
/// `psf.rs` builds its kernel from a veil and, but for bloom, the
/// diffraction pattern of an aperture with a lens's dust and scratches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GlareStyle {
    /// The eye's veiling glare alone (Vos): a glow with a long power-law
    /// tail across the frame.
    Bloom,
    /// A camera lens: an iris of nine rounded blades, dust and scratches.
    /// Eighteen thin rays, irregular streaks and coloured haze, over a faint
    /// veil.
    #[default]
    Aperture,
    /// The eye: the veil, and a corona of many fine radial streaks from the
    /// lens's ciliary fibres.
    Eye,
    /// A camera stopped down to six straight blades: six strong rays over
    /// the same dust, scratches and faint veil.
    Star,
}

impl GlareStyle {
    /// Every style, in the order they are offered.
    pub const ALL: [Self; 4] = [Self::Bloom, Self::Aperture, Self::Eye, Self::Star];
}

impl Glare {
    /// No glare.
    pub const OFF: Self = Self {
        strength: 0.0,
        threshold: 2.0,
        star: 0.0,
        style: GlareStyle::Aperture,
    };
    /// The live stage's default.
    pub const STAGE: Self = Self {
        strength: 0.7,
        threshold: 2.0,
        star: 0.5,
        style: GlareStyle::Aperture,
    };
    /// Whether any glare is drawn.
    #[must_use]
    pub fn on(&self) -> bool {
        self.strength > 0.0
    }
}

/// Scene-wide background and ambient contribution, in linear RGB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    /// Linear RGB clear colour behind the scene.
    pub background: [f32; 3],
    /// Linear RGB ambient-light colour.
    pub ambient_color: [f32; 3],
    /// Scalar ambient strength. Zero disables ambient light without changing
    /// the background.
    pub ambient_intensity: f32,
    /// Optional HDR image-based light. When absent, `ambient_color` and
    /// `ambient_intensity` remain the complete indirect-light fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<EnvironmentProbe>,
}

/// One Radiance HDR environment used for image-based lighting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProbe {
    /// Path relative to the renderer asset library root.
    pub asset: String,
    /// Scalar multiplier on both diffuse and specular image-based light.
    pub intensity: f32,
    /// Rotation around world Z, in degrees.
    pub rotation_deg: f32,
    /// Whether the probe is also painted behind scene geometry.
    pub visible: bool,
}

/// A physically based atmosphere: where the sun is, and nothing else.
///
/// The model is Hillaire (EGSR 2020) and lives in `crate::atmosphere`. Two
/// angles are the whole authored surface, because everything else about a sky
/// follows from them: the colour of the horizon, the colour of the sun, the
/// brightness of the day and the direction of every cast shadow are one
/// calculation, not four dials that can be set to disagree.
///
/// # Angles
///
/// Both are in the renderer's Z-up world. Elevation is degrees above the
/// horizon. Azimuth is degrees counter-clockwise from world +X, the same
/// convention the view fits in `luma_scene::camera` use — so `+90` is toward
/// the crowd (+Y) and `270` is straight upstage (-Y).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkyParams {
    /// Degrees above the horizon. Negative is civil twilight, which the model
    /// renders rather than clamping away.
    pub sun_elevation_deg: f32,
    /// Degrees counter-clockwise from world +X.
    pub sun_azimuth_deg: f32,
    /// Albedo of the ground the sky bounces sunlight off, linear RGB, 0 to
    /// 1: the floor's mean colour. It is what fills the sky below the
    /// horizon, where a venue's floor dissolves into the background, and
    /// what the ambient light bounces up off the ground. A single number,
    /// as older descriptors hold, is read as grey.
    #[serde(deserialize_with = "albedo_rgb")]
    pub ground_albedo: [f32; 3],
    /// Display exposure, or `None` for the elevation-fitted default.
    ///
    /// The default is the honest answer for almost every frame: dusk to noon is
    /// many stops, and the fitted curve is what keeps a rig legible across all
    /// of them. Set it only to hold a deliberate look — a render that must
    /// match another at a different hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,
    /// The cloud layer over the venue. Omitted when clear, so a descriptor
    /// written before clouds existed serializes unchanged.
    #[serde(default, skip_serializing_if = "CloudCover::is_clear")]
    pub clouds: CloudCover,
}

/// An RGB albedo, or a single number for a grey one.
pub(crate) fn albedo_rgb<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[f32; 3], D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Albedo {
        Grey(f32),
        Rgb([f32; 3]),
    }
    Ok(match Albedo::deserialize(deserializer)? {
        Albedo::Grey(value) => [value; 3],
        Albedo::Rgb(rgb) => rgb,
    })
}

impl SkyParams {
    /// Degrees off dead upstage the default sun stands.
    ///
    /// A sun on the centre line puts the disc behind the middle of the rig and
    /// the picture reads as a diagram. Twenty degrees is enough that the rake
    /// of the light is visible without losing the backlight.
    const DEFAULT_OFFSET_DEG: f32 = 20.0;

    /// The outdoor default: dusk behind the stage.
    ///
    /// Four degrees is the elevation where the ozone term is doing its most
    /// visible work — warm along the horizon, blue overhead — and where a rig
    /// backlit by the sun silhouettes. Upstage is world -Y, i.e. azimuth 270,
    /// offset so the disc is not dead centre.
    pub const DUSK: Self = Self {
        sun_elevation_deg: 4.0,
        sun_azimuth_deg: 270.0 - Self::DEFAULT_OFFSET_DEG,
        ground_albedo: [0.1; 3],
        exposure: None,
        clouds: CloudCover::Clear,
    };

    /// The sky for an open-air venue whose sun is `elevation_deg` up and
    /// `azimuth_deg` round.
    ///
    /// **This is the environment seam.** [`VenueEnvironment::Outdoor`] carries
    /// the sun's two angles, and this is the function that turns them into a
    /// sky: `crate::house::fill`'s outdoor arm returns it, and the atmosphere
    /// supplies the background, the ambient and the sun. The renderer prefers
    /// the sky's sun over an authored one whenever [`RenderSettings::sky`] is
    /// set.
    #[must_use]
    pub fn outdoor(elevation_deg: f32, azimuth_deg: f32) -> Self {
        Self {
            sun_elevation_deg: if elevation_deg.is_finite() {
                elevation_deg.clamp(-90.0, 90.0)
            } else {
                Self::DUSK.sun_elevation_deg
            },
            sun_azimuth_deg: wrap_azimuth(azimuth_deg),
            ..Self::DUSK
        }
    }
}

/// An azimuth brought into 0..=360 degrees.
///
/// A value already in range is kept as it is, so 360 stays 360 and a slider
/// held at its top does not jump to its bottom. Any other finite value wraps
/// round the circle; a non-finite one reads as [`SkyParams::DUSK`]'s.
#[must_use]
pub fn wrap_azimuth(deg: f32) -> f32 {
    if !deg.is_finite() {
        SkyParams::DUSK.sun_azimuth_deg
    } else if (0.0..=360.0).contains(&deg) {
        deg
    } else {
        deg.rem_euclid(360.0)
    }
}

/// The cloud layer over an open-air venue: one of five skies.
///
/// A preset rather than dials, for the same reason [`SkyParams`] is two
/// angles: coverage, thickness, altitude and darkness are one weather, and
/// dials for each could be set to a sky nobody has seen. `crate::atmosphere`
/// turns each into a layer, and the same layer dims the sun, greys the
/// ambient and fills the background, so the three cannot disagree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloudCover {
    /// No clouds: the sky this app has always drawn.
    #[default]
    Clear,
    /// Small separate cumulus with blue sky between them.
    FairWeather,
    /// High, thin cirrus streaks: the sun shines through them.
    Wispy,
    /// Large cumulus over about half the sky; they can cover the sun.
    Scattered,
    /// One unbroken grey layer. The light is soft and has almost no shadow.
    Overcast,
    /// A thick, dark, low layer under a dim sky.
    Storm,
}

impl CloudCover {
    /// Every preset, clearest first: the order the editor offers them in.
    pub const ALL: [Self; 6] = [
        Self::Clear,
        Self::FairWeather,
        Self::Wispy,
        Self::Scattered,
        Self::Overcast,
        Self::Storm,
    ];

    /// Whether this is [`Self::Clear`]; serde's skip test.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        *self == Self::Clear
    }

    /// The name the editor shows, in sentence case.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::FairWeather => "Fair weather",
            Self::Wispy => "Wispy",
            Self::Scattered => "Scattered",
            Self::Overcast => "Overcast",
            Self::Storm => "Storm",
        }
    }
}

/// The ground a venue stands on: one of a few real materials.
///
/// Venue truth, like [`CloudCover`]: it sits on the venue's environment
/// record. Each is a scanned or procedural CC0 material set
/// (`resources/meshes/floors`), drawn by [`crate::floor`]. An open-air venue
/// offers the ground kinds and a room offers floor coverings; concrete is
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Floor {
    /// A lawn.
    Grass,
    /// Trodden earth with straw, patched with mud.
    Dirt,
    /// Fine, even sand.
    FineSand,
    /// A beach: pale dry sand with fine wind ripples.
    Beach,
    /// Coarse orange sand with grit.
    GravellySand,
    /// A car park or road.
    Asphalt,
    /// Worn concrete, indoors or out.
    Concrete,
    /// Fine grey gravel.
    Gravel,
    /// Dark, worn stage planks.
    StageDeck,
    /// Varnished hall floorboards.
    HallFloor,
    /// Matte black dance floor.
    BlackStage,
    /// Plain dark loop-pile carpet.
    Carpet,
}

impl Floor {
    /// The floors an open-air venue offers, in the editor's order.
    pub const OUTDOOR: [Self; 8] = [
        Self::Grass,
        Self::Dirt,
        Self::FineSand,
        Self::Beach,
        Self::GravellySand,
        Self::Asphalt,
        Self::Concrete,
        Self::Gravel,
    ];

    /// The floors a room offers, in the editor's order.
    pub const INDOOR: [Self; 5] = [
        Self::Concrete,
        Self::StageDeck,
        Self::HallFloor,
        Self::BlackStage,
        Self::Carpet,
    ];

    /// The floors a venue of this kind offers.
    #[must_use]
    pub fn options(indoor: bool) -> &'static [Self] {
        if indoor {
            &Self::INDOOR
        } else {
            &Self::OUTDOOR
        }
    }

    /// What a venue of this kind stands on until someone chooses: the
    /// nearest to the floor it had before floors existed. Indoors that was
    /// black paint; outdoors a plain grey ground.
    #[must_use]
    pub fn default_for(indoor: bool) -> Self {
        if indoor {
            Self::BlackStage
        } else {
            Self::Concrete
        }
    }

    fn is_indoor_default(&self) -> bool {
        *self == Self::default_for(true)
    }

    fn is_outdoor_default(&self) -> bool {
        *self == Self::default_for(false)
    }

    fn indoor_default() -> Self {
        Self::default_for(true)
    }

    fn outdoor_default() -> Self {
        Self::default_for(false)
    }

    /// The name the editor shows, in sentence case.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Grass => "Grass",
            Self::Dirt => "Dirt",
            Self::FineSand => "Fine sand",
            Self::Beach => "Beach",
            Self::GravellySand => "Gravelly sand",
            Self::Asphalt => "Asphalt",
            Self::Concrete => "Concrete",
            Self::Gravel => "Gravel",
            Self::StageDeck => "Stage deck",
            Self::HallFloor => "Hall floor",
            Self::BlackStage => "Black stage floor",
            Self::Carpet => "Carpet",
        }
    }

    /// The material set's directory under `resources/meshes/floors`, and its
    /// key in that directory's `floors.json`.
    #[must_use]
    pub fn set(self) -> &'static str {
        match self {
            Self::Grass => "grass",
            Self::Dirt => "dirt",
            Self::FineSand => "fine_sand",
            Self::Beach => "beach",
            Self::GravellySand => "gravelly_sand",
            Self::Asphalt => "asphalt",
            Self::Concrete => "concrete",
            Self::Gravel => "gravel",
            Self::StageDeck => "stage_deck",
            Self::HallFloor => "hall_floor",
            Self::BlackStage => "black_stage",
            Self::Carpet => "carpet",
        }
    }
}

/// What kind of room a venue is, and the one dial that mode has.
///
/// This is **venue truth**, not a render dial: it sits on the venue record
/// beside its name, and every picture of that room — the editor viewport, an
/// agent's offscreen frame — is taken under it. [`crate::house`] is the one
/// place it turns into light.
///
/// A room is either lit by its own house rig or by the sky. Indoors the one
/// question is "how far up?" — how bright the house is. Outdoors it is where
/// the sun is: how high, and from which side. Everything else about either
/// mode is derived, so there is nothing else to store and nothing that can
/// disagree.
///
/// The dials are read through [`Self::house_level`],
/// [`Self::sun_elevation_deg`] and [`Self::sun_azimuth_deg`], which clamp or
/// wrap: a value that arrived from a database column or an agent cannot put
/// the renderer in a state it has no answer for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum VenueEnvironment {
    /// A room with a house rig over it. `house_level` is 0 (dark) to 1 (full).
    Indoor {
        /// How far up the house lights are.
        #[serde(rename = "houseLevel")]
        house_level: f32,
        /// The floor. A record written before floors existed, or one on
        /// the default, has no key.
        #[serde(
            default = "Floor::indoor_default",
            skip_serializing_if = "Floor::is_indoor_default"
        )]
        floor: Floor,
    },
    /// Open air. `sun_elevation_deg` is the time of day: -90 (midnight) to
    /// 90 (overhead).
    ///
    /// The sky and the sun light are not built here — see [`crate::house`].
    Outdoor {
        /// Degrees above the horizon.
        #[serde(rename = "sunElevationDeg")]
        sun_elevation_deg: f32,
        /// Degrees counter-clockwise from world +X, as
        /// [`SkyParams::sun_azimuth_deg`]. A record written before this dial
        /// existed reads as [`SkyParams::DUSK`]'s.
        #[serde(rename = "sunAzimuthDeg", default = "dusk_azimuth")]
        sun_azimuth_deg: f32,
        /// The sky's cloud cover. A record written before clouds existed,
        /// or one that is clear, has no key: clear is the sky it always had.
        #[serde(default, skip_serializing_if = "CloudCover::is_clear")]
        clouds: CloudCover,
        /// The ground, as [`Self::Indoor`]'s floor.
        #[serde(
            default = "Floor::outdoor_default",
            skip_serializing_if = "Floor::is_outdoor_default"
        )]
        floor: Floor,
    },
}

impl Default for VenueEnvironment {
    /// Indoor, house at full: the picture this app has always drawn.
    fn default() -> Self {
        Self::indoor(1.0)
    }
}

impl VenueEnvironment {
    /// An indoor room at `level`, clamped to 0..=1.
    #[must_use]
    pub fn indoor(level: f32) -> Self {
        Self::Indoor {
            house_level: level.clamp(0.0, 1.0),
            floor: Floor::default_for(true),
        }
    }

    /// Open air with the sun `deg` above the horizon, clamped to -90..=90,
    /// at [`SkyParams::DUSK`]'s azimuth.
    #[must_use]
    pub fn outdoor(deg: f32) -> Self {
        Self::Outdoor {
            sun_elevation_deg: deg.clamp(-90.0, 90.0),
            sun_azimuth_deg: SkyParams::DUSK.sun_azimuth_deg,
            clouds: CloudCover::Clear,
            floor: Floor::default_for(false),
        }
    }

    /// The same room with the sun at azimuth `deg`, wrapped into 0..=360.
    /// Indoors there is no sun, so an indoor room is returned unchanged.
    #[must_use]
    pub fn with_sun_azimuth(self, deg: f32) -> Self {
        match self {
            Self::Indoor { .. } => self,
            Self::Outdoor {
                sun_elevation_deg,
                clouds,
                floor,
                ..
            } => Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg: wrap_azimuth(deg),
                clouds,
                floor,
            },
        }
    }

    /// The same room under `clouds`. Indoors there is no sky, so an indoor
    /// room is returned unchanged.
    #[must_use]
    pub fn with_clouds(self, clouds: CloudCover) -> Self {
        match self {
            Self::Indoor { .. } => self,
            Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                floor,
                ..
            } => Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                clouds,
                floor,
            },
        }
    }

    /// The same room on `floor`, or unchanged when this kind of venue does
    /// not offer it.
    #[must_use]
    pub fn with_floor(self, floor: Floor) -> Self {
        if !Floor::options(self.is_indoor()).contains(&floor) {
            return self;
        }
        match self {
            Self::Indoor { house_level, .. } => Self::Indoor { house_level, floor },
            Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                clouds,
                ..
            } => Self::Outdoor {
                sun_elevation_deg,
                sun_azimuth_deg,
                clouds,
                floor,
            },
        }
    }

    /// The floor, always one this kind of venue offers: a record that names
    /// another reads as the default.
    #[must_use]
    pub fn floor(self) -> Floor {
        let floor = match self {
            Self::Indoor { floor, .. } | Self::Outdoor { floor, .. } => floor,
        };
        if Floor::options(self.is_indoor()).contains(&floor) {
            floor
        } else {
            Floor::default_for(self.is_indoor())
        }
    }

    fn is_indoor(self) -> bool {
        matches!(self, Self::Indoor { .. })
    }

    /// The sky's cloud cover, or [`CloudCover::Clear`] indoors.
    #[must_use]
    pub fn clouds(self) -> CloudCover {
        match self {
            Self::Indoor { .. } => CloudCover::Clear,
            Self::Outdoor { clouds, .. } => clouds,
        }
    }

    /// How far up the house is, or zero outdoors. Always in 0..=1.
    #[must_use]
    pub fn house_level(self) -> f32 {
        match self {
            Self::Indoor { house_level, .. } => {
                if house_level.is_finite() {
                    house_level.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
            Self::Outdoor { .. } => 0.0,
        }
    }

    /// The sun's elevation, or zero indoors. Always in -90..=90.
    #[must_use]
    pub fn sun_elevation_deg(self) -> f32 {
        match self {
            Self::Indoor { .. } => 0.0,
            Self::Outdoor {
                sun_elevation_deg: deg,
                ..
            } => {
                if deg.is_finite() {
                    deg.clamp(-90.0, 90.0)
                } else {
                    0.0
                }
            }
        }
    }

    /// The sun's azimuth, always in 0..=360. Indoors there is no sun, and
    /// this is [`SkyParams::DUSK`]'s.
    #[must_use]
    pub fn sun_azimuth_deg(self) -> f32 {
        match self {
            Self::Indoor { .. } => SkyParams::DUSK.sun_azimuth_deg,
            Self::Outdoor {
                sun_azimuth_deg, ..
            } => wrap_azimuth(sun_azimuth_deg),
        }
    }

    /// `"indoor"` or `"outdoor"` — the spelling the record, the agent verb and
    /// the editor's mode selector all use.
    #[must_use]
    pub fn mode(self) -> &'static str {
        match self {
            Self::Indoor { .. } => "indoor",
            Self::Outdoor { .. } => "outdoor",
        }
    }

    /// The JSON one venue record's `environment` column holds.
    ///
    /// The record spelling and the wire spelling are the same string on
    /// purpose: an agent reading `luma.venue.environment()`, the editor's mode
    /// selector and the database column are all looking at one value, and a
    /// second encoding would be a second place for them to disagree.
    #[must_use]
    pub fn to_record(self) -> String {
        serde_json::to_string(&self).unwrap_or_else(|_| String::from("{}"))
    }
}

fn dusk_azimuth() -> f32 {
    SkyParams::DUSK.sun_azimuth_deg
}

/// Read an environment back out of a venue record.
///
/// **Total.** A column that is empty, truncated, written by an older build or
/// simply wrong reads as the default — indoor, house at full — because there
/// is no useful thing for a venue to do with "this room has no lighting model"
/// and a failed read here would take the whole venue down with it.
impl From<String> for VenueEnvironment {
    fn from(record: String) -> Self {
        serde_json::from_str(&record).unwrap_or_default()
    }
}

impl From<VenueEnvironment> for String {
    fn from(environment: VenueEnvironment) -> Self {
        environment.to_record()
    }
}

/// How hazy a venue is, and what its haze looks like.
///
/// **Venue truth**, the same tier as [`VenueEnvironment`]: it sits on the venue
/// record, it syncs with the venue, and every picture of that room — the editor
/// viewport, an agent's offscreen frame — is taken through it. A room either
/// has atmosphere in it or it does not, and that is a property of the room, not
/// of whoever is looking at it.
///
/// Deliberately **without `steps` and `resolution`**. Those two live on
/// [`HazeSettings`] and they are the march's cost, not the room's look: how
/// many samples a beam is integrated at, and at what fraction of the output
/// resolution. They belong to the machine drawing the frame — a laptop on
/// battery and a workstation should not have to disagree about what the venue
/// *is* in order to disagree about how expensive it may be — so they stay
/// local and unsynced. Resist adding them here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VenueHaze {
    /// Whether the participating-medium pass is evaluated for this room at all.
    pub enabled: bool,
    /// Mean density, independent of hazer fixtures. 0..=0.5.
    pub density: f32,
    /// Shared spatial appearance and movement.
    pub appearance: HazeAppearance,
}

impl Default for VenueHaze {
    /// The haze this app has always drawn: on, at 0.24, with the default
    /// appearance. A venue nobody has touched looks exactly as it did before
    /// haze became a venue property.
    fn default() -> Self {
        Self {
            enabled: true,
            density: 0.24,
            appearance: HazeAppearance::DEFAULT,
        }
    }
}

impl VenueHaze {
    /// Largest mean density a room may hold. Past this the march is fog rather
    /// than haze and a beam stops reading as a beam.
    pub const MAX_DENSITY: f32 = 0.5;

    /// Clamp authored or stored controls at the rendering boundary.
    ///
    /// Applied on the way out of [`From<String>`], so a row written by an older
    /// build, an agent or a hand-edited database cannot put the renderer in a
    /// state it has no answer for.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let default = Self::default();
        Self {
            enabled: self.enabled,
            density: if self.density.is_finite() {
                self.density.clamp(0.0, Self::MAX_DENSITY)
            } else {
                default.density
            },
            appearance: self.appearance.sanitized(),
        }
    }

    /// The JSON one venue record's `haze` column holds.
    ///
    /// The record spelling and the wire spelling are the same string, for the
    /// same reason as [`VenueEnvironment::to_record`]: the column, the agent
    /// verb and the editor's controls are all looking at one value, and a
    /// second encoding would be a second place for them to disagree.
    #[must_use]
    pub fn to_record(self) -> String {
        serde_json::to_string(&self).unwrap_or_else(|_| String::from("{}"))
    }
}

/// Read a venue's haze back out of its record.
///
/// **Total**, and sanitized. A column that is empty, truncated, written by an
/// older build or simply wrong reads as the default, because there is no useful
/// thing for a venue to do with "this room has no atmosphere model" and a
/// failed read here would take the whole venue down with it.
impl From<String> for VenueHaze {
    fn from(record: String) -> Self {
        serde_json::from_str::<Self>(&record)
            .unwrap_or_default()
            .sanitized()
    }
}

impl From<VenueHaze> for String {
    fn from(haze: VenueHaze) -> Self {
        haze.to_record()
    }
}

impl Environment {
    /// A black environment with no ambient contribution.
    pub const DARK: Self = Self {
        background: [0.0; 3],
        ambient_color: [1.0; 3],
        ambient_intensity: 0.0,
        probe: None,
    };

    /// The editor's neutral lit-stage environment.
    ///
    /// The ambient term is the **fill**, not the key: it multiplies albedo, and
    /// the staging props are near-black, so on its own it cannot make one
    /// legible. Measured on `stage-builder`, taking the ambient from 0.45 to
    /// 2.0 moved a vertical deck face from 5.3 to 5.7 mean luma out of 255.
    /// What lifts the room is [`DirectionalLight::EDITOR`]; this is what keeps
    /// the faces turned away from it from going to nothing.
    pub const EDITOR: Self = Self {
        // Linear form of the old sRGB `#191919` clear colour.
        background: [0.009_721_217; 3],
        ambient_color: [1.0; 3],
        ambient_intensity: 0.45,
        probe: None,
    };
}

/// Appearance of a shared procedural medium, independent of fixture output.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HazeAppearance {
    /// Density contrast: zero is uniform, one has pronounced clouds.
    pub cloudiness: f32,
    /// Characteristic cloud size in metres.
    pub cloud_size: f32,
    /// Strength of evolving deformation and fine wisps.
    pub turbulence: f32,
    /// Horizontal drift in metres per second.
    pub wind_speed: f32,
    /// World XY direction, degrees from +X toward +Y.
    pub wind_direction: f32,
}

impl Default for HazeAppearance {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl HazeAppearance {
    /// Subtle indoor haze with a gentle horizontal drift.
    pub const DEFAULT: Self = Self {
        cloudiness: 0.35,
        cloud_size: 4.0,
        turbulence: 0.3,
        wind_speed: 0.15,
        wind_direction: 30.0,
    };
    /// Clamp invalid authored controls at the rendering boundary.
    pub fn sanitized(self) -> Self {
        let default = Self::default();
        let bounded = |v: f32, fallback: f32, min: f32, max: f32| {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                fallback
            }
        };
        Self {
            cloudiness: bounded(self.cloudiness, default.cloudiness, 0.0, 1.0),
            cloud_size: bounded(self.cloud_size, default.cloud_size, 0.5, 20.0),
            turbulence: bounded(self.turbulence, default.turbulence, 0.0, 1.0),
            wind_speed: bounded(self.wind_speed, default.wind_speed, 0.0, 10.0),
            wind_direction: if self.wind_direction.is_finite() {
                self.wind_direction.rem_euclid(360.0)
            } else {
                default.wind_direction
            },
        }
    }
}

/// Serializable controls for the analytic fixture haze pass.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HazeSettings {
    /// Whether the participating-medium pass is evaluated.
    pub enabled: bool,
    /// Equiangular samples per beam.
    pub steps: u32,
    /// Render-target scale; one is native output resolution.
    pub resolution: f32,
    /// Mean density, independent of hazer fixtures.
    pub density: f32,
    /// Shared spatial appearance and movement.
    #[serde(default)]
    pub appearance: HazeAppearance,
}

/// One directional light. Direction points from the scene toward the light and
/// is normalized at the renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectionalLight {
    /// World-space direction from a shaded point toward the light.
    pub direction: [f32; 3],
    /// Linear RGB light colour.
    pub color: [f32; 3],
    /// Scalar illuminance used by the renderer's relative-lighting model.
    pub intensity: f32,
    /// Whether this light renders and samples the directional shadow map.
    pub shadows: bool,
    /// Radius of the deterministic shadow filter in shadow-map texels.
    /// Zero is a single hard comparison; values up to three widen the penumbra.
    #[serde(default = "default_shadow_softness")]
    pub shadow_softness: f32,
}

const fn default_shadow_softness() -> f32 {
    1.0
}

impl DirectionalLight {
    /// The neutral editor key light of the lit-stage preset.
    ///
    /// Bright, because it is doing the whole job. A stage is built out of black
    /// steel and black ply, and a key at show level leaves them **darker than
    /// the floor they stand on** — measured on `stage-builder` at intensity
    /// 1.4: deck 5.6, floor 9.1, out of 255. That is the wrong picture for a
    /// page whose subject is where a truss goes, and it is not an ambient
    /// problem ([`Environment::EDITOR`] carries that measurement). At this
    /// level the same deck reads 32.6 against a 44.8 floor and casts a shadow
    /// that says where it is standing.
    pub const EDITOR: Self = Self {
        // `world_from_three([8, 12, 6])` from the original renderer.
        direction: [8.0, -6.0, 12.0],
        color: [1.0; 3],
        intensity: 6.0,
        shadows: true,
        shadow_softness: 1.0,
    };
}

impl RenderSettings {
    /// Interactive dark-stage defaults. Haze stays independently enabled.
    #[must_use]
    pub const fn dark_stage(fov: f32, haze_resolution: f32) -> Self {
        Self {
            environment: Environment::DARK,
            haze: HazeSettings {
                appearance: HazeAppearance::DEFAULT,
                enabled: true,
                steps: 8,
                resolution: haze_resolution,
                density: 0.24,
            },
            sun: None,
            show_grid: false,
            show_floor: true,
            show_cables: true,
            show_gizmos: true,
            house: None,
            sky: None,
            debug_view: DebugView::Pbr,
            look: Look::NEUTRAL,
            fixture_surface_lighting: true,
            fixture_shadows: true,
            geometry_shadows: false,
            cluster_debug: false,
            probes: ProbeView::PLAIN,
            fov,
            quality: Quality::High,
            golden_shadow_eye: None,
        }
    }

    /// One object, lit so you can see it — the palette and library thumbnails.
    ///
    /// Not a room, and deliberately without a [`house`](Self::house): the
    /// subject is a piece on nothing, and hanging a house rig over it would
    /// light a ceiling that does not exist. A *room* is lit by its venue's own
    /// environment; see [`Self::room`].
    #[must_use]
    pub const fn object_lit(fov: f32, haze_resolution: f32) -> Self {
        Self {
            environment: Environment::EDITOR,
            haze: HazeSettings {
                appearance: HazeAppearance::DEFAULT,
                enabled: true,
                steps: 8,
                resolution: haze_resolution,
                density: 0.24,
            },
            sun: Some(DirectionalLight::EDITOR),
            show_grid: true,
            show_floor: true,
            show_cables: true,
            show_gizmos: true,
            house: None,
            sky: None,
            debug_view: DebugView::Pbr,
            look: Look::NEUTRAL,
            fixture_surface_lighting: true,
            fixture_shadows: true,
            geometry_shadows: false,
            cluster_debug: false,
            probes: ProbeView::PLAIN,
            fov,
            quality: Quality::High,
            golden_shadow_eye: None,
        }
    }

    /// A room, lit by its venue's environment.
    ///
    /// The one preset a venue is ever drawn under, offscreen and live alike.
    /// `environment` and `sun` come straight out of [`crate::house::fill`], so
    /// there is no second opinion about what an environment means; the frame
    /// builder reads [`Self::house`] back to hang the lamps once it knows the
    /// room's bounds.
    ///
    /// Indoors, the house downlights provide the direct light, with a small
    /// ambient approximation for room bounce. There is no directional key.
    ///
    /// `haze` is the room's own atmosphere, for the same reason `environment`
    /// is: both are venue truth, and a venue is only ever drawn under this
    /// preset. `haze_resolution` stays a separate argument because it is the
    /// caller's cost budget, not the venue's look — see [`VenueHaze`].
    #[must_use]
    pub fn room(
        environment: VenueEnvironment,
        haze: VenueHaze,
        fov: f32,
        haze_resolution: f32,
    ) -> Self {
        let fill = crate::house::fill(environment);
        let haze = haze.sanitized();
        Self {
            environment: fill.environment,
            haze: HazeSettings {
                appearance: haze.appearance,
                enabled: haze.enabled,
                steps: 8,
                resolution: haze_resolution,
                density: haze.density,
            },
            sun: fill.sun,
            show_grid: true,
            show_floor: true,
            show_cables: true,
            show_gizmos: true,
            house: Some(environment),
            sky: fill.sky,
            debug_view: DebugView::Pbr,
            look: Look::NEUTRAL,
            fixture_surface_lighting: true,
            fixture_shadows: true,
            geometry_shadows: false,
            cluster_debug: false,
            probes: ProbeView::default(),
            fov,
            quality: Quality::High,
            golden_shadow_eye: None,
        }
    }
}

/// The checked-in golden catalogue predates the independent environment
/// contract. Keep that compatibility at this file boundary; every caller and
/// every newly serialized descriptor sees only [`RenderSettings`].
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderSettingsWire {
    #[serde(default)]
    environment: Option<Environment>,
    #[serde(default)]
    haze: Option<HazeSettings>,
    #[serde(default)]
    sun: Option<DirectionalLight>,
    #[serde(default)]
    show_grid: Option<bool>,
    #[serde(default)]
    show_cables: Option<bool>,
    #[serde(default)]
    show_gizmos: Option<bool>,
    #[serde(default)]
    house: Option<VenueEnvironment>,
    #[serde(default)]
    sky: Option<SkyParams>,
    #[serde(default)]
    debug_view: DebugView,
    #[serde(default)]
    look: Option<Look>,
    #[serde(default)]
    fixture_surface_lighting: Option<bool>,
    #[serde(default)]
    fixture_shadows: Option<bool>,
    #[serde(default)]
    geometry_shadows: bool,
    #[serde(default)]
    cluster_debug: bool,
    fov: f32,
    #[serde(default)]
    quality: Quality,
    #[serde(default)]
    dark_stage: Option<bool>,
    #[serde(default)]
    volumetric_haze: Option<bool>,
    #[serde(default)]
    haze_steps: Option<u32>,
    #[serde(default)]
    haze_resolution: Option<f32>,
    #[serde(default)]
    haze_density: Option<f32>,
}

impl<'de> Deserialize<'de> for RenderSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RenderSettingsWire::deserialize(deserializer)?;
        if let Some(environment) = wire.environment {
            return Ok(Self {
                environment,
                haze: wire.haze.ok_or_else(|| D::Error::missing_field("haze"))?,
                sun: wire.sun,
                show_grid: wire.show_grid.unwrap_or(false),
                show_floor: true,
                // The tracked contract images predate rigging cables and are
                // about materials and beams, not about rooms. Absent means off
                // at this boundary, on in every constructor — the same split
                // `fixture_surface_lighting` is held at, for the same reason.
                show_cables: wire.show_cables.unwrap_or(false),
                // Same split, same reason: the compass is chrome, and the
                // tracked images predate it.
                show_gizmos: wire.show_gizmos.unwrap_or(false),
                // Absent means "not a room", exactly as it does in
                // `object_lit`. The tracked contract captures predate venue
                // environments and are about materials and beams; a house hung
                // over them would rewrite all nineteen.
                house: wire.house,
                // Absent means no atmosphere, which is what every tracked
                // contract image was captured under.
                sky: wire.sky,
                debug_view: wire.debug_view,
                // Absent means the camera every tracked image was captured
                // with.
                look: wire.look.unwrap_or(Look::NEUTRAL),
                // Absent means the constructors' default, which is on — only
                // the golden catalogue branch below pins it off, and that pin
                // has its own justification.
                fixture_surface_lighting: wire.fixture_surface_lighting.unwrap_or(true),
                fixture_shadows: wire.fixture_shadows.unwrap_or(true),
                geometry_shadows: wire.geometry_shadows,
                cluster_debug: wire.cluster_debug,
                probes: ProbeView::default(),
                fov: wire.fov,
                quality: wire.quality,
                golden_shadow_eye: None,
            });
        }

        let dark = wire
            .dark_stage
            .ok_or_else(|| D::Error::missing_field("environment"))?;
        let mut settings = if dark {
            Self::dark_stage(wire.fov, wire.haze_resolution.unwrap_or(1.0))
        } else {
            Self::object_lit(wire.fov, wire.haze_resolution.unwrap_or(1.0))
        };
        settings.haze.enabled = wire.volumetric_haze.unwrap_or(false) && dark;
        settings.haze.steps = wire.haze_steps.unwrap_or(8);
        settings.haze.density = wire.haze_density.unwrap_or(0.0);
        settings.debug_view = wire.debug_view;
        settings.look = wire.look.unwrap_or(Look::NEUTRAL);
        // The golden catalogue predates surface fixture lighting. Keeping it
        // off here preserves those captured inputs; every interactive preset
        // enables the path.
        settings.show_cables = wire.show_cables.unwrap_or(false);
        settings.show_gizmos = wire.show_gizmos.unwrap_or(false);
        settings.sky = wire.sky;
        settings.fixture_surface_lighting = wire.fixture_surface_lighting.unwrap_or(false);
        settings.fixture_shadows = wire.fixture_shadows.unwrap_or(false);
        settings.geometry_shadows = wire.geometry_shadows;
        settings.cluster_debug = wire.cluster_debug;
        settings.golden_shadow_eye = (!dark).then_some(DirectionalLight::EDITOR.direction);
        Ok(settings)
    }
}

/// Z-up data space; `pos[2]` is height, rotations are Euler XYZ radians.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fixture {
    /// Venue-unique id; primitive keys are `"<id>:<head>"`.
    pub id: String,
    /// Key into [`Catalogue::definitions`].
    pub fixture_path: String,
    /// Which `Mode` of the definition is patched.
    pub mode_name: String,
    /// Position.
    pub pos: [f32; 3],
    /// Euler XYZ rotation.
    pub rot: [f32; 3],
}

/// Where a piece's geometry comes from.
///
/// One field, not a path plus an optional override: a piece is authored art or
/// it is generated, and a schema that can say both at once needs a precedence
/// rule nobody can see. Serialized flat into [`Piece`], so an authored piece is
/// still `{"meshPath": "..."}` and every existing scene file reads unchanged.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Geometry {
    /// Path under `resources/meshes/`.
    MeshPath(String),
    /// A generated family and its parameters.
    Procedural(Procedural),
    /// Several authored meshes bolted together, named by the catalog id of the
    /// assembly that lists them. A *reference*, like `MeshPath`, rather than an
    /// inlined part list: the parts are catalog-fixed, not per-node, so
    /// copying them into every scene description would be a second copy to
    /// keep true.
    Assembly(String),
}

impl Geometry {
    /// An authored mesh at a path under `resources/meshes/`.
    pub fn mesh(path: impl Into<String>) -> Self {
        Self::MeshPath(path.into())
    }

    /// The parts of an assembly, or an empty slice for anything else.
    #[must_use]
    pub fn parts(&self) -> &'static [luma_scene::catalog::Part] {
        match self {
            Self::Assembly(id) => {
                luma_scene::catalog::piece(id).map_or(&[], |piece| piece.geometry.parts())
            }
            _ => &[],
        }
    }
}

/// The generated piece families. A closed vocabulary: a new set object is an
/// authored mesh unless its shape is genuinely parametric.
///
/// Every variant is a truss piece today, and every one of them mates with every
/// other — see [`crate::truss`]. Parameters here are *requests*: each is
/// quantized or clamped by the generator's constructor, so an unbuildable value
/// is not an error, it is the nearest buildable one.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Procedural {
    /// A continuous F34 lattice.
    Truss {
        /// Requested span in metres, before snapping to whole panels.
        span: f32,
    },
    /// A box corner, open on two to six faces.
    Corner {
        /// Which faces are open; fewer than two widens to straight-through.
        faces: crate::truss::FaceSet,
    },
    /// Two half-boxes on a vertical pin.
    Hinge {
        /// Deflection in degrees, before clamping to `0..=180` and rounding.
        angle: f32,
    },
}

impl Procedural {
    /// The generated piece this describes, with its parameters made buildable.
    ///
    /// One place converts a scene file's requests into geometry, so the frame
    /// builder, the camera framing, and any future collision box all see the
    /// same clamped piece rather than each re-deriving it.
    fn part(self) -> Part {
        match self {
            Self::Truss { span } => Part::Truss(crate::truss::Truss::new(span)),
            Self::Corner { faces } => Part::Corner(crate::truss::Corner::new(faces)),
            Self::Hinge { angle } => Part::Hinge(crate::truss::Hinge::new(angle)),
        }
    }

    /// Stable identity of this piece's geometry in the frame's mesh bank.
    ///
    /// Every parameter that changes a vertex is in the key, and every key is
    /// drawn from a finite set — spans are whole panels, corners are one of 64
    /// face sets, hinges are whole degrees — so a venue of generated pieces
    /// interns a bounded number of meshes.
    #[must_use]
    pub fn mesh_key(self) -> String {
        match self.part() {
            Part::Truss(t) => t.mesh_key(),
            Part::Corner(c) => c.mesh_key(),
            Part::Hinge(h) => h.mesh_key(),
        }
    }

    /// The piece as one uploadable triangle list, in piece-local space.
    #[must_use]
    pub fn mesh(self) -> crate::frame::MeshData {
        match self.part() {
            Part::Truss(t) => t.mesh(),
            Part::Corner(c) => c.mesh(),
            Part::Hinge(h) => h.mesh(),
        }
    }

    /// Every face another piece can bolt onto, in piece-local space.
    #[must_use]
    pub fn end_frames(self) -> Vec<crate::truss::EndFrame> {
        match self.part() {
            Part::Truss(t) => t.end_frames().to_vec(),
            Part::Corner(c) => c.end_frames().collect(),
            Part::Hinge(h) => h.end_frames().to_vec(),
        }
    }

    /// Half-width of the box [`Scene::framing`] stands on this piece's origin.
    #[must_use]
    pub fn half_extent_m(self) -> f32 {
        match self.part() {
            Part::Truss(t) => t.span_m() / 2.0,
            // A block is the same size whatever its ways, and a hinge is one
            // block long in the worst case.
            Part::Corner(_) | Part::Hinge(_) => crate::truss::OUTER_M,
        }
    }
}

/// A [`Procedural`]'s parameters, made buildable. Private: the vocabulary the
/// wire and the callers share is [`Procedural`], and a second public enum over
/// the same three shapes would be a list to keep in sync.
enum Part {
    Truss(crate::truss::Truss),
    Corner(crate::truss::Corner),
    Hinge(crate::truss::Hinge),
}

/// A stage piece, in the same Z-up data space as [`Fixture`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Piece {
    /// Venue-unique id.
    pub id: String,
    /// Authored mesh or generated family.
    #[serde(flatten)]
    pub geometry: Geometry,
    /// The venue's own vocabulary for what this is: `truss`, `stand`, `floor`,
    /// `guardrail`, `speaker`, `cdj`, `mixer`. Read only by
    /// [`Piece::is_rig_bearing`]; the renderer draws every piece the same way.
    /// Absent in the golden catalogue, which predates it.
    #[serde(default)]
    pub kind: String,
    /// Position.
    pub pos: [f32; 3],
    /// Euler XYZ rotation.
    pub rot: [f32; 3],
    /// Uniform scale.
    pub scale: f32,
}

impl Piece {
    /// Whether this piece is part of the *rig* — something fixtures hang on —
    /// rather than part of the room around it.
    ///
    /// This selects the room bounds used for house lighting. Camera framing
    /// includes every authored piece, so decks and speakers stay visible too.
    /// An unknown kind — including the empty one the goldens carry — is room.
    #[must_use]
    pub fn is_rig_bearing(&self) -> bool {
        matches!(self.kind.as_str(), "truss" | "stand")
    }

    /// Whether this piece is floor the rig stands on — a deck — rather than
    /// something on it. The stage camera pivots on the rig before a floor.
    #[must_use]
    pub fn is_floor(&self) -> bool {
        self.kind == "floor"
    }

    /// Half-width of the box [`Scene::framing`] stands on this piece's origin.
    ///
    /// An authored piece's real size is in its mesh, which is loaded
    /// asynchronously and is not part of a scene description, so its uniform
    /// scale stands in. A generated one has no such excuse — its span *is* the
    /// parameter — and a twelve-metre truss framed as a one-metre box is the
    /// whole rig off-screen.
    #[must_use]
    pub fn framing_half_extent(&self) -> f32 {
        match &self.geometry {
            Geometry::MeshPath(_) => self.scale.abs().max(0.25),
            Geometry::Procedural(p) => self.scale.abs() * p.half_extent_m(),
            // An assembly's parts are catalog-fixed, so unlike a lone mesh its
            // real size is knowable without loading anything: the widest part
            // offset plus a part's own reach. A booth framed as a 0.25 m box
            // would sit half out of shot.
            Geometry::Assembly(_) => {
                self.scale.abs() * crate::catalog::assembly_half_extent(self.geometry.parts())
            }
        }
    }
}

/// One head's evaluated state. Mirrors the eval engine's `PrimitiveState`
/// minus `speed`, which only gates mesh articulation the goldens freeze.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct PrimitiveState {
    /// 0..1 intensity.
    pub dimmer: f32,
    /// Linear RGB.
    pub color: [f32; 3],
    /// 0..1 rate, not a boolean; the display clock turns it into on/off.
    pub strobe: f32,
    /// `[pan_deg, tilt_deg]`.
    pub position: [f32; 2],
    /// Procedural gobo selector: 0 open, 1 radial spokes, 2 breakup grid.
    #[serde(default)]
    pub gobo: u32,
    /// Gobo rotation in radians around the beam axis.
    #[serde(default)]
    pub gobo_rotation: f32,
}

/// The QLC+ `.qxf` subset the renderer reads.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Definition {
    /// QLC+ fixture type; selects the mesh and the fallback cone.
    #[serde(rename = "Type")]
    pub kind: String,
    /// Patchable modes; the head count comes from the patched one.
    #[serde(rename = "Mode")]
    pub modes: Vec<Mode>,
    /// Physical block: dimensions, pixel layout, lens.
    #[serde(rename = "Physical")]
    pub physical: Option<Physical>,
}

/// A patchable mode. Only its head count matters here — the visualizer patches
/// DMX, not pixels.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Mode {
    /// Mode name, matched against `Fixture::mode_name`.
    #[serde(rename = "@Name")]
    pub name: String,
    /// Heads, kept opaque: only the length is read.
    #[serde(rename = "Head", default)]
    pub heads: Vec<serde_json::Value>,
}

/// QLC+ `Physical`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Physical {
    /// Housing size.
    #[serde(rename = "Dimensions")]
    pub dimensions: Option<Dimensions>,
    /// Pixel grid, for procedural bars and matrices.
    #[serde(rename = "Layout")]
    pub layout: Option<Layout>,
    /// Lens block — the source of cone geometry.
    #[serde(rename = "Lens")]
    pub lens: Option<Lens>,
}

/// Millimetres, as QLC+ writes them.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Dimensions {
    /// Width, mm.
    #[serde(rename = "@Width")]
    pub width: f32,
    /// Height, mm.
    #[serde(rename = "@Height")]
    pub height: f32,
    /// Depth, mm.
    #[serde(rename = "@Depth")]
    pub depth: f32,
}

/// Pixel grid of a procedural fixture.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Layout {
    /// Pixels per row.
    #[serde(rename = "@Width")]
    pub width: u32,
    /// Rows.
    #[serde(rename = "@Height")]
    pub height: u32,
}

/// Lens block. A fixed lens repeats one value; a zoom lens gives a range and,
/// with no zoom channel in the state model, the renderer sits at mid-zoom.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Lens {
    /// Narrow end of the beam angle, degrees. Zero means "unknown".
    #[serde(rename = "@DegreesMin")]
    pub degrees_min: f32,
    /// Wide end of the beam angle, degrees. Zero means "unknown".
    #[serde(rename = "@DegreesMax")]
    pub degrees_max: f32,
    /// Measured front-lens radius in metres, from a spec sheet. QLC+ has no
    /// such attribute; when present it wins over the per-class stand-in in
    /// [`crate::luminaire::lens_for`].
    #[serde(rename = "@RadiusM", default, skip_serializing_if = "Option::is_none")]
    pub radius_m: Option<f32>,
    /// Measured distance in metres from the head's tilt pivot forward along
    /// the beam axis to the lens centre (GDTF's `Beam` position). When present
    /// it wins over the head mesh's front face in `frame::lens_centre`.
    #[serde(rename = "@OffsetM", default, skip_serializing_if = "Option::is_none")]
    pub offset_m: Option<f32>,
}

/// Versioned, self-describing inputs for one renderer golden PNG.
///
/// The descriptor deliberately contains only the fixture definitions referenced
/// by the scene. Paths remain part of the contract for mesh assets, while every
/// numeric render, camera, fixture and primitive-state input is recorded inline.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoldenFrameDescriptor<'a> {
    /// Schema identifier for consumers that compare captures across revisions.
    pub schema: &'static str,
    /// PNG filename written beside this descriptor.
    pub image: String,
    /// Source viewport in CSS pixels.
    pub viewport: &'a Viewport,
    /// Device scale applied to the source viewport.
    pub device_scale_factor: f32,
    /// Physical output dimensions in pixels.
    pub output_size: [u32; 2],
    /// Deterministic temporal samples accumulated into the output.
    pub subframes: u32,
    /// Absolute scene clock evaluated for this output, in seconds.
    pub time_seconds: f32,
    /// Complete closed scene, camera, environment and primitive-state contract.
    pub scene: &'a Scene,
    /// Renderer-relevant fixture definitions used to resolve scene fixtures.
    pub definitions: BTreeMap<&'a str, &'a Definition>,
}

impl Catalogue {
    /// # Errors
    /// Fails if the file is missing or is not the catalogue shape.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Pixel dimensions of a captured frame: CSS viewport times the capture's
    /// device scale factor.
    #[must_use]
    pub fn frame_size(&self) -> (u32, u32) {
        let s = self.device_scale_factor;
        (
            (self.viewport.width as f32 * s).round() as u32,
            (self.viewport.height as f32 * s).round() as u32,
        )
    }

    /// Build the deterministic input descriptor written beside a golden frame.
    ///
    /// # Errors
    /// Fails when a fixture refers to a definition absent from this catalogue.
    pub fn frame_descriptor<'a>(
        &'a self,
        scene: &'a Scene,
        time_seconds: f32,
        subframes: u32,
    ) -> anyhow::Result<GoldenFrameDescriptor<'a>> {
        let mut definitions = BTreeMap::new();
        for fixture in &scene.fixtures {
            let definition = self.definitions.get(&fixture.fixture_path).ok_or_else(|| {
                anyhow::anyhow!(
                    "scene {:?} references missing fixture definition {:?}",
                    scene.id,
                    fixture.fixture_path
                )
            })?;
            definitions.insert(fixture.fixture_path.as_str(), definition);
        }
        let (width, height) = self.frame_size();
        Ok(GoldenFrameDescriptor {
            schema: "luma.renderer-frame/1",
            image: scene.frame_name(time_seconds),
            viewport: &self.viewport,
            device_scale_factor: self.device_scale_factor,
            output_size: [width, height],
            subframes,
            time_seconds,
            scene,
            definitions,
        })
    }
}

impl Scene {
    /// What a camera looking at this scene has to fit, in world space.
    ///
    /// The rule lives in [`luma_scene::Framing`]; this is the one place the
    /// scene's data-space geometry is brought into world space for it, so the
    /// desktop viewport and an offscreen `render(view=…)` frame the same rig
    /// the same way.
    ///
    /// Every fixture contributes a [`luma_scene::Beam`] — the head and where
    /// its light goes at the state this scene pins. The definitions are what
    /// says *which way* that is (see [`beam_direction`](crate::luminaire::beam_direction)):
    /// a pixel bar fires along its own length and a hazer not at all, and a
    /// framing that assumed every fixture was a mover put a club's extent six
    /// metres wider than its rig. Every authored piece contributes its bounds:
    /// construction inspection must also show decks, speakers and guardrails.
    /// The renderer's unbounded ground plane is not an authored piece.
    ///
    /// Generated pieces use their actual local bounds transformed by the
    /// same pose as the renderer. Meshes retain an approximate standing box:
    /// their assets load asynchronously and are not in a scene description.
    #[must_use]
    pub fn framing(&self, definitions: &BTreeMap<String, Definition>) -> luma_scene::Framing {
        luma_scene::Framing::of(
            self.fixtures.iter().map(|f| luma_scene::Beam {
                origin: crate::coords::world_from_data(glam::Vec3::from(f.pos)),
                direction: crate::luminaire::beam_direction(
                    definitions.get(&f.fixture_path),
                    f.rot,
                    self.primitive(&f.id, 0).map(|s| s.position),
                ),
            }),
            self.pieces.iter().map(piece_box),
        )
    }

    /// The room a house rig hangs over, in world space.
    ///
    /// Every head and every rig-bearing piece, and nothing else: unlike
    /// [`Self::framing`] this does not follow the beams out, because where the
    /// light *goes* is not where the room is. A venue with nothing in it
    /// returns [`luma_scene::Aabb::EMPTY`]; [`crate::house::lamps`] turns that
    /// into a minimum room rather than refusing, since an empty venue is
    /// exactly the one an operator is about to start building in.
    #[must_use]
    pub fn room_bounds(&self) -> luma_scene::Aabb {
        let mut bounds = luma_scene::Aabb::from_points(
            self.fixtures
                .iter()
                .map(|f| crate::coords::world_from_data(glam::Vec3::from(f.pos))),
        );
        for piece in self.pieces.iter().filter(|p| p.is_rig_bearing()) {
            bounds.union(&piece_box(piece));
        }
        bounds
    }

    /// `<id>-<t>.png`, matching `harness/shot-visualizer.mjs`'s `stamp`.
    #[must_use]
    pub fn frame_name(&self, t: f32) -> String {
        format!("{}-{t:.3}.png", self.id)
    }

    /// `<id>-<t>.json`, paired with [`Self::frame_name`].
    #[must_use]
    pub fn descriptor_name(&self, t: f32) -> String {
        format!("{}-{t:.3}.json", self.id)
    }

    /// The pinned state of one head, if the scene declares it.
    #[must_use]
    pub fn primitive(&self, fixture_id: &str, head: usize) -> Option<PrimitiveState> {
        self.state.get(&format!("{fixture_id}:{head}")).copied()
    }
}

/// Generated geometry uses its measured local envelope and renderer pose.
/// Mesh-only scenes keep a standing-box estimate until their assets load.
/// Shared by camera framing and house-lighting bounds.
fn piece_box(piece: &Piece) -> luma_scene::Aabb {
    if let Geometry::Procedural(params) = piece.geometry {
        let local = crate::catalog::procedural_bounds(params);
        let transform = glam::Mat4::from_mat3(crate::coords::three_to_world_basis())
            * crate::coords::three_pose_from_data(piece.pos, piece.rot)
            * glam::Mat4::from_scale(glam::Vec3::splat(piece.scale));
        return luma_scene::Aabb::from_points(
            luma_scene::Aabb::new(local.min.as_vec3(), local.max.as_vec3())
                .corners()
                .map(|point| transform.transform_point3(point)),
        );
    }
    let base = crate::coords::world_from_data(glam::Vec3::from(piece.pos));
    let half = piece.framing_half_extent();
    luma_scene::Aabb::new(
        base - glam::Vec3::new(half, half, 0.0),
        base + glam::Vec3::splat(half),
    )
}

impl Definition {
    /// Heads in the named mode, or zero when the mode is absent.
    #[must_use]
    pub fn head_count(&self, mode_name: &str) -> usize {
        self.modes
            .iter()
            .find(|m| m.name == mode_name)
            .map_or(0, |m| m.heads.len())
    }

    /// Physical size in metres, defaulting each missing axis to 300 mm the way
    /// `extractPhysicalDimensions` does.
    #[must_use]
    pub fn dimensions_m(&self) -> [f32; 3] {
        let d = self.physical.as_ref().and_then(|p| p.dimensions.as_ref());
        let axis = |v: f32| if v > 0.0 { v / 1000.0 } else { 0.3 };
        d.map_or([0.3; 3], |d| [axis(d.width), axis(d.height), axis(d.depth)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_azimuth_round_trips_through_the_environment_record() {
        let environment = VenueEnvironment::outdoor(12.0).with_sun_azimuth(95.0);
        assert_eq!(VenueEnvironment::from(environment.to_record()), environment);
        assert_eq!(environment.sun_azimuth_deg(), 95.0);
        assert_eq!(environment.sun_elevation_deg(), 12.0);
    }

    #[test]
    fn clouds_round_trip_and_a_clear_sky_writes_no_key() {
        let clear = VenueEnvironment::outdoor(12.0).with_sun_azimuth(95.0);
        assert!(
            !clear.to_record().contains("clouds"),
            "{}",
            clear.to_record()
        );
        for clouds in CloudCover::ALL {
            let environment = clear.with_clouds(clouds);
            assert_eq!(VenueEnvironment::from(environment.to_record()), environment);
            assert_eq!(environment.clouds(), clouds);
        }
        // Changing the sun keeps the weather.
        let storm = clear.with_clouds(CloudCover::Storm);
        assert_eq!(storm.with_sun_azimuth(10.0).clouds(), CloudCover::Storm);
        // Indoors there is no sky to cloud.
        let indoor = VenueEnvironment::indoor(0.5);
        assert_eq!(indoor.with_clouds(CloudCover::Storm), indoor);
        assert_eq!(indoor.clouds(), CloudCover::Clear);
    }

    #[test]
    fn floors_round_trip_per_kind_of_venue_and_the_default_writes_no_key() {
        for indoor in [true, false] {
            let room = if indoor {
                VenueEnvironment::indoor(0.5)
            } else {
                VenueEnvironment::outdoor(12.0).with_clouds(CloudCover::Storm)
            };
            assert_eq!(room.floor(), Floor::default_for(indoor));
            assert!(!room.to_record().contains("floor"), "{}", room.to_record());
            for &floor in Floor::options(indoor) {
                let chosen = room.with_floor(floor);
                assert_eq!(VenueEnvironment::from(chosen.to_record()), chosen);
                assert_eq!(chosen.floor(), floor);
            }
        }
        // A room offers no grass, and an open-air venue no carpet.
        let indoor = VenueEnvironment::indoor(0.5);
        assert_eq!(indoor.with_floor(Floor::Grass), indoor);
        let outdoor = VenueEnvironment::outdoor(12.0).with_floor(Floor::Gravel);
        assert_eq!(outdoor.with_floor(Floor::Carpet), outdoor);
        // Moving the sun and changing the weather keep the ground.
        let moved = outdoor
            .with_sun_azimuth(10.0)
            .with_clouds(CloudCover::Storm);
        assert_eq!(moved.floor(), Floor::Gravel);
        // Records from before floors existed read as the default.
        let old = VenueEnvironment::from(String::from(r#"{"mode":"indoor","houseLevel":0.3}"#));
        assert_eq!(old.floor(), Floor::BlackStage);
    }

    #[test]
    fn a_record_without_an_azimuth_reads_the_dusk_default() {
        let old = String::from(r#"{"mode":"outdoor","sunElevationDeg":20.0}"#);
        let environment = VenueEnvironment::from(old);
        assert_eq!(environment.sun_elevation_deg(), 20.0);
        assert_eq!(
            environment.sun_azimuth_deg(),
            SkyParams::DUSK.sun_azimuth_deg
        );
    }

    #[test]
    fn sun_azimuth_wraps_into_a_full_turn() {
        let at = |deg| {
            VenueEnvironment::outdoor(10.0)
                .with_sun_azimuth(deg)
                .sun_azimuth_deg()
        };
        assert_eq!(at(0.0), 0.0);
        assert_eq!(at(360.0), 360.0, "the top of the slider stays at the top");
        assert_eq!(at(370.0), 10.0);
        assert_eq!(at(-30.0), 330.0);
        assert_eq!(at(f32::NAN), SkyParams::DUSK.sun_azimuth_deg);
        // Stored past the range by another writer, read back in range.
        let stored = VenueEnvironment::Outdoor {
            sun_elevation_deg: 10.0,
            sun_azimuth_deg: 725.0,
            clouds: CloudCover::Clear,
            floor: Floor::Concrete,
        };
        assert_eq!(stored.sun_azimuth_deg(), 5.0);
        // Indoors there is no sun to turn.
        let indoor = VenueEnvironment::indoor(0.5);
        assert_eq!(indoor.with_sun_azimuth(10.0), indoor);
    }

    #[test]
    fn venue_haze_round_trips_through_its_record() {
        let haze = VenueHaze {
            enabled: false,
            density: 0.42,
            appearance: HazeAppearance {
                cloudiness: 0.6,
                cloud_size: 7.5,
                turbulence: 0.2,
                wind_speed: 1.25,
                wind_direction: 210.0,
            },
        };
        assert_eq!(VenueHaze::from(haze.to_record()), haze);
    }

    #[test]
    fn a_malformed_haze_record_reads_as_the_default() {
        for record in ["", "{", "null", "[]", "not json at all"] {
            assert_eq!(
                VenueHaze::from(record.to_string()),
                VenueHaze::default(),
                "{record:?} should have decoded to the default"
            );
        }
    }

    #[test]
    fn an_out_of_range_haze_record_is_clamped_on_the_way_in() {
        let record = r#"{"enabled":true,"density":9.0,"appearance":{"cloudiness":-1.0,"cloudSize":900.0,"turbulence":5.0,"windSpeed":-3.0,"windDirection":400.0}}"#;
        let haze = VenueHaze::from(record.to_string());
        assert_eq!(haze.density, VenueHaze::MAX_DENSITY);
        assert_eq!(haze.appearance, haze.appearance.sanitized());
    }

    #[test]
    fn a_room_is_drawn_through_its_venues_haze() {
        let haze = VenueHaze {
            enabled: true,
            density: 0.11,
            appearance: HazeAppearance::DEFAULT,
        };
        let render = RenderSettings::room(VenueEnvironment::default(), haze, 50.0, 0.5);
        assert_eq!(render.haze.density, 0.11);
        assert!(render.haze.enabled);
        // The cost knobs are the caller's, not the venue's.
        assert_eq!(render.haze.resolution, 0.5);
        assert_eq!(render.haze.steps, 8);
    }

    /// The catalogue the test rig is patched from: one moving head.
    fn definitions() -> BTreeMap<String, Definition> {
        BTreeMap::from([(
            "Luma/Mover.qxf".to_string(),
            Definition {
                kind: "Moving Head".into(),
                modes: Vec::new(),
                physical: None,
            },
        )])
    }

    /// A rig of four movers over a deck, the shape every visualizer test uses.
    fn scene() -> Scene {
        Scene {
            id: "framing".into(),
            times: Vec::new(),
            camera: CameraPose {
                position: [0.0; 3],
                target: [0.0; 3],
            },
            editing: false,
            aim_arrows: false,
            render: RenderSettings::dark_stage(50.0, 1.0),
            selected_fixture_ids: Vec::new(),
            editor: Editor::default(),
            fixtures: (0..4)
                .map(|i| Fixture {
                    id: format!("fixture-{i}"),
                    fixture_path: "Luma/Mover.qxf".into(),
                    mode_name: "Default".into(),
                    pos: [i as f32 * 0.8 - 1.2, 0.0, 3.0],
                    rot: [0.0; 3],
                })
                .collect(),
            pieces: vec![
                Piece {
                    id: "deck".into(),
                    geometry: Geometry::mesh("stage_lab/deck.glb"),
                    kind: "floor".into(),
                    pos: [0.0; 3],
                    rot: [0.0; 3],
                    scale: 1.0,
                },
                // Six metres out and irrelevant to the picture, the shape a
                // real venue's guardrails and speakers have.
                Piece {
                    id: "rail".into(),
                    geometry: Geometry::mesh("stage_lab/guardrail.glb"),
                    kind: "guardrail".into(),
                    pos: [6.0, 6.0, 0.0],
                    rot: [0.0; 3],
                    scale: 1.0,
                },
            ],
            state: BTreeMap::new(),
        }
    }

    /// Four movers with no pinned state hang at `z = 3` pointing down, so the
    /// framed box is the rig *and its pools* — floor to head — without a deck
    /// having to supply the bottom.
    #[test]
    fn beams_carry_the_box_down_to_the_floor() {
        let framing = scene().framing(&definitions());
        assert!(framing.floor_z().abs() < 1e-5, "{}", framing.floor_z());
        assert!((framing.bounds().min.z).abs() < 1e-5);
        let top = 3.0 + luma_scene::Framing::HEAD_RADIUS;
        assert!((framing.bounds().max.z - top).abs() < 1e-5, "{framing:?}");
    }

    /// `meshPath` is flattened, not nested: every scene file written before
    /// [`Geometry`] existed still reads, and a procedural piece is the same
    /// object with a different key.
    #[test]
    fn geometry_is_flat_on_the_wire() {
        let authored: Piece = serde_json::from_str(
            r#"{"id":"t","meshPath":"stage_lab/x.glb","kind":"truss",
                "pos":[0,0,3],"rot":[0,0,0],"scale":1}"#,
        )
        .expect("legacy piece still parses");
        assert!(matches!(&authored.geometry, Geometry::MeshPath(p) if p == "stage_lab/x.glb"));
        let json = serde_json::to_value(&authored).expect("piece serializes");
        assert_eq!(json["meshPath"], "stage_lab/x.glb");
        assert!(json.get("geometry").is_none());

        let generated: Piece = serde_json::from_str(
            r#"{"id":"t","procedural":{"truss":{"span":3.0}},"kind":"truss",
                "pos":[0,0,3],"rot":[0,0,0],"scale":1}"#,
        )
        .expect("procedural piece parses");
        assert!(matches!(
            generated.geometry,
            Geometry::Procedural(Procedural::Truss { span }) if (span - 3.0).abs() < 1e-6
        ));
        assert_eq!(
            serde_json::to_value(&generated).expect("piece serializes")["procedural"]["truss"]
                ["span"],
            3.0
        );
    }

    /// A generated piece knows its own size, so the camera does not have to
    /// guess it from a uniform scale that means nothing to it.
    #[test]
    fn a_procedural_truss_is_framed_at_its_own_span() {
        let piece = |geometry| Piece {
            id: "t".into(),
            geometry,
            kind: "truss".into(),
            pos: [0.0, 0.0, 3.0],
            rot: [0.0; 3],
            scale: 1.0,
        };
        assert!((piece(Geometry::mesh("x.glb")).framing_half_extent() - 1.0).abs() < 1e-6);
        let generated = piece(Geometry::Procedural(Procedural::Truss { span: 12.3 }));
        assert!((generated.framing_half_extent() - 6.25).abs() < 1e-6);
    }

    #[test]
    fn authored_room_pieces_are_in_the_camera_frame() {
        let extent = scene().framing(&definitions()).bounds();
        assert!(extent.max.x >= 7.0, "{extent:?}");
        assert!(extent.min.y <= -7.0, "{extent:?}");
    }

    /// The gauntlet's 10x6 stage extends downstage of its overhead row.
    /// A fit of just the heads and cubic truss estimates cropped that edge
    /// while targeting empty space several metres above the actual hardware.
    #[test]
    fn front_frames_the_stage_foreground_below_a_horizontal_row() {
        let mut scene = scene();
        scene.fixtures.clear();
        scene.pieces.clear();
        for u in [-4.0, -2.0, 0.0, 2.0, 4.0] {
            for v in [-2.5, -1.5, -0.5, 0.5, 1.5, 2.5] {
                scene.pieces.push(Piece {
                    id: format!("deck-{u}-{v}"),
                    geometry: Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                    kind: "floor".into(),
                    pos: [u, v, 0.0],
                    rot: [0.0; 3],
                    scale: 1.0,
                });
            }
        }
        scene.pieces.push(Piece {
            id: "overhead".into(),
            geometry: Geometry::Procedural(Procedural::Truss { span: 10.0 }),
            kind: "truss".into(),
            pos: [0.0, -3.0, 5.17],
            rot: [0.0; 3],
            scale: 1.0,
        });
        let framing = scene.framing(&definitions());
        assert!(framing.bounds().max.z < 5.35, "{framing:?}");
        for aspect in [1.0, 16.0 / 9.0, 9.0 / 16.0] {
            let finder = luma_scene::Viewfinder::new(50.0, aspect);
            let camera =
                luma_scene::Camera::for_view(luma_scene::View::Front, &framing, None, &finder);
            for x in [-5.0, 5.0] {
                for y in [-3.0, 3.0] {
                    for z in [0.0, 1.01] {
                        let point = glam::Vec3::new(x, y, z);
                        let ndc = camera.project(point, aspect);
                        assert!(
                            ndc.x.abs() < 1.0 && ndc.y.abs() < 1.0 && (0.0..=1.0).contains(&ndc.z),
                            "stage corner {point:?} cropped at aspect {aspect}: {ndc:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn generated_truss_bounds_follow_length_rotation_and_scale() {
        let mut piece = Piece {
            id: "beam".into(),
            geometry: Geometry::Procedural(Procedural::Truss { span: 10.0 }),
            kind: "truss".into(),
            pos: [0.0, -3.0, 5.17],
            rot: [0.0; 3],
            scale: 1.0,
        };
        let horizontal = piece_box(&piece);
        assert!((horizontal.size().x - 10.0).abs() < 0.01, "{horizontal:?}");
        assert!((horizontal.max.z - 5.34).abs() < 0.01, "{horizontal:?}");
        assert!((horizontal.min.z - 5.0).abs() < 0.01, "{horizontal:?}");
        piece.rot[1] = std::f32::consts::FRAC_PI_2;
        piece.scale = 0.5;
        let vertical = piece_box(&piece);
        assert!((vertical.size().z - 5.0).abs() < 0.01, "{vertical:?}");
        assert!(
            vertical.size().x < 0.2 && vertical.size().y < 0.2,
            "{vertical:?}"
        );
    }
}
