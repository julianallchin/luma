//! The golden-scene catalogue, as the three.js capture defined it.
//!
//! `src/harness/golden-scenes.ts` is the single description of what the eight
//! golden frames contain. `tools/dump-golden-scenes.ts` serialises that module
//! to `goldens/scenes.json`; this is its deserialiser. Nothing here is
//! transcribed by hand — when a scene changes, regenerate and both renderers
//! move together.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// The whole golden-scene catalogue, as `dump-golden-scenes.ts` writes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalogue {
    /// Jitter subframes the capture settled through before shooting. The
    /// three.js side ran a temporal EMA; we average this many subframes
    /// instead (spec §6) — same jitter primitive, deterministic.
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
#[derive(Debug, Deserialize)]
pub struct Viewport {
    /// Width in CSS pixels.
    pub width: u32,
    /// Height in CSS pixels.
    pub height: u32,
}

/// One golden scene: a complete description of a frame's inputs.
#[derive(Debug, Deserialize)]
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
    /// Every render dial the frame depends on, pinned.
    pub render: RenderSettings,
    /// Drives the gizmo and the selection outline.
    pub selected_fixture_ids: Vec<String>,
    /// Patched fixtures, in submission order.
    pub fixtures: Vec<Fixture>,
    /// Stage pieces (decks, trusses, speakers).
    pub pieces: Vec<Piece>,
    /// Fixed primitive state per `"<fixtureId>:<head>"` key.
    pub state: BTreeMap<String, PrimitiveState>,
}

/// Three.js Y-up, because that is the space `useCameraStore` holds.
#[derive(Debug, Deserialize)]
pub struct CameraPose {
    /// Eye position.
    pub position: [f32; 3],
    /// Look-at point.
    pub target: [f32; 3],
}

/// The subset of `use-render-settings-store.ts` the renderer reads. `bloom` and
/// `maxDpr` are deliberately absent: bloom is dropped (spec §2.5) and DPR is the
/// capture's business, not the renderer's.
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
    /// Renderer diagnostic output. `Pbr` is the authored display path.
    pub debug_view: DebugView,
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// Original positional anchor from the legacy directional-light capture.
    /// Skipped by the new contract; only the golden adapter populates it so
    /// its orthographic shadow projection remains byte-exact.
    #[serde(skip)]
    pub(crate) legacy_shadow_eye: Option<[f32; 3]>,
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

/// Scene-wide background and ambient contribution, in linear RGB.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    /// Linear RGB clear colour behind the scene.
    pub background: [f32; 3],
    /// Linear RGB ambient-light colour.
    pub ambient_color: [f32; 3],
    /// Scalar ambient strength. Zero disables ambient light without changing
    /// the background.
    pub ambient_intensity: f32,
}

impl Environment {
    /// A black environment with no ambient contribution.
    pub const DARK: Self = Self {
        background: [0.0; 3],
        ambient_color: [1.0; 3],
        ambient_intensity: 0.0,
    };

    /// The legacy editor's neutral lit-stage environment.
    pub const EDITOR: Self = Self {
        // Linear form of the old sRGB `#191919` clear colour.
        background: [0.009_721_217; 3],
        ambient_color: [1.0; 3],
        ambient_intensity: 0.2,
    };
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
    /// Nominal density before hazer-fixture scaling.
    pub density: f32,
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
}

impl DirectionalLight {
    /// The neutral editor key light used by the legacy lit-stage preset.
    pub const EDITOR: Self = Self {
        // `world_from_three([8, 12, 6])` from the original renderer.
        direction: [8.0, -6.0, 12.0],
        color: [1.0; 3],
        intensity: 1.4,
        shadows: true,
    };
}

impl RenderSettings {
    /// Interactive dark-stage defaults. Haze stays independently enabled.
    #[must_use]
    pub const fn dark_stage(fov: f32, haze_resolution: f32) -> Self {
        Self {
            environment: Environment::DARK,
            haze: HazeSettings {
                enabled: true,
                steps: 8,
                resolution: haze_resolution,
                density: 0.8,
            },
            sun: None,
            show_grid: false,
            debug_view: DebugView::Pbr,
            fov,
            legacy_shadow_eye: None,
        }
    }

    /// Interactive editor-light defaults.
    #[must_use]
    pub const fn editor_lit(fov: f32, haze_resolution: f32) -> Self {
        Self {
            environment: Environment::EDITOR,
            haze: HazeSettings {
                enabled: true,
                steps: 8,
                resolution: haze_resolution,
                density: 0.8,
            },
            sun: Some(DirectionalLight::EDITOR),
            show_grid: true,
            debug_view: DebugView::Pbr,
            fov,
            legacy_shadow_eye: None,
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
    debug_view: DebugView,
    fov: f32,
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
                debug_view: wire.debug_view,
                fov: wire.fov,
                legacy_shadow_eye: None,
            });
        }

        let dark = wire
            .dark_stage
            .ok_or_else(|| D::Error::missing_field("environment"))?;
        let mut settings = if dark {
            Self::dark_stage(wire.fov, wire.haze_resolution.unwrap_or(1.0))
        } else {
            Self::editor_lit(wire.fov, wire.haze_resolution.unwrap_or(1.0))
        };
        settings.haze.enabled = wire.volumetric_haze.unwrap_or(false) && dark;
        settings.haze.steps = wire.haze_steps.unwrap_or(8);
        settings.haze.density = wire.haze_density.unwrap_or(0.0);
        settings.debug_view = wire.debug_view;
        settings.legacy_shadow_eye = (!dark).then_some(DirectionalLight::EDITOR.direction);
        Ok(settings)
    }
}

/// Z-up data space; `pos[2]` is height, rotations are Euler XYZ radians.
#[derive(Debug, Deserialize)]
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

/// A stage piece, in the same Z-up data space as [`Fixture`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Piece {
    /// Venue-unique id.
    pub id: String,
    /// Path under `resources/meshes/`.
    pub mesh_path: String,
    /// Position.
    pub pos: [f32; 3],
    /// Euler XYZ rotation.
    pub rot: [f32; 3],
    /// Uniform scale.
    pub scale: f32,
}

/// One head's evaluated state. Mirrors the eval engine's `PrimitiveState`
/// minus `speed`, which only gates mesh articulation the goldens freeze.
#[derive(Debug, Clone, Copy, Deserialize)]
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
#[derive(Debug, Deserialize)]
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
#[derive(Debug, Deserialize)]
pub struct Mode {
    /// Mode name, matched against `Fixture::mode_name`.
    #[serde(rename = "@Name")]
    pub name: String,
    /// Heads, kept opaque: only the length is read.
    #[serde(rename = "Head", default)]
    pub heads: Vec<serde_json::Value>,
}

/// QLC+ `Physical`.
#[derive(Debug, Deserialize)]
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
#[derive(Debug, Deserialize)]
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
#[derive(Debug, Deserialize)]
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
#[derive(Debug, Deserialize)]
pub struct Lens {
    /// Narrow end of the beam angle, degrees. Zero means "unknown".
    #[serde(rename = "@DegreesMin")]
    pub degrees_min: f32,
    /// Wide end of the beam angle, degrees. Zero means "unknown".
    #[serde(rename = "@DegreesMax")]
    pub degrees_max: f32,
}

impl Catalogue {
    /// # Errors
    /// Fails if the file is missing or is not the shape `dump-golden-scenes.ts`
    /// writes.
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
}

impl Scene {
    /// `<id>-<t>.png`, matching `harness/shot-visualizer.mjs`'s `stamp`.
    #[must_use]
    pub fn frame_name(&self, t: f32) -> String {
        format!("{}-{t:.3}.png", self.id)
    }

    /// The pinned state of one head, if the scene declares it.
    #[must_use]
    pub fn primitive(&self, fixture_id: &str, head: usize) -> Option<PrimitiveState> {
        self.state.get(&format!("{fixture_id}:{head}")).copied()
    }
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
