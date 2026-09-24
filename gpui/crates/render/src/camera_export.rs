//! A frame's camera and sun, written out so a view can be reproduced exactly.
//!
//! The stage's "Export camera" action writes one of these as JSON. It holds
//! the inputs a render needs to put the same camera and sun back (the frame
//! camera, the sun light, the viewport size) and the numbers the renderer
//! derived from them for that frame: the view and projection, and each sun
//! cascade's fit and matrix. The derived numbers come from the same functions
//! the renderer calls, so they are what was drawn, not a re-derivation.
//!
//! A test reproduces the view with [`CameraExport::read`], builds its frame,
//! calls [`CameraExport::apply`] and renders at [`CameraExport::render_size`].
//!
//! Matrices are column-major `[f32; 16]` (`glam::Mat4::to_cols_array`). World
//! space is the renderer's: Z up, metres.

use std::path::Path;

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

use crate::frame::{DirectionalLight, Frame};
use crate::gpu::{
    camera_matrices, cascade_fits, local_bounding_sphere, sun_casters, CAMERA_NEAR, CASCADE_BLEND,
    CASCADE_SPLITS, SHADOW_SIZE,
};

/// The file's layout version. Bump it when a field changes meaning.
pub const VERSION: u32 = 1;

/// One exported view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraExport {
    /// The file layout, [`VERSION`].
    pub version: u32,
    /// UTC, as the exporter wrote it.
    pub exported_at: String,
    /// The room and score.
    pub scene: SceneIdentity,
    /// The render target.
    pub viewport: Viewport,
    /// The camera.
    pub camera: CameraState,
    /// `None` when the frame has no directional light.
    pub sun: Option<Sun>,
    /// The atmosphere's sun, when the room is open air.
    pub sky: Option<Sky>,
    /// The sun shadow cascades.
    pub shadow: ShadowState,
    /// The haze.
    pub haze: Haze,
    /// Union of every caster sphere's box. `None` with no casters.
    pub caster_bounds: Option<Bounds>,
    /// Every sun shadow caster: opaque draws but the ground.
    pub casters: Vec<Caster>,
    /// The renderer quality level, as its `Debug` name.
    pub quality: String,
    /// The diagnostic view, as its `Debug` name.
    pub debug_view: String,
}

/// Which room and score the view was taken of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneIdentity {
    /// The venue id.
    pub venue_id: String,
    /// The venue name.
    pub venue_name: String,
    /// The score lighting the rig, if any.
    pub score_id: Option<String>,
    /// Transport time of the frame, in track seconds.
    pub playhead_s: f32,
}

/// The render target.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// Render pixels, after the stage's render scale and pixel budget.
    pub width: u32,
    /// Render pixels down.
    pub height: u32,
    /// The window's device scale factor.
    pub scale_factor: f32,
}

/// The orbit controller's parameters (`luma_scene::Camera`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Orbit {
    /// The pivot the camera orbits.
    pub target: [f32; 3],
    /// Distance from the pivot, metres.
    pub radius: f32,
    /// Yaw: rotation about +Z from +X, radians.
    pub azimuth: f32,
    /// Angle from +Z, radians. Pitch above the horizon is `pi/2 - polar`.
    pub polar: f32,
}

/// The camera as the scene pass used it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraState {
    /// Eye position, world space.
    pub eye: [f32; 3],
    /// Look-at point, world space.
    pub target: [f32; 3],
    /// World up (+Z).
    pub up: [f32; 3],
    /// Vertical field of view, degrees.
    pub fov_y_deg: f32,
    /// Near plane distance.
    pub near: f32,
    /// Far plane distance.
    pub far: f32,
    /// Always `"perspective-reverse-z"`: near maps to depth one, far to zero.
    pub projection_kind: String,
    /// Orbit parameters, when the camera is an orbit camera.
    pub orbit: Option<Orbit>,
    /// World to view.
    pub view: [f32; 16],
    /// View to clip.
    pub projection: [f32; 16],
    /// World to clip.
    pub view_projection: [f32; 16],
}

/// The directional light.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sun {
    /// Unit direction from the scene toward the sun.
    pub direction: [f32; 3],
    /// Derived from `direction`: degrees from +X toward +Y.
    pub azimuth_deg: f32,
    /// Derived from `direction`: degrees above the horizon.
    pub elevation_deg: f32,
    /// Linear RGB, intensity included.
    pub radiance: [f32; 3],
    /// Shadow-camera anchor (goldens only).
    pub shadow_eye: [f32; 3],
    /// Whether the sun casts shadows.
    pub shadows: bool,
    /// Filter radius in shadow-map texels.
    pub shadow_softness: f32,
}

/// The open-air atmosphere's sun.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sky {
    /// Unit direction toward the sun.
    pub sun_direction: [f32; 3],
    /// Sun irradiance after extinction and exposure.
    pub sun_radiance: [f32; 3],
    /// Display exposure.
    pub exposure: f32,
    /// Ground albedo of the sky table, linear RGB: the floor's mean colour.
    /// Files written before it was a colour hold one number, read as grey.
    #[serde(deserialize_with = "crate::scene_desc::albedo_rgb")]
    pub ground_albedo: [f32; 3],
}

/// The sun's cascaded shadow map for this frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowState {
    /// Texels per side of each cascade.
    pub map_size: u32,
    /// Camera near plane the first slice starts at.
    pub near: f32,
    /// Far camera distance of each slice.
    pub splits: Vec<f32>,
    /// Blend band between cascades, as a fraction of a slice.
    pub blend: f32,
    /// Empty when there is no sun.
    pub cascades: Vec<Cascade>,
}

/// One cascade's fit. Depths are distances along the light, in its fixed
/// light frame (`-z` of `light_view`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cascade {
    /// Camera distances of the view slice.
    pub slice: [f32; 2],
    /// Snapped slice centre in light space, x and y.
    pub centre: [f32; 2],
    /// Half-extent of the orthographic square.
    pub radius: f32,
    /// Nearest and furthest slice corner depth.
    pub slice_depth: [f32; 2],
    /// Nearest depth after reaching back to the casters.
    pub caster_depth: f32,
    /// The orthographic near and far planes as passed to the projection.
    pub planes: [f32; 2],
    /// The fixed light frame.
    pub light_view: [f32; 16],
    /// World to clip.
    pub view_projection: [f32; 16],
}

/// Haze settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Haze {
    /// Mean density; zero is off.
    pub density: f32,
    /// Procedural density and drift.
    pub appearance: crate::scene_desc::HazeAppearance,
    /// Indoor support of the haze.
    pub bounds: Bounds,
    /// Samples per beam.
    pub steps: u32,
    /// Fraction of output resolution.
    pub resolution: f32,
}

/// An axis-aligned box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    /// Lowest corner.
    pub min: [f32; 3],
    /// Highest corner.
    pub max: [f32; 3],
}

/// One shadow caster's world bounding sphere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Caster {
    /// Mesh key.
    pub mesh: String,
    /// World centre.
    pub centre: [f32; 3],
    /// World radius.
    pub radius: f32,
}

/// What the frame alone does not know.
#[derive(Debug, Clone)]
pub struct Context {
    /// UTC, as the exporter wrote it.
    pub exported_at: String,
    /// The room and score.
    pub scene: SceneIdentity,
    /// The render target.
    pub viewport: Viewport,
    /// Orbit parameters, when the camera is an orbit camera.
    pub orbit: Option<Orbit>,
}

impl CameraExport {
    /// Record `frame` as the renderer will draw it into `context.viewport`.
    #[must_use]
    pub fn capture(frame: &Frame, context: Context) -> Self {
        let Viewport { width, height, .. } = context.viewport;
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        let matrices = camera_matrices(frame, aspect);
        let mesh_bounds: Vec<_> = frame
            .meshes
            .iter()
            .map(|mesh| local_bounding_sphere(&mesh.vertices))
            .collect();
        let casters: Vec<_> = sun_casters(frame, &mesh_bounds)
            .map(|(draw, (centre, radius))| Caster {
                mesh: frame.meshes[draw.mesh].key.clone(),
                centre: centre.to_array(),
                radius,
            })
            .collect();
        let caster_bounds = casters
            .iter()
            .map(|caster| {
                let centre = Vec3::from(caster.centre);
                (centre - caster.radius, centre + caster.radius)
            })
            .reduce(|(lo, hi), (a, b)| (lo.min(a), hi.max(b)))
            .map(|(min, max)| Bounds {
                min: min.to_array(),
                max: max.to_array(),
            });
        let cascades = frame.directional.map_or_else(Vec::new, |light| {
            let spheres: Vec<_> = casters
                .iter()
                .map(|caster| (Vec3::from(caster.centre), caster.radius))
                .collect();
            cascade_fits(
                frame.camera.eye,
                (frame.camera.target - frame.camera.eye).normalize_or(Vec3::Y),
                frame.camera.fov_y_deg.to_radians(),
                aspect,
                light.direction,
                &spheres,
            )
            .iter()
            .map(|fit| Cascade {
                slice: [fit.slice.0, fit.slice.1],
                centre: [fit.centre.0, fit.centre.1],
                radius: fit.radius,
                slice_depth: [fit.slice_depth.0, fit.slice_depth.1],
                caster_depth: fit.caster_depth,
                planes: [fit.planes.0, fit.planes.1],
                light_view: fit.light_view.to_cols_array(),
                view_projection: fit.view_proj.to_cols_array(),
            })
            .collect()
        });
        Self {
            version: VERSION,
            exported_at: context.exported_at,
            scene: context.scene,
            viewport: context.viewport,
            camera: CameraState {
                eye: frame.camera.eye.to_array(),
                target: frame.camera.target.to_array(),
                up: Vec3::Z.to_array(),
                fov_y_deg: frame.camera.fov_y_deg,
                near: CAMERA_NEAR,
                far: matrices.far,
                projection_kind: "perspective-reverse-z".into(),
                orbit: context.orbit,
                view: matrices.view.to_cols_array(),
                projection: matrices.proj.to_cols_array(),
                view_projection: (matrices.proj * matrices.view).to_cols_array(),
            },
            sun: frame.directional.map(|light| {
                let direction = light.direction.normalize_or(Vec3::Z);
                Sun {
                    direction: direction.to_array(),
                    azimuth_deg: direction.y.atan2(direction.x).to_degrees(),
                    elevation_deg: direction.z.clamp(-1.0, 1.0).asin().to_degrees(),
                    radiance: light.radiance.to_array(),
                    shadow_eye: light.shadow_eye.to_array(),
                    shadows: light.shadows,
                    shadow_softness: light.shadow_softness,
                }
            }),
            sky: frame.sky.map(|sky| Sky {
                sun_direction: sky.sun_direction.to_array(),
                sun_radiance: sky.sun_radiance.to_array(),
                exposure: sky.exposure,
                ground_albedo: sky.ground_albedo.to_array(),
            }),
            shadow: ShadowState {
                map_size: SHADOW_SIZE,
                near: CAMERA_NEAR,
                splits: CASCADE_SPLITS.to_vec(),
                blend: CASCADE_BLEND,
                cascades,
            },
            haze: Haze {
                density: frame.haze_density,
                appearance: frame.haze_appearance,
                bounds: Bounds {
                    min: frame.haze_bounds.min.to_array(),
                    max: frame.haze_bounds.max.to_array(),
                },
                steps: frame.haze_steps,
                resolution: frame.haze_resolution,
            },
            caster_bounds,
            casters,
            quality: format!("{:?}", frame.quality),
            debug_view: format!("{:?}", frame.debug_view),
        }
    }

    /// Read an exported file.
    ///
    /// # Errors
    /// The file cannot be read or is not an export of this [`VERSION`].
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let export: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(
            export.version == VERSION,
            "camera export version {} is not {VERSION}",
            export.version
        );
        Ok(export)
    }

    /// Write as pretty JSON.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// Put this view's camera and sun on `frame`. The sun replaces the
    /// frame's directional light (and the sky's sun direction, which is the
    /// same light), or removes it when the export had none.
    pub fn apply(&self, frame: &mut Frame) {
        frame.camera = crate::frame::Camera {
            eye: Vec3::from(self.camera.eye),
            target: Vec3::from(self.camera.target),
            fov_y_deg: self.camera.fov_y_deg,
        };
        frame.directional = self.sun.map(|sun| DirectionalLight {
            direction: Vec3::from(sun.direction),
            radiance: Vec3::from(sun.radiance),
            shadow_eye: Vec3::from(sun.shadow_eye),
            shadows: sun.shadows,
            shadow_softness: sun.shadow_softness,
        });
        if let (Some(sky), Some(sun)) = (frame.sky.as_mut(), self.sun) {
            sky.sun_direction = Vec3::from(sun.direction);
        }
    }

    /// The render size the view was drawn at.
    #[must_use]
    pub fn render_size(&self) -> (u32, u32) {
        (self.viewport.width, self.viewport.height)
    }

    /// The view matrix, as a matrix.
    #[must_use]
    pub fn view(&self) -> Mat4 {
        Mat4::from_cols_array(&self.camera.view)
    }

    /// The projection matrix, as a matrix.
    #[must_use]
    pub fn projection(&self) -> Mat4 {
        Mat4::from_cols_array(&self.camera.projection)
    }

    /// Each cascade's world-to-shadow matrix.
    #[must_use]
    pub fn cascade_matrices(&self) -> Vec<Mat4> {
        self.shadow
            .cascades
            .iter()
            .map(|cascade| Mat4::from_cols_array(&cascade.view_projection))
            .collect()
    }
}
