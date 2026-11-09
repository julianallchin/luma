//! One frame's pass chain, encoded into a single submission: shadows, depth,
//! scene, haze, composite and post, editor overlays, and the readback copy.

use std::collections::HashSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use glam::{Mat4, Vec3, Vec4};

use crate::frame::Frame;
use crate::haze_compact::HAZE_WORKGROUP;
use crate::light_index::{LightCore, LightIndexInput, LightRest};
use crate::shadow::{
    assign_shadow_slots, fixture_shadow_caster_hash, fixture_shadow_matrix, fixture_shadow_planes,
    fixture_shadow_texture_array, shadow_matrix_bits, ShadowCacheKey, FIXTURE_SHADOW_SIZE,
    MAX_FIXTURE_SHADOWS,
};
use crate::viewport::PRESENTATION_SLOTS;

use super::buffers::{local_bounding_sphere, pad_at_least_one, ResidentGeometry};
use super::camera::{
    camera_matrices, cascade_cache_keys, cascade_matrices, opaque_depth_hash, sun_casters,
    world_sphere, CameraMatrices,
};
use super::descriptors::{
    binding, depth_attachment, depth_attachment_load, depth_attachment_transient,
};
use super::materials::{material_bind_group, upload_texture_view, MaterialKey, TextureKey};
use super::pipelines::overlay_pipeline_index;
use super::readback::{claim_start_timestamp, Completion, Finish, PendingFrame, PendingProfile};
use super::stats::{CpuSpans, FixtureShadowCpuSpans, ShadowStats, SubmissionCpuSpans};
use super::targets::PresentationTarget;
use super::uniforms::{
    instance_of, CompositeUniform, FixtureShadowMatrix, Globals, HazeUniform, Instance,
    OverlayInstance, SurfaceClusterUniform,
};
use super::{
    Destination, Gpu, Renderer, Transport, CAMERA_NEAR, CASCADE_BLEND, CASCADE_COUNT,
    CASCADE_SPLITS, MSAA_SAMPLES, QUERY_COUNT, SHADOW_SIZE,
};

/// Emitted intensity times the brightest colour channel below which a cone
/// is dark. The frame builder already drops dimmers under 1%; this catches a
/// lit dimmer on a black colour.
const DARK_CONE: f32 = 1e-4;
/// Haze march samples per light for a light the software visibility
/// reference (`visibility_reference`, which only tests turn on) traces.
const REFERENCE_GEOMETRY_SHADOW_SAMPLES: f32 = 2.0;

/// The per-frame work a [`Quality`](crate::scene_desc::Quality) level buys.
/// `High` is the reference picture: its shadow walk is the one the renderer
/// had before the levels existed. Its plain-beam piece cap and shadow redraw
/// budget are new, and measured to leave the picture unchanged. `Low` is
/// `Medium` without the march and without fixture shadows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct QualityBudget {
    /// Whether the haze is marched. Without it no volumetric pass runs —
    /// not the fog grid, the medium cache, the lit-interval cache, the
    /// compaction, the haze kernels, the sun shafts or the haze glow — and
    /// `haze_analytic.wgsl` writes the haze target in closed form.
    marched: bool,
    /// Whether fixtures cast shadows. Without them no fixture shadow map is
    /// assigned, drawn or reduced, and surfaces take each fixture's light
    /// unshadowed. `Frame::fixture_shadows` can only turn them off.
    fixture_shadows: bool,
    /// Gauss-Legendre piece cap for a plain beam (`quadrature_pieces`).
    plain_pieces: f32,
    /// Scale on the haze shadow walk's pixel-footprint allowance.
    shadow_footprint: f32,
    /// Scale on the haze shadow walk's soft and hard step counts.
    shadow_steps: f32,
    /// Most moved fixture shadow maps redrawn in one frame.
    shadow_redraws: usize,
    /// Far-field fog grid cell size in pixels.
    fog_tile: u32,
    /// Scene-pass samples per pixel.
    msaa: u32,
    /// Output pixels per ambient-visibility texel along each axis.
    ambient_divisor: u32,
}

impl QualityBudget {
    fn of(quality: crate::scene_desc::Quality, camera_moving: bool) -> Self {
        use crate::scene_desc::Quality;
        match (quality, camera_moving) {
            (Quality::High, _) => Self {
                marched: true,
                fixture_shadows: true,
                plain_pieces: 8.0,
                shadow_footprint: 1.0,
                shadow_steps: 1.0,
                shadow_redraws: 64,
                fog_tile: crate::fog_grid::TILE_SIZE,
                msaa: MSAA_SAMPLES,
                ambient_divisor: 1,
            },
            // A laptop budget: twice the shadow walk's footprint allowance
            // and half its steps, and twice again while the camera moves,
            // when nothing settles anyway.
            (Quality::Medium, moving) => Self {
                marched: true,
                fixture_shadows: true,
                plain_pieces: 4.0,
                shadow_footprint: if moving { 4.0 } else { 2.0 },
                shadow_steps: if moving { 0.25 } else { 0.5 },
                shadow_redraws: 16,
                fog_tile: crate::fog_grid::LOW_TILE_SIZE,
                msaa: 1,
                ambient_divisor: 1,
            },
            (Quality::Low, moving) => Self {
                marched: false,
                fixture_shadows: false,
                ambient_divisor: 2,
                ..Self::of(Quality::Medium, moving)
            },
        }
    }

    /// `Haze::quality`.
    fn haze(self) -> [f32; 4] {
        [
            self.plain_pieces,
            self.shadow_footprint,
            self.shadow_steps,
            0.0,
        ]
    }
}

impl Renderer {
    pub(super) fn submit_readback(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        destination: Destination,
        slot: usize,
        temporal: bool,
        measure: bool,
    ) -> PendingFrame {
        assert!(
            slot < PRESENTATION_SLOTS,
            "presentation slot is bounded to the target count"
        );
        self.fog_blocks_valid = false;
        let started = Instant::now();
        // Created before the first upload rather than before the first pass:
        // every `storage` upload records a staging-belt copy into this
        // encoder, and a copy is ordered before the passes that read it by
        // being recorded first.
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let profile_resources = self.profiler.as_ref().filter(|_| measure).map(|profile| {
            let resources = &profile.slots[slot];
            (
                resources.query_set.clone(),
                resources.resolve.clone(),
                resources.readback.clone(),
                profile.timestamp_period_ns,
            )
        });
        let mut pass_queries = crate::pass_profile::PassQueries::new(
            profile_resources
                .as_ref()
                .filter(|(q, ..)| q.count() > QUERY_COUNT)
                .map(|(q, ..)| q),
        );
        // --- resident geometry ----------------------------------------------
        // Frame assembly is intentionally cheap and ephemeral, but the meshes
        // it names are immutable. Upload the combined bank only when those
        // stable identities change (normally a venue/asset change), never for
        // camera, transport or fixture-state updates.
        if self
            .geometry
            .as_ref()
            .is_none_or(|geometry| !geometry.matches(frame))
        {
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            let mut ranges = Vec::new();
            let mut bounds = Vec::new();
            for mesh in &frame.meshes {
                bounds.push(local_bounding_sphere(&mesh.vertices));
                let base_vertex = vertices.len() as i32;
                let first_index = indices.len() as u32;
                vertices.extend_from_slice(&mesh.vertices);
                indices.extend(mesh.indices.iter().copied());
                ranges.push((first_index, indices.len() as u32, base_vertex));
            }
            let geometry = ResidentGeometry {
                keys: frame.meshes.iter().map(|mesh| mesh.key.clone()).collect(),
                vertices: self.immutable(&vertices, wgpu::BufferUsages::VERTEX, "vertices"),
                indices: self.immutable(&indices, wgpu::BufferUsages::INDEX, "indices"),
                ranges,
                bounds,
            };
            self.geometry = Some(geometry);
            self.upload_stats.geometry += 1;
        }

        let aspect = width as f32 / height as f32;
        let CameraMatrices {
            far: camera_far,
            view,
            proj,
        } = camera_matrices(frame, aspect);
        let view_proj = proj * view;

        let camera_forward = (frame.camera.target - frame.camera.eye).normalize_or(Vec3::Y);
        let opaque = frame.draws.len() - frame.transparent.len();
        let light_view_proj = frame
            .directional
            .map_or([Mat4::IDENTITY; CASCADE_COUNT], |light| {
                let bounds = &self
                    .geometry
                    .as_ref()
                    .expect("frame geometry uploaded")
                    .bounds;
                let casters: Vec<_> = sun_casters(frame, bounds)
                    .map(|(_, sphere)| sphere)
                    .collect();
                cascade_matrices(
                    frame.camera.eye,
                    camera_forward,
                    frame.camera.fov_y_deg.to_radians(),
                    aspect,
                    light.direction,
                    &casters,
                )
            });

        // A cone that emits nothing visible (a black colour at any dimmer)
        // gets no shadow map, no light-index entry and no beam work.
        let fixture_cones: Vec<_> = frame
            .fixture_cones
            .iter()
            .map(sanitize_fixture_cone)
            .filter(|cone| cone.intensity * cone.color.max_element() > DARK_CONE)
            .take(crate::frame::MAX_FIXTURE_CONES)
            .collect();
        let camera_bits = [
            frame.camera.eye.x,
            frame.camera.eye.y,
            frame.camera.eye.z,
            frame.camera.target.x,
            frame.camera.target.y,
            frame.camera.target.z,
            frame.camera.fov_y_deg,
        ]
        .map(f32::to_bits);
        let camera_moving = temporal
            && self
                .last_live_camera
                .is_some_and(|last| last != camera_bits);
        self.last_live_camera = temporal.then_some(camera_bits);
        let budget = QualityBudget::of(frame.quality, camera_moving);
        let fixture_shadows = frame.fixture_shadows && budget.fixture_shadows;
        let fog_tile = budget.fog_tile;
        let caster_hash = fixture_shadow_caster_hash(frame, opaque);
        // Everything `draw_depth` rasterises, fixture bodies included: the
        // camera depth and the sun cascades draw the same opaque list.
        let depth_hash = opaque_depth_hash(frame, opaque);
        let cached_shadows =
            (self.geometry_shadows || frame.geometry_shadows) && !self.visibility_reference;
        let shadow_capacity = frame
            .fixture_shadow_capacity_hint
            .min(crate::frame::MAX_FIXTURE_CONES)
            .max(fixture_cones.len())
            .next_power_of_two()
            .max(MAX_FIXTURE_SHADOWS);
        if cached_shadows && fixture_shadows && self.fixture_shadow_cache.len() < shadow_capacity {
            // Two banks stay inside the default 256-array-layer device limit,
            // including devices shared with GPUI's compositor.
            let (first, mut layers) =
                fixture_shadow_texture_array(&self.gpu.device, shadow_capacity.min(256) as u32);
            let extra_count = shadow_capacity.saturating_sub(256);
            let (second, extra) =
                fixture_shadow_texture_array(&self.gpu.device, extra_count.max(1) as u32);
            if extra_count > 0 {
                layers.extend(extra);
            }
            self.shadow_hierarchy = crate::shadow_hierarchy::Targets::new(
                &self.gpu.device,
                FIXTURE_SHADOW_SIZE,
                shadow_capacity,
            );
            self.fixture_shadow_map = first;
            self.fixture_shadow_map_extra = second;
            self.fixture_shadow_layers = layers;
            self.fixture_shadow_cache = vec![None; shadow_capacity];
            self.fixture_shadow_slots = vec![None; shadow_capacity];
            self.fixture_shadow_drawn = vec![None; shadow_capacity];
            self.fixture_shadow_age = vec![0; shadow_capacity];
        }
        // Full mode retains every projection; the legacy comparison keeps
        // its priority-based 16-map selection.
        let shadow_slots = if fixture_shadows {
            if cached_shadows {
                let origins: Vec<_> = self
                    .fixture_shadow_drawn
                    .iter()
                    .map(|drawn| drawn.map(|(origin, _)| origin))
                    .collect();
                crate::shadow::assign_cached_slots(
                    &fixture_cones,
                    &self.fixture_shadow_cache,
                    &origins,
                    caster_hash,
                )
            } else {
                let previous = std::array::from_fn(|i| self.fixture_shadow_slots[i]);
                assign_shadow_slots(&fixture_cones, frame.camera.eye, &previous).to_vec()
            }
        } else {
            vec![None; self.fixture_shadow_cache.len()]
        };
        self.fixture_shadow_slots = shadow_slots.clone();
        let fixture_shadow_count = shadow_slots.iter().filter(|slot| slot.is_some()).count();
        let cores: Vec<LightCore> = fixture_cones
            .iter()
            .map(|light| LightCore {
                position: light.position.to_array(),
                range: light.range.clamp(0.05, 100.0),
            })
            .collect();
        let mut slot_of = vec![-1.0_f32; fixture_cones.len()];
        for (slot, resident) in shadow_slots.iter().enumerate() {
            if let Some(index) = resident {
                slot_of[*index] = slot as f32;
            }
        }
        let rests: Vec<LightRest> = fixture_cones
            .iter()
            .enumerate()
            .map(|(index, light)| {
                let direction = light.direction.try_normalize().unwrap_or(Vec3::NEG_Y);
                let helper = if direction.z.abs() > 0.98 {
                    Vec3::Y
                } else {
                    Vec3::Z
                };
                let field = light.cos_field.clamp(-1.0, 1.0);
                LightRest {
                    direction: direction.to_array(),
                    cos_beam: light.cos_beam.clamp(-1.0, 1.0),
                    color: light.color.to_array(),
                    intensity: light.intensity.clamp(0.0, 100.0),
                    cos_field: light.cos_field.clamp(-1.0, 1.0),
                    wash: light.wash.clamp(0.0, 1.0),
                    gobo: light.gobo.min(2) as f32,
                    gobo_rotation: light.gobo_rotation.rem_euclid(std::f32::consts::TAU),
                    shadow_slot: slot_of[index],
                    haze_gain: light.haze_gain.clamp(0.0, 1.0),
                    inverse_right_length: direction.cross(helper).length().recip(),
                    field_tangent: (1.0 - field * field).max(0.0).sqrt() / field.max(0.05),
                    lens_distance: light.lens_distance(),
                    lens_reserved: [0.0; 3],
                }
            })
            .collect();
        let fixture_shadow_matrices: Vec<FixtureShadowMatrix> = shadow_slots
            .iter()
            .map(|resident| match resident {
                Some(index) => {
                    let cone = &fixture_cones[*index];
                    let (near, far) = fixture_shadow_planes(cone);
                    FixtureShadowMatrix {
                        view_proj: fixture_shadow_matrix(cone).to_cols_array_2d(),
                        params: [near, far, 0.0, 0.0],
                    }
                }
                None => FixtureShadowMatrix {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                    params: [0.1, 1.0, 0.0, 0.0],
                },
            })
            .collect();
        // Bound the maps redrawn per frame. A moved head over the budget keeps
        // sampling the map it has, with the projection that map was drawn
        // with, until its turn comes.
        let mut fixture_shadow_matrices = fixture_shadow_matrices;
        if cached_shadows && fixture_shadows {
            let dirty: Vec<bool> = fixture_shadow_matrices
                .iter()
                .zip(&shadow_slots)
                .zip(&self.fixture_shadow_cache)
                .map(|((matrix, resident), cached)| {
                    resident.is_some()
                        && *cached
                            != Some(ShadowCacheKey {
                                matrix_bits: shadow_matrix_bits(&matrix.view_proj),
                                caster_hash,
                            })
                })
                .collect();
            let moved: Vec<bool> = shadow_slots
                .iter()
                .zip(&self.fixture_shadow_drawn)
                .zip(&self.fixture_shadow_cache)
                .map(|((resident, drawn), cached)| {
                    // Same head and the same casters: only the projection moved.
                    resident
                        .zip(*drawn)
                        .is_some_and(|(index, (origin, _))| origin == fixture_cones[index].position)
                        && cached.is_some_and(|key| key.caster_hash == caster_hash)
                })
                .collect();
            let priority: Vec<f32> = shadow_slots
                .iter()
                .map(|resident| {
                    resident.map_or(0.0, |index| {
                        crate::shadow::shadow_priority(&fixture_cones[index], frame.camera.eye)
                    })
                })
                .collect();
            let redraw = crate::shadow::budget_redraws(
                &dirty,
                &moved,
                &priority,
                &self.fixture_shadow_age,
                budget.shadow_redraws,
            );
            for slot in 0..dirty.len() {
                if dirty[slot] && !redraw[slot] {
                    let (_, drawn) =
                        self.fixture_shadow_drawn[slot].expect("a moved map was drawn");
                    fixture_shadow_matrices[slot] = drawn;
                    self.fixture_shadow_age[slot] += 1;
                } else {
                    self.fixture_shadow_age[slot] = 0;
                }
            }
        }
        // Build the unified light index for this frame: sanitise + depth-sort +
        // Z-bins on the CPU, tile masks in two compute dispatches, and the
        // reordered light SoA upload. Rebuilt every frame — no cache, no key:
        // a camera-derived key is unsound for a static camera with moving
        // lights, and the build is two small dispatches plus a 512-cone sort.
        let cluster_started = Instant::now();
        let light_index_bg = self
            .light_index
            .build(
                &self.gpu.light_index_pipelines,
                &self.gpu.device,
                &self.gpu.queue,
                &mut encoder,
                &LightIndexInput {
                    cones: &fixture_cones,
                    camera: frame.camera,
                    viewport: [width, height],
                    near: CAMERA_NEAR,
                    far: camera_far,
                },
                &cores,
                &rests,
                pass_queries.compute(
                    "light-index",
                    profile_resources.as_ref().map(|(queries, ..)| {
                        wgpu::ComputePassTimestampWrites {
                            query_set: queries,
                            beginning_of_pass_write_index: Some(4),
                            end_of_pass_write_index: Some(5),
                        }
                    }),
                ),
            )
            .clone();
        let clusters_done = Instant::now();
        let cpu_cluster = clusters_done - cluster_started;

        let haze_density = frame
            .haze_density
            .is_finite()
            .then_some(frame.haze_density.clamp(0.0, 4.0))
            .unwrap_or(0.0);
        let haze_time = frame.time.is_finite().then_some(frame.time).unwrap_or(0.0);
        let medium =
            crate::medium::Uniform::new(frame, haze_density * Transport::EXTINCTION, haze_time);
        let outdoor_sun = frame
            .sky
            .map_or(Vec3::ZERO, |sky| sky.sun_radiance)
            .extend(Transport::PHASE_G)
            .to_array();
        // The compute kernels draw the haze, except for the exhaustive
        // reference's per-ray fragment pass.
        let per_ray = self.reference;
        // The far-field grid chain runs under the compute kernels whenever
        // there is haze to march.
        let grid_fog = budget.marched && !per_ray && haze_density >= 0.001;
        // The far-field grid's radial extent: the haze uniform's `shadow.w`,
        // and the surface pass's `surface_fog.x` when it reads that grid.
        let fog_far = fixture_cones
            .iter()
            .map(|c| frame.camera.eye.distance(c.position) + c.range)
            .fold(1.0_f32, f32::max)
            .min(camera_far);
        // The surface takes outdoor camera transmittance from the far-field
        // fog grid on frames that run the fog chain, so the scene pass is
        // encoded after `fog-transmittance` then; otherwise it marches.
        let scene_after_fog = grid_fog;
        let mut globals = Globals {
            surface_fog: [fog_far, f32::from(u8::from(grid_fog)), 0.0, 0.0],
            viewport: [
                width as f32,
                height as f32,
                1.0 / width as f32,
                1.0 / height as f32,
            ],
            medium,
            outdoor_sun,
            view_proj: view_proj.to_cols_array_2d(),
            light_view_proj: light_view_proj.map(|matrix| matrix.to_cols_array_2d()),
            camera_pos: frame.camera.eye.extend(1.0).to_array(),
            camera_forward: camera_forward.extend(0.0).to_array(),
            cascade_splits: [
                CASCADE_SPLITS[0],
                CASCADE_SPLITS[1],
                CASCADE_SPLITS[2],
                CASCADE_BLEND,
            ],
            ambient: frame.ambient.extend(0.0).to_array(),
            room: frame.room.map_or([0.0; 4], |room| {
                [room.centre.x, room.centre.y, room.half.x, room.half.y]
            }),
            room_falloff: [frame.room.map_or(0.0, |room| room.margin), 0.0, 0.0, 0.0],
            dir_to_light: frame
                .directional
                .map_or(Vec4::ZERO, |light| light.direction.extend(1.0))
                .to_array(),
            dir_color: frame
                .directional
                .map_or(Vec4::ZERO, |light| {
                    light.radiance.extend(light.shadow_softness)
                })
                .to_array(),
            params: [
                0.0,
                1.0 / SHADOW_SIZE as f32,
                f32::from(u8::from(
                    frame.directional.is_some_and(|light| light.shadows),
                )),
                frame.debug_view.shader_code() as f32,
            ],
            floor: frame.floor.map_or([[0.0; 4]; 4], |floor| floor.uniform()),
        };
        if self.environment.prepare(
            &self.gpu.environment,
            &self.gpu.device,
            &self.gpu.queue,
            frame.environment.as_ref(),
        ) {
            self.upload_stats.environments += 1;
        }
        let ambient_buf = self.environment.ambient(&self.gpu.device);
        let mut ambient_probe = self.environment.irradiance(&self.gpu.environment).clone();
        let (probe_cubes, probe_grid, probe_ambient) = self.probes.bindings();
        let local_probes = crate::environment::LocalProbes {
            cubes: probe_cubes,
            dummy: self.gpu.probes.dummy_cubes(),
            grid: probe_grid,
            ambient: probe_ambient,
        };
        let (_environment_uniform, environment_groups) = self.environment.bind_group(
            &self.gpu.environment,
            &self.gpu.device,
            frame.environment.as_ref(),
            &ambient_buf,
            &local_probes,
        );
        let mut environment_bg = environment_groups.scene;
        let mut probe_environment_bg = environment_groups.relight;
        // The sky, when there is one, is both the background the composite
        // resolves and the probe the scene pass is lit by. Its tables are
        // rebuilt only when the sun moves, so this is a cache hit for every
        // frame of a fixed hour.
        // The clouds are traced here too, from the unjittered camera: they
        // are the background, and the composite reads them.
        let (sky_bg, sky_probe) = self.atmosphere.prepare(
            &self.gpu.atmosphere,
            &self.gpu.environment,
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            frame.sky.as_ref(),
            &crate::atmosphere::CloudFrame {
                view_proj,
                eye: frame.camera.eye,
                width,
                height,
                time: frame.time,
                temporal,
                quality: frame.quality,
            },
            &mut pass_queries,
        );
        let aerial = self.atmosphere.prepare_aerial(
            &self.gpu.atmosphere,
            &self.gpu.device,
            &mut encoder,
            frame.sky.as_ref(),
            frame.camera.eye,
            &mut pass_queries,
        );
        if let Some(probe) = &sky_probe {
            let groups = self.gpu.environment.sky_bind_group(
                &self.gpu.device,
                probe,
                &ambient_buf,
                &local_probes,
            );
            environment_bg = groups.scene;
            probe_environment_bg = groups.relight;
            ambient_probe = probe.0.clone();
        }
        // Frame-constant probe mean, computed once here instead of by six cube
        // samples in every surface and composite fragment. Encoded after the
        // probe it reads and before every pass that reads the result.
        self.gpu.environment.dispatch_ambient(
            &self.gpu.device,
            &mut encoder,
            &ambient_probe,
            &ambient_buf,
        );

        let geometry = self.geometry.as_ref().expect("frame geometry uploaded");
        let vertex_buf = geometry.vertices.clone();
        let index_buf = geometry.indices.clone();
        let ranges = geometry.ranges.clone();
        let mesh_bounds = geometry.bounds.clone();

        // Outdoors, the stage's height field: redrawn only when its geometry
        // changes. Its presence switches the scene pass's sky term to the
        // occluded split (`globals.room_falloff.y`).
        let sky_field_redraw = self.sky_visibility.update_field(
            &self.gpu.device,
            frame,
            opaque,
            caster_hash,
            &mesh_bounds,
            frame.sky.is_some(),
        );
        globals.room_falloff[1] = f32::from(u8::from(self.sky_visibility.active()));
        let sun_direction = frame
            .directional
            .map(|light| light.direction.normalize_or_zero());
        let sky_height_buf = self.storage(
            &mut encoder,
            &[self.sky_visibility.height_params(sun_direction)],
            wgpu::BufferUsages::UNIFORM,
            "sky-height-params",
        );
        let ambient_params_buf = self.storage(
            &mut encoder,
            &[crate::sky_visibility::ao_params(
                frame.camera.eye,
                camera_forward,
                frame.camera.fov_y_deg.to_radians(),
                [width, height],
                [CAMERA_NEAR, camera_far],
                self.sky_visibility.active(),
                budget.ambient_divisor,
            )],
            wgpu::BufferUsages::UNIFORM,
            "ambient-visibility-params",
        );

        let instances: Vec<Instance> = frame
            .draws
            .iter()
            .map(|draw| instance_of(frame, draw))
            .collect();
        let overlay_instances: Vec<OverlayInstance> = frame
            .overlays
            .iter()
            .map(|o| OverlayInstance {
                model: o.model.to_cols_array_2d(),
                color: o.color.extend(o.opacity).to_array(),
            })
            .collect();
        // Upload each immutable image once per color-space role. A source can
        // legitimately be used as both base color (sRGB) and a data map
        // (linear), and those require different GPU formats.
        let material_keys: Vec<MaterialKey> = frame
            .draws
            .iter()
            .map(|draw| MaterialKey::of(draw, frame))
            .collect();
        for (draw, key) in frame.draws.iter().zip(&material_keys) {
            let roles = [
                (draw.textures.base_color, key.base_color.as_ref()),
                (draw.textures.normal, key.normal.as_ref()),
                (
                    draw.textures.metallic_roughness,
                    key.metallic_roughness.as_ref(),
                ),
                (draw.textures.occlusion, key.occlusion.as_ref()),
                (draw.textures.emissive, key.emissive.as_ref()),
            ];
            for (index, texture_key) in roles {
                let Some((index, texture_key)) = index.zip(texture_key) else {
                    continue;
                };
                if !self.texture_views.contains_key(texture_key) {
                    let view = upload_texture_view(
                        &self.gpu.device,
                        &self.gpu.queue,
                        &frame.images[index].image,
                        texture_key.encoding,
                    );
                    self.texture_views.insert(texture_key.clone(), view);
                    self.upload_stats.textures += 1;
                }
            }
        }
        let used_textures: HashSet<&TextureKey> = material_keys
            .iter()
            .flat_map(MaterialKey::textures)
            .collect();
        self.texture_views
            .retain(|key, _| used_textures.contains(key));
        self.materials
            .retain(|key, _| material_keys.iter().any(|used| used == key));
        for key in &material_keys {
            if self.materials.contains_key(key) {
                continue;
            }
            let view = |texture: Option<&TextureKey>, default| {
                texture.map_or(default, |key| &self.texture_views[key])
            };
            let bind_group = material_bind_group(
                &self.gpu.device,
                &self.gpu.material_layout,
                if key.floor {
                    &self.gpu.floor_sampler
                } else {
                    &self.gpu.texture_sampler
                },
                view(
                    key.base_color.as_ref(),
                    &self.gpu.material_defaults.base_color,
                ),
                view(key.normal.as_ref(), &self.gpu.material_defaults.normal),
                view(
                    key.metallic_roughness.as_ref(),
                    &self.gpu.material_defaults.metallic_roughness,
                ),
                view(
                    key.occlusion.as_ref(),
                    &self.gpu.material_defaults.occlusion,
                ),
                view(key.emissive.as_ref(), &self.gpu.material_defaults.emissive),
            );
            self.materials.insert(key.clone(), bind_group);
        }
        let materials: Vec<wgpu::BindGroup> = material_keys
            .iter()
            .map(|key| self.materials[key].clone())
            .collect();

        let instance_buf = self.storage(
            &mut encoder,
            &pad_at_least_one(instances),
            wgpu::BufferUsages::STORAGE,
            "instances",
        );
        let globals_buf = self.storage(
            &mut encoder,
            &[globals],
            wgpu::BufferUsages::UNIFORM,
            "globals",
        );
        let fixture_shadow_matrix_buf = self.storage(
            &mut encoder,
            &pad_at_least_one(fixture_shadow_matrices.clone()),
            wgpu::BufferUsages::STORAGE,
            "fixture-shadow-matrices",
        );
        // Avoid an extra geometry pass for short light lists.
        let surface_depth_cull = self.surface_depth_cull
            && frame.fixture_surface_lighting
            && self.light_index.stats().lights_on_screen >= 32;
        let cluster_uniform = self.storage(
            &mut encoder,
            &[SurfaceClusterUniform {
                flags: [
                    f32::from(u8::from(frame.fixture_surface_lighting)),
                    f32::from(u8::from(frame.cluster_debug)),
                    f32::from(u8::from(surface_depth_cull)),
                    0.0,
                ],
                shadow: [
                    fixture_shadow_count as f32,
                    1.0 / FIXTURE_SHADOW_SIZE as f32,
                    // A cone's `intensity` is a 0..1 dimmer times its optic
                    // gain, not radiance — the absolute scale is the transport
                    // beam gain, and the surface pass must apply the *same*
                    // scale the haze march does or the two halves of one beam
                    // disagree about how bright it is (bright shaft over a
                    // black pool).
                    Transport::BEAM_GAIN,
                    0.0,
                ],
            }],
            wgpu::BufferUsages::UNIFORM,
            "surface-cluster-uniform",
        );
        self.visibility.update(
            &self.gpu.device,
            frame,
            self.visibility_reference
                && (self.geometry_shadows || frame.geometry_shadows)
                && fixture_shadows,
        );
        let index_bindings = self.light_index.bindings();
        let overlay_buf = self.storage(
            &mut encoder,
            &pad_at_least_one(overlay_instances),
            wgpu::BufferUsages::STORAGE,
            "overlays",
        );
        let overlay_bg = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("overlay"),
                layout: &self.gpu.overlay_layout,
                entries: &[
                    binding(0, globals_buf.as_entire_binding()),
                    binding(1, overlay_buf.as_entire_binding()),
                ],
            });

        let hard_shadows = frame
            .directional
            .is_some_and(|light| light.shadow_softness == 0.0);
        let lit_bg =
            self.scene_bind_group(&globals_buf, &instance_buf, true, hard_shadows, &aerial);
        let unlit_bg = self.scene_bind_group(&globals_buf, &instance_buf, false, false, &aerial);
        let shadow_bgs: Vec<_> = light_view_proj
            .iter()
            .enumerate()
            .map(|(cascade, matrix)| {
                let mut shadow_globals = globals;
                shadow_globals.light_view_proj[0] = matrix.to_cols_array_2d();
                // Indexed label: one pooled buffer per cascade — see `storage`.
                let buffer = self.storage(
                    &mut encoder,
                    &[shadow_globals],
                    wgpu::BufferUsages::UNIFORM,
                    &format!("shadow-globals-{cascade}"),
                );
                (
                    buffer.clone(),
                    self.scene_bind_group(&buffer, &instance_buf, false, false, &aerial),
                )
            })
            .collect();
        let shadow_globals_started = Instant::now();
        let fixture_shadow_keys: Vec<_> = fixture_shadow_matrices
            .iter()
            .map(|matrix| ShadowCacheKey {
                matrix_bits: shadow_matrix_bits(&matrix.view_proj),
                caster_hash,
            })
            .collect();
        let fixture_shadow_dirty: Vec<_> = fixture_shadow_keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                // Vacant slots retain their previous depth and cache key for
                // reuse. Their placeholder matrix does not invalidate either
                // the map or its hierarchy: no light reads that slot now.
                shadow_slots[index].is_some() && self.fixture_shadow_cache[index] != Some(*key)
            })
            .collect();
        // Only a map that is about to be redrawn needs a projection uniform and
        // a bind group. Building all 128 regardless was most of this frame's
        // encode time on the shadowed preset.
        let fixture_shadow_bgs: Vec<_> = fixture_shadow_matrices
            .iter()
            .zip(&fixture_shadow_dirty)
            .enumerate()
            .map(|(slot, (matrix, dirty))| {
                dirty.then(|| {
                    let mut shadow_globals = globals;
                    shadow_globals.light_view_proj[0] = matrix.view_proj;
                    // Indexed label: one pooled buffer per shadow slot.
                    let buffer = self.storage(
                        &mut encoder,
                        &[shadow_globals],
                        wgpu::BufferUsages::UNIFORM,
                        &format!("fixture-shadow-globals-{slot}"),
                    );
                    buffer
                })
            })
            .collect();

        let mut shadow_cpu = FixtureShadowCpuSpans {
            globals: shadow_globals_started.elapsed(),
            ..Default::default()
        };

        let (t_width, t_height) = (width, height);
        // Below a quarter the bilateral upsample has too few taps per output
        // pixel to anchor on and beams start to crawl; above native there is
        // nothing left to gain. `max(1)` covers an element only a few pixels
        // tall, where the ratio alone would round the target away.
        let scale = frame.haze_resolution.clamp(0.25, 1.0);
        let haze_size = (
            ((width as f32 * scale).round() as u32).max(1),
            ((height as f32 * scale).round() as u32).max(1),
        );
        let targets_started = Instant::now();
        let (
            msaa_color,
            msaa_surface_depth,
            msaa_depth,
            scene_view,
            depth_view,
            haze_view,
            fog_grid,
            fog_integral,
            fog_transmittance,
            fog_tau,
            fog_columns,
            fog_candidates,
            output_view,
            channels,
            finish,
            bytes_per_row,
        ) = {
            let t = self.targets(
                t_width,
                t_height,
                haze_size,
                destination,
                grid_fog,
                surface_depth_cull,
                fog_tile,
                budget.msaa,
            );
            let presentation = &t.presentations[slot];
            let finish = match presentation {
                PresentationTarget::Staged {
                    output, readback, ..
                } => Finish::Copy(output.clone(), readback.clone()),
                PresentationTarget::Shared(shared) => Finish::Share(shared.surface()),
            };
            (
                t.msaa_color.clone(),
                t.msaa_surface_depth.clone(),
                t.msaa_depth.clone(),
                t.scene.clone(),
                t.depth.clone(),
                t.haze.clone(),
                t.fog.incident.clone(),
                t.fog.integral.clone(),
                t.fog.transmittance.clone(),
                t.fog.tau.clone(),
                t.fog.columns.clone(),
                t.fog.candidates.clone(),
                presentation.view().clone(),
                presentation.channels(),
                finish,
                t.bytes_per_row,
            )
        };
        let targets_done = Instant::now();
        let ambient_visibility =
            self.sky_visibility
                .output(&self.gpu.device, width, height, budget.ambient_divisor);
        // Outdoor haze with a sun: the sun-shaft target, which the scene pass
        // reads per fragment through binding 16. A texel of full sun stands
        // in when the pass does not run.
        let shafts = (budget.marched && frame.sky.is_some() && medium.min[3] > 0.0).then(|| {
            self.sun_shafts
                .target(&self.gpu.device, t_width, t_height, frame.quality)
        });
        let shaft_view = shafts
            .clone()
            .unwrap_or_else(|| self.gpu.sun_shafts.off.clone());
        // Built after the targets: binding 13 is this frame's fog prefix.
        // The probe relight's copy binds every cone in source order in place
        // of the light index's in-view subset (`probes.rs`).
        self.probes
            .set_lights(&self.gpu.device, &self.gpu.queue, &cores, &rests);
        let probe_work = self.probes.prepare(&self.gpu.queue, frame, opaque);
        let probe_debug_balls = self.probes.debug_balls();
        // Where the haze's glow is this frame, before the surfaces and the
        // composite bind it; it is filled after the shadow maps.
        self.haze_glow
            .prepare(&self.gpu.device, &self.gpu.queue, &medium, frame);
        let glow_bindings = self.haze_glow.bindings(17);
        let cluster_group = |label: &str, core: &wgpu::Buffer, rest: &wgpu::Buffer| {
            self.gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &self.gpu.cluster_layout,
                    entries: &[
                        glow_bindings[0].clone(),
                        glow_bindings[1].clone(),
                        glow_bindings[2].clone(),
                        binding(0, core.as_entire_binding()),
                        binding(1, rest.as_entire_binding()),
                        binding(4, cluster_uniform.as_entire_binding()),
                        binding(5, fixture_shadow_matrix_buf.as_entire_binding()),
                        binding(
                            6,
                            wgpu::BindingResource::TextureView(&self.fixture_shadow_map),
                        ),
                        binding(7, wgpu::BindingResource::Sampler(&self.gpu.linear_sampler)),
                        binding(8, index_bindings.params.as_entire_binding()),
                        binding(9, index_bindings.tile_masks.as_entire_binding()),
                        binding(10, index_bindings.z_bins.as_entire_binding()),
                        binding(11, self.visibility.buffer.as_entire_binding()),
                        binding(
                            12,
                            wgpu::BindingResource::TextureView(&self.fixture_shadow_map_extra),
                        ),
                        binding(13, wgpu::BindingResource::TextureView(&fog_transmittance)),
                        binding(14, index_bindings.surface_splits.as_entire_binding()),
                        binding(15, wgpu::BindingResource::TextureView(&ambient_visibility)),
                        binding(16, wgpu::BindingResource::TextureView(&shaft_view)),
                    ],
                })
        };
        let cluster_bg = cluster_group(
            "surface-clusters",
            &index_bindings.core,
            &index_bindings.rest,
        );
        let probe_cluster_bg = self
            .probes
            .cone_buffers()
            .map(|(core, rest)| cluster_group("probe-clusters", core, rest));
        // This block runs to the end of the function: `encode_scene` below
        // captures its draw closures and is called either here or, when the
        // surface pass reads the fog grid, after `fog-integrate`.
        {
            let mut pending_start = profile_resources.as_ref().map(|(queries, ..)| queries);
            let all_opaque: Vec<usize> = (0..opaque).collect();
            // Under a floor material the ground has its own fragment shader.
            let (floor_draws, lit_draws): (Vec<usize>, Vec<usize>) =
                all_opaque.iter().partition(|&&index| {
                    frame.floor.is_some()
                        && crate::frame::is_ground(&frame.meshes[frame.draws[index].mesh].key)
                });
            let transparent: Vec<usize> = (opaque..frame.draws.len()).collect();
            // Which casters each shadow map actually needs.
            //
            // A fixture lights a cone a few metres long; the venue around it is
            // not in that cone and contributes nothing to its depth map. Drawing
            // all of it anyway made the shadow passes the whole frame — at a
            // realistic draw count they submitted 16 x every opaque draw, and
            // cost more than everything else put together.
            //
            // Only computed for maps that are about to render: a clean map
            // keeps the caster set it was drawn with, because the same key that
            // says the depth is still valid says the caster set is too. A moved
            // head is dirty by definition, so its set is never stale.
            let has_fixture_shadow_pass = fixture_shadow_dirty.iter().any(|dirty| *dirty);
            let shadow_cull_started = Instant::now();
            // A draw's world-space sphere is shared by every light in this
            // submission. Transform it once, using the same largest-axis
            // scale as the cone test used per light. Rebuild on each dirty
            // frame so moved or rescaled casters cannot leave stale bounds.
            let caster_bounds: Vec<_> = if has_fixture_shadow_pass {
                frame.draws[..opaque]
                    .iter()
                    .map(|draw| world_sphere(draw, mesh_bounds[draw.mesh]))
                    .collect()
            } else {
                Vec::new()
            };
            let shadow_casters: Vec<Vec<usize>> = shadow_slots
                .iter()
                .zip(&fixture_shadow_dirty)
                .map(|(resident, dirty)| {
                    let Some(cone) = resident
                        .filter(|_| *dirty)
                        .map(|index| &fixture_cones[index])
                    else {
                        return Vec::new();
                    };
                    let direction = cone.direction.try_normalize().unwrap_or(Vec3::NEG_Z);
                    all_opaque
                        .iter()
                        .copied()
                        .filter(|&index| {
                            let (center, radius) = caster_bounds[index];
                            crate::light_index::cone_reaches_sphere(
                                cone.apex(),
                                direction,
                                cone.apex_reach(),
                                cone.cos_field,
                                center,
                                radius,
                            )
                        })
                        .collect()
                })
                .collect();
            shadow_cpu.cull = shadow_cull_started.elapsed();
            let shadow_buckets_started = Instant::now();
            let redrawn_maps = shadow_casters
                .iter()
                .zip(&fixture_shadow_bgs)
                .filter(|(_, prepared)| prepared.is_some())
                .count();
            self.shadow_stats = ShadowStats {
                redrawn_maps,
                hierarchy_layers: 0,
                caster_draws: shadow_casters
                    .iter()
                    .zip(&fixture_shadow_bgs)
                    .filter(|(_, prepared)| prepared.is_some())
                    .map(|(casters, _)| casters.len())
                    .sum(),
                unculled_draws: redrawn_maps * opaque,
                // Filled below once the buckets exist.
                mesh_draws: 0,
            };
            // One instanced draw per (map, distinct mesh): bucket each map's
            // casters by mesh and concatenate the draw indices into one
            // storage buffer the fixture-shadow vertex stage indexes through
            // `instance_index`. Fixture bodies are excluded here (they used
            // to be skipped at draw time): a luminaire sits at the apex of
            // its own cone and would shadow every sample.
            let mut caster_instance_data: Vec<u32> = Vec::new();
            let shadow_mesh_buckets: Vec<Vec<(usize, u32, u32)>> = shadow_casters
                .iter()
                .map(|casters| {
                    let mut sorted: Vec<usize> = casters
                        .iter()
                        .copied()
                        .filter(|&index| {
                            !matches!(
                                &frame.draws[index].editor_object,
                                Some(crate::frame::EditorObject::Fixture(_))
                            )
                        })
                        .collect();
                    // Mesh-major, then draw order, so the buffer contents are
                    // reproducible across frames.
                    sorted.sort_by_key(|&index| (frame.draws[index].mesh, index));
                    let mut buckets = Vec::new();
                    let mut cursor = 0;
                    while cursor < sorted.len() {
                        let mesh = frame.draws[sorted[cursor]].mesh;
                        let first = caster_instance_data.len() as u32;
                        let mut count = 0;
                        while cursor < sorted.len() && frame.draws[sorted[cursor]].mesh == mesh {
                            caster_instance_data.push(sorted[cursor] as u32);
                            count += 1;
                            cursor += 1;
                        }
                        buckets.push((mesh, first, count));
                    }
                    buckets
                })
                .collect();
            let mesh_draws = shadow_mesh_buckets
                .iter()
                .map(|buckets| buckets.len())
                .sum();
            self.shadow_stats.mesh_draws = mesh_draws;
            shadow_cpu.buckets = shadow_buckets_started.elapsed();
            let shadow_resources_started = Instant::now();
            let caster_instance_buf = self.storage(
                &mut encoder,
                &pad_at_least_one(caster_instance_data),
                wgpu::BufferUsages::STORAGE,
                "caster-instances",
            );
            let fixture_shadow_pass_bgs: Vec<Option<wgpu::BindGroup>> = fixture_shadow_bgs
                .iter()
                .map(|prepared| {
                    prepared.as_ref().map(|globals_buf| {
                        self.gpu
                            .device
                            .create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("fixture-shadow"),
                                layout: &self.gpu.fixture_shadow_layout,
                                entries: &[
                                    binding(0, globals_buf.as_entire_binding()),
                                    binding(1, instance_buf.as_entire_binding()),
                                    binding(5, caster_instance_buf.as_entire_binding()),
                                ],
                            })
                    })
                })
                .collect();
            shadow_cpu.resources = shadow_resources_started.elapsed();
            let draw_range = |pass: &mut wgpu::RenderPass, range: &[usize]| {
                pass.set_vertex_buffer(0, vertex_buf.slice(..));
                pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                let mut bound = None;
                for &i in range {
                    let draw = &frame.draws[i];
                    if bound != Some(&material_keys[i]) {
                        bound = Some(&material_keys[i]);
                        pass.set_bind_group(1, &materials[i], &[]);
                    }
                    let (first, last, base) = ranges[draw.mesh];
                    pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                }
            };
            // Depth shaders do not sample materials. Consecutive instances
            // of one mesh can share a draw without changing primitive order,
            // including the order of coplanar receivers in the MSAA pass.
            //
            // The sun cascades leave the ground out (`with_ground` false): it
            // only receives, and its own depth in the map was what a grazing
            // sun needed a large receiver offset against.
            let draw_depth = |pass: &mut wgpu::RenderPass, with_ground: bool| {
                pass.set_vertex_buffer(0, vertex_buf.slice(..));
                pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                pass.set_bind_group(1, &self.gpu.white_material, &[]);
                let mut start = 0;
                while start < opaque {
                    let mesh = frame.draws[start].mesh;
                    let mut end = start + 1;
                    while end < opaque && frame.draws[end].mesh == mesh {
                        end += 1;
                    }
                    if with_ground || !crate::frame::is_ground(&frame.meshes[mesh].key) {
                        let (first, last, base) = ranges[mesh];
                        pass.draw_indexed(first..last, base, start as u32..end as u32);
                    }
                    start = end;
                }
            };

            if has_fixture_shadow_pass {
                let shadow_maps_started = Instant::now();
                for ((layer, prepared), buckets) in self
                    .fixture_shadow_layers
                    .iter()
                    .zip(&fixture_shadow_pass_bgs)
                    .zip(&shadow_mesh_buckets)
                {
                    // A slot has a bind group exactly when it is both occupied
                    // and dirty, so the two cannot disagree about which passes
                    // to encode. Slots are sparse — hysteresis leaves gaps —
                    // so occupancy is per slot, not a prefix count.
                    let Some(bind_group) = prepared else {
                        continue;
                    };

                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("fixture-shadow"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(depth_attachment(layer)),
                        timestamp_writes: pass_queries
                            .render("fixture-shadow", claim_start_timestamp(&mut pending_start)),
                        ..Default::default()
                    });
                    // An empty caster list still runs: the attachment's clear
                    // is what makes the map read as unoccluded, and skipping the
                    // pass would leave whatever the slot's previous tenant wrote.
                    pass.set_pipeline(&self.gpu.fixture_shadow_pipeline);
                    pass.set_bind_group(0, bind_group, &[]);
                    pass.set_vertex_buffer(0, vertex_buf.slice(..));
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(mesh, first, count) in buckets {
                        let (first_index, last_index, base) = ranges[mesh];
                        pass.draw_indexed(first_index..last_index, base, first..first + count);
                    }
                }
                shadow_cpu.maps = shadow_maps_started.elapsed();
                let shadow_hierarchy_started = Instant::now();
                self.shadow_stats.hierarchy_layers = self.shadow_hierarchy.record(
                    &self.gpu.shadow_hierarchy_pipelines,
                    &self.gpu.device,
                    &mut encoder,
                    [&self.fixture_shadow_map, &self.fixture_shadow_map_extra],
                    &fixture_shadow_dirty,
                    &mut pass_queries,
                );
                shadow_cpu.hierarchy = shadow_hierarchy_started.elapsed();
                for (index, key) in fixture_shadow_keys.iter().copied().enumerate() {
                    if fixture_shadow_dirty[index] {
                        self.fixture_shadow_cache[index] = Some(key);
                        self.fixture_shadow_drawn[index] = shadow_slots[index].map(|cone| {
                            (fixture_cones[cone].position, fixture_shadow_matrices[index])
                        });
                    }
                }
            }

            // A cascade's depth map only changes when its projection or its
            // casters do, exactly as for a fixture map. Redrawing all three
            // every frame was the sun path missing the check the fixture path
            // already had.
            let cascade_keys = cascade_cache_keys(&light_view_proj, depth_hash);
            if frame.directional.is_some_and(|light| light.shadows) {
                for (cascade, layer) in self.shadow_layers.iter().enumerate() {
                    if self.cascade_shadow_cache[cascade] == Some(cascade_keys[cascade]) {
                        continue;
                    }
                    self.cascade_shadow_cache[cascade] = Some(cascade_keys[cascade]);
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("shadow-cascade"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(depth_attachment(layer)),
                        timestamp_writes: pass_queries
                            .render("shadow-cascade", claim_start_timestamp(&mut pending_start)),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.shadow_pipeline);
                    pass.set_bind_group(0, &shadow_bgs[cascade].1, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_depth(&mut pass, false);
                }
            }

            if let Some(redraw) = &sky_field_redraw {
                for (index, (matrix, layer)) in
                    redraw.matrices.iter().zip(&redraw.layers).enumerate()
                {
                    let mut field_globals = globals;
                    field_globals.light_view_proj[0] = matrix.to_cols_array_2d();
                    let buffer = self.storage(
                        &mut encoder,
                        &[field_globals],
                        wgpu::BufferUsages::UNIFORM,
                        &format!("sky-height-globals-{index}"),
                    );
                    let bind_group =
                        self.scene_bind_group(&buffer, &instance_buf, false, false, &aerial);
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("sky-height"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(depth_attachment(layer)),
                        timestamp_writes: pass_queries
                            .render("sky-height", claim_start_timestamp(&mut pending_start)),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.shadow_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &self.gpu.white_material, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    pass.set_vertex_buffer(0, vertex_buf.slice(..));
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(start, end) in &redraw.runs {
                        let (first, last, base) = ranges[frame.draws[start].mesh];
                        pass.draw_indexed(first..last, base, start as u32..end as u32);
                    }
                }
            }
            self.sky_visibility.encode_field(
                &self.gpu.sky_visibility,
                &self.gpu.device,
                &mut encoder,
                &sky_height_buf,
                sun_direction,
                &mut pass_queries,
            );

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("depth-prepass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(depth_attachment(&depth_view)),
                    timestamp_writes: pass_queries
                        .render("depth-prepass", claim_start_timestamp(&mut pending_start)),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.depth_pipeline);
                pass.set_bind_group(0, &unlit_bg, &[]);
                pass.set_bind_group(2, &environment_bg, &[]);
                pass.set_bind_group(3, &cluster_bg, &[]);
                draw_depth(&mut pass, true);
            }

            if surface_depth_cull {
                let msaa_surface_depth = msaa_surface_depth
                    .as_ref()
                    .expect("allocated for surface culling");
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("surface-depth"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: msaa_surface_depth,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(depth_attachment_transient(&msaa_depth)),
                        timestamp_writes: pass_queries.render("surface-depth", None),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.scene_pipelines(budget.msaa).surface_depth);
                    pass.set_bind_group(0, &lit_bg, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_depth(&mut pass, true);
                }
                self.light_index.refine_surface(
                    &self.gpu.light_index_pipelines,
                    &self.gpu.device,
                    &mut encoder,
                    msaa_surface_depth,
                    budget.msaa > 1,
                    pass_queries.compute("surface-light-index", None),
                );
            }

            // Ambient visibility reads the depth prepass; the scene pass
            // reads it back per pixel through the cluster group.
            self.sky_visibility.encode_screen(
                &self.gpu.sky_visibility,
                &self.gpu.device,
                &mut encoder,
                &sky_height_buf,
                &ambient_params_buf,
                &depth_view,
                &mut pass_queries,
            );

            // --- sun shafts ------------------------------------------------------
            // How much of each ray's haze the sun reaches past the stage and
            // the clouds. Reads the depth prepass, the shadow cascades and
            // the cloud shadow map, all drawn above; the scene pass reads it.
            let inv_view_proj = view_proj.inverse();
            if shafts.is_some() {
                self.sun_shafts.encode(
                    &self.gpu.sun_shafts,
                    &self.gpu.device,
                    &mut encoder,
                    &crate::sun_shafts::FrameInput {
                        globals: &globals_buf,
                        shadow_map: &self.shadow_map,
                        shadow_sampler: &self.gpu.hard_shadow_sampler,
                        haze_field: (&self.gpu.haze_field.view, &self.gpu.haze_field.sampler),
                        aerial: &aerial,
                        depth: &depth_view,
                        inv_view_proj,
                        quality: frame.quality,
                    },
                    &mut pass_queries,
                );
            }

            // The local reflection probes (`probes.rs`): this frame's share
            // of any capture, then every probe relit and prefiltered, before
            // the scene pass samples them.
            if self.probes.live() {
                let capture = probe_work.capture.clone();
                if !capture.is_empty() {
                    let spheres: Vec<(Vec3, f32)> = frame.draws[..opaque]
                        .iter()
                        .map(|draw| world_sphere(draw, mesh_bounds[draw.mesh]))
                        .collect();
                    self.probes.encode_capture(
                        &self.gpu.probes,
                        &self.gpu.device,
                        &self.gpu.queue,
                        &mut encoder,
                        &instance_buf,
                        capture,
                        pass_queries.render("probe-capture", None),
                        |pass, at, face, texels| {
                            pass.set_vertex_buffer(0, vertex_buf.slice(..));
                            pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                            for (i, (centre, radius)) in spheres.iter().enumerate() {
                                if !crate::probes::face_sees(face, *centre - at, *radius, texels) {
                                    continue;
                                }
                                pass.set_bind_group(1, &materials[i], &[]);
                                let (first, last, base) = ranges[frame.draws[i].mesh];
                                pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                            }
                        },
                    );
                }
                if let Some(probe_clusters) = &probe_cluster_bg {
                    self.probes.encode_relight(
                        &self.gpu.probes,
                        &self.gpu.device,
                        &self.gpu.queue,
                        &mut encoder,
                        &lit_bg,
                        &probe_environment_bg,
                        probe_clusters,
                        pass_queries.compute("probe-relight", None),
                        pass_queries.compute("probe-filter", None),
                    );
                }
            }

            // The scene pass reads nothing the haze chain writes unless the
            // surface transmittance comes from the fog grid; then it is
            // encoded after `fog-transmittance` instead of here, still ahead
            // of the haze pass, so the legacy timestamp slots keep their order.
            // The haze's glow, from every cone's light past its shadow map
            // and the lasers', before the surfaces read it.
            self.haze_glow.encode(
                &self.gpu.haze_glow,
                &self.gpu.device,
                &mut encoder,
                &crate::haze_glow::FrameInput {
                    cores: &cores,
                    rests: &rests,
                    shadow_matrices: &fixture_shadow_matrix_buf,
                    shadow_map: &self.fixture_shadow_map,
                    shadow_map_extra: &self.fixture_shadow_map_extra,
                    medium,
                    haze_field: &self.gpu.haze_field,
                    lasers: &frame.lasers,
                    beam_gain: Transport::BEAM_GAIN,
                },
            );
            let samples = budget.msaa;
            let encode_scene = scene_encoder(|encoder, pass_queries, gpu, queries| {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("scene"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: msaa_color.as_ref().unwrap_or(&scene_view),
                        resolve_target: msaa_color.as_ref().map(|_| &scene_view),
                        depth_slice: None,
                        // Premultiplied radiance and coverage, including each
                        // surface's horizon fade. Transparent draws blend over
                        // them before the composite supplies the background.
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(depth_attachment_transient(&msaa_depth)),
                    timestamp_writes: pass_queries.render(
                        "scene",
                        queries.map(|queries| wgpu::RenderPassTimestampWrites {
                            query_set: queries,
                            beginning_of_pass_write_index: None,
                            end_of_pass_write_index: Some(1),
                        }),
                    ),
                    ..Default::default()
                });
                let pipelines = gpu.scene_pipelines(samples);
                pass.set_bind_group(0, &lit_bg, &[]);
                pass.set_bind_group(2, &environment_bg, &[]);
                pass.set_bind_group(3, &cluster_bg, &[]);
                if !floor_draws.is_empty() {
                    pass.set_pipeline(&pipelines.ground);
                    draw_range(&mut pass, &floor_draws);
                }
                pass.set_pipeline(&pipelines.scene);
                draw_range(&mut pass, &lit_draws);
                if let Some(balls) = probe_debug_balls {
                    pass.set_pipeline(&pipelines.probe_debug);
                    pass.set_bind_group(1, &gpu.white_material, &[]);
                    pass.draw(0..crate::probes::DEBUG_BALL_VERTICES, 0..balls);
                }
                for (slot, kind) in frame.transparent.iter().enumerate() {
                    pass.set_pipeline(match kind {
                        crate::frame::Transparent::Grid => &pipelines.grid,
                        crate::frame::Transparent::Compass => &pipelines.compass,
                        crate::frame::Transparent::Cables => &pipelines.cables,
                    });
                    draw_range(&mut pass, &transparent[slot..=slot]);
                }
            });
            if !scene_after_fog {
                encode_scene(
                    &mut encoder,
                    &mut pass_queries,
                    &self.gpu,
                    profile_resources.as_ref().map(|(queries, ..)| queries),
                );
            }

            // --- haze ------------------------------------------------------------
            // The light index was built (and its SoA uploaded) before the scene
            // passes; the haze passes bind the same frame's index as group 1.
            let subframes = subframes.max(1);
            // The compute kernels write each pixel once; the fragment pass
            // accumulates its subframes.
            let weight = if per_ray { 1.0 / subframes as f32 } else { 1.0 };
            let grid_prepare = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-prepare"),
                    layout: &self.gpu.fog_prepare_layout,
                    entries: &[binding(0, wgpu::BindingResource::TextureView(&fog_columns))],
                });
            let grid_classify = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-classify"),
                    layout: &self.gpu.fog_classify_layout,
                    entries: &[
                        binding(0, wgpu::BindingResource::TextureView(&fog_columns)),
                        binding(1, fog_candidates.as_entire_binding()),
                    ],
                });
            let grid_read = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-grid-read"),
                    layout: &self.gpu.fog_grid_read_layout,
                    entries: &[binding(
                        0,
                        wgpu::BindingResource::TextureView(&fog_integral),
                    )],
                });
            let grid_integrate = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-integrate"),
                    layout: &self.gpu.fog_integrate_layout,
                    entries: &[
                        binding(0, wgpu::BindingResource::TextureView(&fog_integral)),
                        binding(1, wgpu::BindingResource::TextureView(&fog_grid)),
                        binding(2, wgpu::BindingResource::TextureView(&fog_columns)),
                    ],
                });
            let grid_transmittance =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-transmittance"),
                        layout: &self.gpu.fog_tau_write_layout,
                        entries: &[
                            binding(0, wgpu::BindingResource::TextureView(&fog_transmittance)),
                            binding(1, wgpu::BindingResource::TextureView(&fog_grid)),
                            binding(2, wgpu::BindingResource::TextureView(&fog_columns)),
                            binding(3, wgpu::BindingResource::TextureView(&fog_tau)),
                        ],
                    });
            let grid_integrate_reuse =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-integrate-reuse"),
                        layout: &self.gpu.fog_tau_read_layout,
                        entries: &[
                            binding(0, wgpu::BindingResource::TextureView(&fog_integral)),
                            binding(1, wgpu::BindingResource::TextureView(&fog_grid)),
                            binding(2, wgpu::BindingResource::TextureView(&fog_columns)),
                            binding(3, wgpu::BindingResource::TextureView(&fog_tau)),
                        ],
                    });
            let grid_write = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-grid-write"),
                    layout: &self.gpu.fog_grid_write_layout,
                    entries: &[
                        binding(0, wgpu::BindingResource::TextureView(&fog_grid)),
                        binding(1, wgpu::BindingResource::TextureView(&fog_columns)),
                        binding(2, fog_candidates.as_entire_binding()),
                    ],
                });
            self.medium_cache
                .ensure(&self.gpu.device, fixture_cones.len());
            let medium_cache_group = if budget.marched
                && haze_density > 0.0
                && !fixture_cones.is_empty()
            {
                let cache_uniform = crate::medium::CacheUniform {
                    medium,
                    count: [fixture_cones.len() as u32, 0, 0, 0],
                };
                let buffer = self.storage(
                    &mut encoder,
                    &[cache_uniform],
                    wgpu::BufferUsages::UNIFORM,
                    "medium-cache-uniform",
                );
                Some(
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("medium-cache"),
                            layout: &self.gpu.medium_cache_layout,
                            entries: &[
                                binding(0, buffer.as_entire_binding()),
                                binding(1, index_bindings.core.as_entire_binding()),
                                binding(2, index_bindings.rest.as_entire_binding()),
                                binding(
                                    3,
                                    wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                                ),
                                binding(
                                    4,
                                    wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                                ),
                                binding(
                                    5,
                                    wgpu::BindingResource::TextureView(&self.medium_cache.view),
                                ),
                            ],
                        }),
                )
            } else {
                None
            };
            let medium_cache_dispatch = [
                medium.shape[3] as u32,
                medium.shape[3] as u32,
                fixture_cones.len() as u32,
            ];
            let encode_medium_cache = |encoder: &mut wgpu::CommandEncoder,
                                       pass_queries: &mut crate::pass_profile::PassQueries<'_>,
                                       gpu: &Gpu| {
                let Some(group) = medium_cache_group.as_ref() else {
                    return;
                };
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("medium-cache"),
                    timestamp_writes: pass_queries.compute("medium-cache", None),
                });
                pass.set_pipeline(&gpu.medium_cache_pipeline);
                pass.set_bind_group(0, group, &[]);
                pass.dispatch_workgroups(
                    medium_cache_dispatch[0],
                    medium_cache_dispatch[1],
                    medium_cache_dispatch[2],
                );
            };
            // Compute-lane order: on a grid-fed surface frame the medium cache
            // and the compaction classify are encoded after the scene pass.
            // Nothing ahead of the scene reads either — `fog-prepare` and
            // `fog-transmittance` bind the cache slot but never sample it, and
            // `fog-grid`, the residual and the hot kernel all follow the scene —
            // while ahead of `fog-prepare` they only delay the transmittance
            // prefix the scene's fragment stage waits on.
            let medium_cache_late = scene_after_fog && grid_fog;
            if !medium_cache_late {
                encode_medium_cache(&mut encoder, &mut pass_queries, &self.gpu);
            }

            let haze_passes = if per_ray && budget.marched {
                subframes
            } else {
                1
            };
            let (cache_uniform, cache_header, cache_claims, cache_table) = self
                .plan_interval_cache(
                    &mut encoder,
                    frame,
                    grid_fog && cached_shadows && fixture_shadows && !fixture_cones.is_empty(),
                    opaque,
                    &inv_view_proj,
                    camera_far,
                    (width, height),
                    haze_size,
                    &shadow_slots,
                    &fixture_cones,
                    &rests,
                );
            // Residual compaction needs a settled cache (some slot reading or
            // writing; a moving camera traverses every pair, which the fused
            // kernel does as fast), and list entries carry a 22-bit pixel
            // index.
            let cache = self.interval_cache.stats();
            let compact_plan = if grid_fog {
                // Shadow slot → this frame's light-index id, for the residual list.
                let source_to_sorted = self.light_index.source_to_sorted();
                let slot_lights: Vec<u32> = shadow_slots
                    .iter()
                    .map(|resident| {
                        resident
                            .and_then(|index| source_to_sorted.get(index).copied().flatten())
                            .unwrap_or(u32::MAX)
                    })
                    .collect();
                // A slot's classification depends on its interval-cache
                // generation (header and payload state) and on nothing else
                // that can change while that holds.
                let slot_words: Vec<u32> = self
                    .interval_cache_entries
                    .iter()
                    .map(|entry| {
                        if entry[3] & 0xFF == crate::interval_cache::MODE_OFF {
                            u32::MAX
                        } else {
                            entry[3] >> 8
                        }
                    })
                    .collect();
                self.compaction.plan(
                    &self.gpu.device,
                    &mut encoder,
                    cache.read + cache.write > 0
                        && u64::from(haze_size.0) * u64::from(haze_size.1) <= 1 << 22,
                    haze_size,
                    &slot_words,
                    &slot_lights,
                )
            } else {
                self.compaction.idle();
                None
            };
            let compact_pipes = &self.gpu.haze_compact_pipelines;
            let compact_uniform = compact_plan.as_ref().map(|plan| {
                self.storage(
                    &mut encoder,
                    &[plan.uniform],
                    wgpu::BufferUsages::UNIFORM,
                    "haze-compact-params",
                )
            });
            let compact_bind_group = |indirect: bool, hot: bool| -> wgpu::BindGroup {
                self.compaction.bind_group(
                    &self.gpu.device,
                    &compact_pipes.layout,
                    &crate::haze_compact::FrameBindings {
                        uniform: compact_uniform.as_ref().expect("compact frame"),
                        grid: &fog_integral,
                        static_field: &self.gpu.haze_field.view,
                        output: &haze_view,
                        cache: [&cache_uniform, &cache_header, &cache_claims, &cache_table],
                    },
                    indirect,
                    hot,
                )
            };
            let grid_read_transmittance =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-grid-read-transmittance"),
                        layout: &self.gpu.fog_grid_read_layout,
                        entries: &[binding(
                            0,
                            wgpu::BindingResource::TextureView(&fog_transmittance),
                        )],
                    });
            for k in 0..haze_passes {
                let uniform = HazeUniform {
                    medium,
                    inv_view_proj: inv_view_proj.to_cols_array_2d(),
                    camera_pos: frame.camera.eye.extend(1.0).to_array(),
                    params: [
                        fixture_cones.len() as f32,
                        haze_density,
                        frame.haze_steps as f32,
                        // Feeding track time makes the noise drift identical on
                        // every run (spec §6).
                        haze_time,
                    ],
                    tuning: [
                        (k & 4095) as f32,
                        weight,
                        Transport::NEAR_CLAMP,
                        Transport::BEAM_GAIN,
                    ],
                    transport: [
                        Transport::WHITE_LEAK,
                        Transport::PHASE_G,
                        haze_size.1 as f32,
                        haze_size.0 as f32,
                    ],
                    // The light index lives in full-resolution pixel space; xy
                    // scales this pass's fragment coordinate up to it.
                    tiles: [
                        width as f32 / haze_size.0 as f32,
                        height as f32 / haze_size.1 as f32,
                        0.0,
                        0.0,
                    ],
                    depth: [
                        CAMERA_NEAR,
                        camera_far,
                        haze_density * Transport::EXTINCTION,
                        0.0,
                    ],
                    shadow: [
                        fixture_shadow_count as f32,
                        1.0 / FIXTURE_SHADOW_SIZE as f32,
                        if self.visibility_reference
                            && (self.geometry_shadows || frame.geometry_shadows)
                            && fixture_shadows
                        {
                            REFERENCE_GEOMETRY_SHADOW_SAMPLES
                        } else {
                            0.0
                        },
                        fog_far,
                    ],
                    quality: budget.haze(),
                };
                // Indexed label: every subframe's upload lands before the one
                // submit, so one shared label would leave all of them reading the
                // last subframe's jitter seed.
                let haze_buf = self.storage(
                    &mut encoder,
                    &[uniform],
                    wgpu::BufferUsages::UNIFORM,
                    &format!("haze-{k}"),
                );
                let haze_bind_group = |buffer: &wgpu::Buffer| {
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze"),
                            layout: &self.gpu.haze_layout,
                            entries: &[
                                binding(0, buffer.as_entire_binding()),
                                binding(
                                    10,
                                    wgpu::BindingResource::TextureView(&self.medium_cache.view),
                                ),
                                binding(
                                    11,
                                    wgpu::BindingResource::TextureView(
                                        &self.shadow_hierarchy.views[0],
                                    ),
                                ),
                                binding(
                                    12,
                                    wgpu::BindingResource::TextureView(
                                        &self.shadow_hierarchy.views[1],
                                    ),
                                ),
                                binding(1, index_bindings.core.as_entire_binding()),
                                binding(2, index_bindings.rest.as_entire_binding()),
                                binding(3, wgpu::BindingResource::TextureView(&depth_view)),
                                binding(
                                    4,
                                    wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                                ),
                                binding(
                                    5,
                                    wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                                ),
                                binding(6, fixture_shadow_matrix_buf.as_entire_binding()),
                                binding(
                                    7,
                                    wgpu::BindingResource::TextureView(&self.fixture_shadow_map),
                                ),
                                binding(8, self.visibility.buffer.as_entire_binding()),
                                binding(
                                    9,
                                    wgpu::BindingResource::TextureView(
                                        &self.fixture_shadow_map_extra,
                                    ),
                                ),
                            ],
                        })
                };
                let bind_group = haze_bind_group(&haze_buf);
                if !budget.marched {
                    let output = self
                        .gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze-analytic"),
                            layout: &self.gpu.haze_analytic_layout,
                            entries: &[binding(0, wgpu::BindingResource::TextureView(&haze_view))],
                        });
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("haze-analytic"),
                        timestamp_writes: pass_queries.compute(
                            "haze-analytic",
                            profile_resources.as_ref().map(|(queries, ..)| {
                                wgpu::ComputePassTimestampWrites {
                                    query_set: queries,
                                    beginning_of_pass_write_index: None,
                                    end_of_pass_write_index: Some(2),
                                }
                            }),
                        ),
                    });
                    pass.set_pipeline(&self.gpu.haze_analytic_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &light_index_bg, &[]);
                    pass.set_bind_group(2, &output, &[]);
                    pass.dispatch_workgroups(haze_size.0.div_ceil(8), haze_size.1.div_ceil(8), 1);
                    continue;
                }
                // The fill and classify passes never sample the medium cache, but
                // a bind group that carries it would make them wait for the
                // medium-cache pass; this one binds the static density field in
                // its place so they can start as soon as the shadow maps, the
                // hierarchy and the light index are done.
                let bind_group_early =
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze-early"),
                            layout: &self.gpu.haze_layout,
                            entries: &[
                                binding(0, haze_buf.as_entire_binding()),
                                binding(
                                    10,
                                    wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                                ),
                                binding(
                                    11,
                                    wgpu::BindingResource::TextureView(
                                        &self.shadow_hierarchy.views[0],
                                    ),
                                ),
                                binding(
                                    12,
                                    wgpu::BindingResource::TextureView(
                                        &self.shadow_hierarchy.views[1],
                                    ),
                                ),
                                binding(1, index_bindings.core.as_entire_binding()),
                                binding(2, index_bindings.rest.as_entire_binding()),
                                binding(3, wgpu::BindingResource::TextureView(&depth_view)),
                                binding(
                                    4,
                                    wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                                ),
                                binding(
                                    5,
                                    wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                                ),
                                binding(6, fixture_shadow_matrix_buf.as_entire_binding()),
                                binding(
                                    7,
                                    wgpu::BindingResource::TextureView(&self.fixture_shadow_map),
                                ),
                                binding(8, self.visibility.buffer.as_entire_binding()),
                                binding(
                                    9,
                                    wgpu::BindingResource::TextureView(
                                        &self.fixture_shadow_map_extra,
                                    ),
                                ),
                            ],
                        });
                if grid_fog && k == 0 {
                    // Classify needs shadow maps, depth and the light index. It
                    // samples neither the medium cache nor the grid, so it
                    // overlaps the medium-cache pass and the scene, ahead of
                    // everything the residual needs.
                    let encode_classify = |encoder: &mut wgpu::CommandEncoder,
                                           pass_queries: &mut crate::pass_profile::PassQueries<
                        '_,
                    >| {
                        if compact_plan.as_ref().is_some_and(|plan| plan.classify) {
                            self.compaction.encode_classify(
                                encoder,
                                pass_queries,
                                compact_pipes,
                                &crate::haze_compact::Groups {
                                    haze: &bind_group_early,
                                    light_index: &light_index_bg,
                                    grid: &grid_read_transmittance,
                                },
                                &compact_bind_group(true, false),
                            );
                        }
                    };
                    // Encoded after the scene on a grid-fed surface frame: the
                    // classify writes only the interval cache and the residual
                    // planes/list, whose first readers are the residual and hot
                    // kernels behind `fog-integrate`.
                    if !scene_after_fog {
                        encode_classify(&mut encoder, &mut pass_queries);
                    }
                    // The prefix passes never sample the medium cache; the early
                    // group binds the static field in its slot so they do not wait
                    // for the medium-cache pass.
                    let prefix_group = &bind_group_early;
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("fog-grid"),
                        timestamp_writes: pass_queries.compute(
                            "fog-prepare",
                            profile_resources.as_ref().and_then(|(queries, ..)| {
                                (k == 0).then_some(wgpu::ComputePassTimestampWrites {
                                    query_set: queries,
                                    beginning_of_pass_write_index: Some(6),
                                    end_of_pass_write_index: Some(8),
                                })
                            }),
                        ),
                    });
                    pass.set_pipeline(&self.gpu.fog_prepare_pipeline);
                    pass.set_bind_group(0, prefix_group, &[]);
                    pass.set_bind_group(1, &light_index_bg, &[]);
                    pass.set_bind_group(2, &grid_prepare, &[]);
                    pass.dispatch_workgroups(
                        width.div_ceil(fog_tile).div_ceil(8),
                        height.div_ceil(fog_tile).div_ceil(8),
                        1,
                    );
                    drop(pass);
                    if scene_after_fog {
                        // Grid-sourced surface transmittance: the camera prefix
                        // needs only the columns, so the surface pass follows it
                        // here and overlaps the lit grid as it used to overlap the
                        // whole chain.
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("fog-transmittance"),
                            timestamp_writes: pass_queries.compute("fog-transmittance", None),
                        });
                        pass.set_pipeline(&self.gpu.fog_transmittance_pipeline);
                        pass.set_bind_group(0, prefix_group, &[]);
                        pass.set_bind_group(1, &light_index_bg, &[]);
                        pass.set_bind_group(2, &grid_transmittance, &[]);
                        pass.dispatch_workgroups(
                            width.div_ceil(fog_tile),
                            height.div_ceil(fog_tile),
                            1,
                        );
                        drop(pass);
                        encode_scene(
                            &mut encoder,
                            &mut pass_queries,
                            &self.gpu,
                            profile_resources.as_ref().map(|(queries, ..)| queries),
                        );
                        encode_classify(&mut encoder, &mut pass_queries);
                    }
                    if medium_cache_late {
                        encode_medium_cache(&mut encoder, &mut pass_queries, &self.gpu);
                    }
                    self.fog_blocks_valid = true;
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("fog-classify"),
                        timestamp_writes: pass_queries.compute("fog-classify", None),
                    });
                    pass.set_pipeline(&self.gpu.fog_classify_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &light_index_bg, &[]);
                    pass.set_bind_group(2, &grid_classify, &[]);
                    pass.dispatch_workgroups(
                        width
                            .div_ceil(fog_tile)
                            .div_ceil(crate::fog_grid::BLOCK_SIDE),
                        height
                            .div_ceil(fog_tile)
                            .div_ceil(crate::fog_grid::BLOCK_SIDE),
                        crate::fog_grid::SLICES.div_ceil(crate::fog_grid::BLOCK_SIDE),
                    );
                    drop(pass);
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("fog-grid"),
                        timestamp_writes: pass_queries.compute(
                            "fog-grid",
                            profile_resources.as_ref().map(|(queries, ..)| {
                                wgpu::ComputePassTimestampWrites {
                                    query_set: queries,
                                    beginning_of_pass_write_index: None,
                                    end_of_pass_write_index: Some(9),
                                }
                            }),
                        ),
                    });
                    pass.set_pipeline(&self.gpu.fog_grid_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &light_index_bg, &[]);
                    pass.set_bind_group(2, &grid_write, &[]);
                    pass.dispatch_workgroups(
                        width
                            .div_ceil(fog_tile)
                            .div_ceil(crate::fog_grid::BLOCK_SIDE),
                        height
                            .div_ceil(fog_tile)
                            .div_ceil(crate::fog_grid::BLOCK_SIDE),
                        crate::fog_grid::SLICES.div_ceil(crate::fog_grid::BLOCK_SIDE),
                    );
                    drop(pass);
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("fog-integrate"),
                        timestamp_writes: pass_queries.compute(
                            "fog-integrate",
                            profile_resources.as_ref().and_then(|(queries, ..)| {
                                (k == 0).then_some(wgpu::ComputePassTimestampWrites {
                                    query_set: queries,
                                    beginning_of_pass_write_index: None,
                                    end_of_pass_write_index: Some(7),
                                })
                            }),
                        ),
                    });
                    // `fog-transmittance` ran this frame iff the surface reads
                    // the grid; its per-slice τ then replaces the density taps.
                    if scene_after_fog {
                        pass.set_pipeline(&self.gpu.fog_integrate_reuse_pipeline);
                    } else {
                        pass.set_pipeline(&self.gpu.fog_integrate_pipeline);
                    }
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &light_index_bg, &[]);
                    pass.set_bind_group(
                        2,
                        if scene_after_fog {
                            &grid_integrate_reuse
                        } else {
                            &grid_integrate
                        },
                        &[],
                    );
                    pass.dispatch_workgroups(
                        width.div_ceil(fog_tile),
                        height.div_ceil(fog_tile),
                        1,
                    );
                    drop(pass);
                    if let Some(plan) = compact_plan.as_ref() {
                        self.compaction.encode_residual(
                            &mut encoder,
                            &mut pass_queries,
                            compact_pipes,
                            &crate::haze_compact::Groups {
                                haze: &bind_group,
                                light_index: &light_index_bg,
                                grid: &grid_read,
                            },
                            [
                                &compact_bind_group(true, false),
                                &compact_bind_group(false, false),
                            ],
                            plan.classify,
                        );
                    }
                }
                // Deterministic transport emits each pixel exactly once, as
                // direct stores; the fragment pass below accumulates the MIS
                // estimator's subframes.
                if !per_ray {
                    let output_entries = [
                        binding(0, wgpu::BindingResource::TextureView(&fog_integral)),
                        binding(1, wgpu::BindingResource::TextureView(&haze_view)),
                        binding(4, cache_uniform.as_entire_binding()),
                        binding(5, cache_header.as_entire_binding()),
                        binding(6, cache_claims.as_entire_binding()),
                        binding(7, cache_table.as_entire_binding()),
                    ];
                    let output = self
                        .gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze-compute"),
                            layout: &self.gpu.haze_compute_layout,
                            entries: &output_entries,
                        });
                    let groups = crate::haze_compact::Groups {
                        haze: &bind_group,
                        light_index: &light_index_bg,
                        grid: &grid_read,
                    };
                    let timestamp_writes = profile_resources.as_ref().map(|(queries, ..)| {
                        wgpu::ComputePassTimestampWrites {
                            query_set: queries,
                            beginning_of_pass_write_index: None,
                            end_of_pass_write_index: Some(2),
                        }
                    });
                    if let Some(plan) = compact_plan.as_ref() {
                        self.compaction.encode_output(
                            &mut encoder,
                            &mut pass_queries,
                            compact_pipes,
                            &groups,
                            [
                                &compact_bind_group(true, false),
                                &compact_bind_group(false, true),
                            ],
                            (&self.gpu.haze_compute_pipeline, &output),
                            timestamp_writes,
                            plan.readback,
                        );
                        continue;
                    }
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("haze-compute"),
                        timestamp_writes: pass_queries.compute("haze-compute", timestamp_writes),
                    });
                    pass.set_pipeline(&self.gpu.haze_compute_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &light_index_bg, &[]);
                    pass.set_bind_group(2, &output, &[]);
                    pass.set_bind_group(3, &grid_read, &[]);
                    pass.dispatch_workgroups(
                        haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
                        haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
                        1,
                    );
                    continue;
                }
                // The reference's subframes accumulate additively.
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("haze"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &haze_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: if k == 0 {
                                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                            } else {
                                wgpu::LoadOp::Load
                            },
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: pass_queries.render(
                        "haze",
                        profile_resources.as_ref().and_then(|(queries, ..)| {
                            (k + 1 == haze_passes).then_some(wgpu::RenderPassTimestampWrites {
                                query_set: queries,
                                beginning_of_pass_write_index: None,
                                end_of_pass_write_index: Some(2),
                            })
                        }),
                    ),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.haze_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_bind_group(1, &light_index_bg, &[]);
                pass.set_bind_group(2, &grid_read, &[]);
                pass.set_bind_group(3, &grid_read, &[]);
                pass.draw(0..3, 0..1);
                drop(pass);
            }

            // --- composite + readback --------------------------------------------
            let composite_uniform = CompositeUniform {
                medium,
                outdoor_sun,
                camera_pos: frame.camera.eye.extend(1.0).to_array(),
                inv_view_proj: inv_view_proj.to_cols_array_2d(),
                params: [
                    haze_size.0 as f32,
                    haze_size.1 as f32,
                    // Bilateral sigma in metres of linear view depth.
                    0.25,
                    frame.debug_view.shader_code() as f32,
                ],
                depth: [
                    CAMERA_NEAR,
                    camera_far,
                    haze_density * Transport::EXTINCTION,
                    Transport::PHASE_G,
                ],
                background: frame.clear_color.extend(1.0).to_array(),
                display: [
                    destination.headroom(),
                    f32::from(u8::from(shafts.is_some())),
                    0.0,
                    0.0,
                ],
            };
            let composite_buf = self.storage(
                &mut encoder,
                &[composite_uniform],
                wgpu::BufferUsages::UNIFORM,
                "composite",
            );
            let composite_glow = self.haze_glow.bindings(8);
            let bind_group = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("composite"),
                    layout: &self.gpu.composite_layout,
                    entries: &[
                        binding(0, composite_buf.as_entire_binding()),
                        binding(1, wgpu::BindingResource::TextureView(&scene_view)),
                        binding(2, wgpu::BindingResource::TextureView(&haze_view)),
                        binding(
                            5,
                            wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                        ),
                        binding(
                            6,
                            wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                        ),
                        binding(3, wgpu::BindingResource::Sampler(&self.gpu.linear_sampler)),
                        binding(4, wgpu::BindingResource::TextureView(&depth_view)),
                        binding(7, wgpu::BindingResource::TextureView(&shaft_view)),
                        composite_glow[0].clone(),
                        composite_glow[1].clone(),
                        composite_glow[2].clone(),
                    ],
                });
            {
                // The frame's last timestamp belongs to whichever pass writes
                // the output: the composite, or the post chain after it.
                let last_timestamp = profile_resources.as_ref().map(|(queries, ..)| {
                    wgpu::RenderPassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: Some(3),
                    }
                });
                // Diagnostic views are exact channel dumps; the camera stays
                // out of them.
                // Lasers draw into the post chain's HDR scene, so a frame with
                // any takes the chain even under a look that needs none.
                let post = ((frame.look.needs_post() || !frame.lasers.is_empty())
                    && frame.debug_view == crate::scene_desc::DebugView::Pbr
                    && !frame.cluster_debug)
                    .then(|| self.post.scene_target(&self.gpu.device, t_width, t_height));
                {
                    let (target, pipeline, timestamps) = match &post {
                        Some(scene) => (
                            scene,
                            &self.gpu.composite_linear_pipeline,
                            pass_queries.render("composite", None),
                        ),
                        None => (
                            &output_view,
                            &self.gpu.composite_pipelines[channels.index()],
                            pass_queries.render("composite", last_timestamp.clone()),
                        ),
                    };
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("composite"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: timestamps,
                        ..Default::default()
                    });
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &environment_bg, &[]);
                    pass.set_bind_group(2, &sky_bg, &[]);
                    pass.draw(0..3, 0..1);
                }
                // Lasers add their light to the scene before the camera sees
                // it, so glare blooms them as it does the beams.
                if let Some(scene) = &post {
                    self.gpu.laser.encode(
                        &self.gpu.device,
                        &mut encoder,
                        scene,
                        &depth_view,
                        &crate::laser::FrameInput {
                            scans: &frame.lasers,
                            view_proj,
                            eye: frame.camera.eye,
                            haze_density,
                            medium,
                            haze_field: &self.gpu.haze_field,
                            width: t_width,
                            height: t_height,
                        },
                    );
                }
                if post.is_some() {
                    self.post.encode(
                        &self.gpu.post,
                        &self.gpu.device,
                        &self.gpu.queue,
                        &mut encoder,
                        &crate::post::PostFrame {
                            look: frame.look,
                            quality: frame.quality,
                            cones: &frame.fixture_cones,
                            view_proj,
                            eye: frame.camera.eye,
                            fov_y_deg: frame.camera.fov_y_deg,
                            near: CAMERA_NEAR,
                            far: camera_far,
                            width: t_width,
                            height: t_height,
                            headroom: destination.headroom(),
                            output: channels.index(),
                            temporal,
                            time: frame.time,
                            // The lens's veil from a sun off the frame is
                            // the sun's light through the clouds, or under
                            // a deck the veil glows from a sun nobody sees.
                            sun: frame.sky.as_ref().map(|sky| {
                                let through = crate::atmosphere::clouds::sun_visibility(
                                    sky.clouds,
                                    frame.camera.eye,
                                    sky.sun_direction,
                                    frame.time,
                                );
                                (sky.sun_direction, sky.sun_radiance * through)
                            }),
                        },
                        &depth_view,
                        &output_view,
                        &mut pass_queries,
                        last_timestamp,
                    );
                }

                // Editor affordances are display UI, not scene radiance. Drawing
                // into the final sRGB target after AgX makes authored colours
                // independent of stage lighting and exposure. Cages load the
                // full-resolution reverse-Z prepass depth; free gizmos use Always.
                if !frame.overlays.is_empty() {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("editor-overlays"),
                        timestamp_writes: pass_queries.render("editor-overlays", None),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &output_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(depth_attachment_load(&depth_view)),
                        ..Default::default()
                    });
                    pass.set_bind_group(0, &overlay_bg, &[]);
                    pass.set_vertex_buffer(0, vertex_buf.slice(..));
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for (i, overlay) in frame.overlays.iter().enumerate() {
                        pass.set_pipeline(
                            &self.gpu.overlay_pipelines[overlay_pipeline_index(overlay, channels)],
                        );
                        let (first, last, base) = ranges[overlay.mesh];
                        pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                    }
                }
                if let Finish::Copy(output, readback) = &finish {
                    encoder.copy_texture_to_buffer(
                        output.as_image_copy(),
                        wgpu::TexelCopyBufferInfo {
                            buffer: readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(bytes_per_row),
                                rows_per_image: Some(t_height),
                            },
                        },
                        wgpu::Extent3d {
                            width: t_width,
                            height: t_height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                if let Some((queries, resolve, profile_readback, _)) = &profile_resources {
                    encoder.resolve_query_set(queries, 0..pass_queries.count, resolve, 0);
                    encoder.copy_buffer_to_buffer(
                        resolve,
                        0,
                        profile_readback,
                        0,
                        u64::from(pass_queries.count) * 8,
                    );
                }
            }

            let staging_started = Instant::now();
            self.staging
                .borrow_mut()
                .finish_and_recall_on_submit(&encoder);
            let finish_started = Instant::now();
            let commands = encoder.finish();
            let submit_started = Instant::now();
            self.gpu.queue.submit([commands]);
            if compact_plan.as_ref().is_some_and(|plan| plan.readback) {
                self.compaction.start_readback();
            }
            let submitted = Instant::now();
            let cpu_encode_submit = submitted - started;
            let cpu = CpuSpans {
                prepare: cluster_started - started,
                clusters: clusters_done - cluster_started,
                upload: targets_started - clusters_done,
                targets: targets_done - targets_started,
                encode: cpu_encode_submit - (targets_done - started),
                total: cpu_encode_submit,
                fixture_shadows: shadow_cpu,
                submission: SubmissionCpuSpans {
                    staging: finish_started - staging_started,
                    finish: submit_started - finish_started,
                    submit: submitted - submit_started,
                },
            };
            let completion = match finish {
                Finish::Copy(_, readback) => {
                    let (mapped_tx, mapped) = mpsc::sync_channel(1);
                    readback
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            let _ = mapped_tx
                                .send((Instant::now(), result.map_err(|error| error.to_string())));
                        });
                    Completion::Staged {
                        readback,
                        mapped,
                        mapped_result: None,
                        bytes_per_row,
                    }
                }
                // Without a readback there is no map to wait on, so the queue itself
                // has to say when the surface is safe to sample. On Metal this is
                // the only fence between the two devices; on a shared device it is
                // only the completion stamp.
                Finish::Share(surface) => {
                    let (done_tx, done) = mpsc::sync_channel(1);
                    // Stamped in the callback, not where it is noticed: the whole
                    // question is how much of `draw_time` is the GPU finishing and
                    // how much is the worker getting round to looking.
                    self.gpu.queue.on_submitted_work_done(move || {
                        let _ = done_tx.send(Instant::now());
                    });
                    Completion::Shared {
                        surface,
                        done,
                        finished: false,
                    }
                }
            };
            let query_count = pass_queries.count;
            let passes = pass_queries.spans;
            let moved = pass_queries.moved;
            let pending_profile =
                profile_resources.map(|(query_set, resolve, readback, timestamp_period_ns)| {
                    PendingProfile {
                        device: self.gpu.device.clone(),
                        queue: self.gpu.queue.clone(),
                        query_set,
                        resolve,
                        readback,
                        mapped: None,
                        mapped_result: None,
                        timestamp_period_ns,
                        cpu_encode_submit,
                        cpu_cluster,
                        strict_timestamps: true,
                        grid_fog,
                        query_count,
                        passes,
                        moved,
                    }
                });
            PendingFrame {
                completion,
                width: t_width,
                height: t_height,
                started,
                profile: pending_profile,
                shadows: self.shadow_stats,
                clusters: self.light_index.stats(),
                // Overwritten by `submit_live`, which is the only caller that has
                // a queue to have waited in; the profiler submits directly.
                queued: Duration::ZERO,
                cpu,
                signalled: None,
            }
        }
    }

    fn scene_bind_group(
        &self,
        globals: &wgpu::Buffer,
        instances: &wgpu::Buffer,
        shadows: bool,
        hard_shadows: bool,
        aerial: &crate::atmosphere::AerialTextures,
    ) -> wgpu::BindGroup {
        // The shadow map is a render target during the shadow pass, so the
        // passes that write depth bind a 1x1 placeholder instead.
        let map = if shadows {
            &self.shadow_map
        } else {
            &self.gpu.dummy_shadow
        };
        let mut entries = vec![
            binding(0, globals.as_entire_binding()),
            binding(1, instances.as_entire_binding()),
            binding(3, wgpu::BindingResource::TextureView(map)),
            binding(
                4,
                wgpu::BindingResource::Sampler(if hard_shadows {
                    &self.gpu.hard_shadow_sampler
                } else {
                    &self.gpu.shadow_sampler
                }),
            ),
        ];
        entries.extend(aerial.entries());
        entries.extend([
            binding(
                10,
                wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
            ),
            binding(
                11,
                wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
            ),
        ]);
        self.gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scene"),
                layout: &self.gpu.scene_layout,
                entries: &entries,
            })
    }
}

fn sanitize_fixture_cone(light: &crate::frame::FixtureCone) -> crate::frame::FixtureCone {
    let finite = |value: f32, fallback: f32| value.is_finite().then_some(value).unwrap_or(fallback);
    let finite_vec =
        |value: Vec3, fallback: Vec3| value.is_finite().then_some(value).unwrap_or(fallback);
    let cos_beam = finite(light.cos_beam, 0.95).clamp(0.01, 1.0);
    let cos_field = finite(light.cos_field, cos_beam)
        .clamp(0.01, 1.0)
        .min(cos_beam);
    crate::frame::FixtureCone {
        position: finite_vec(light.position, Vec3::ZERO)
            .clamp(Vec3::splat(-10_000.0), Vec3::splat(10_000.0)),
        range: finite(light.range, 0.05).clamp(0.05, 100.0),
        direction: finite_vec(light.direction, Vec3::NEG_Y)
            .try_normalize()
            .unwrap_or(Vec3::NEG_Y),
        cos_beam,
        color: finite_vec(light.color, Vec3::ZERO).clamp(Vec3::ZERO, Vec3::splat(100.0)),
        intensity: finite(light.intensity, 0.0).clamp(0.0, 100.0),
        cos_field,
        wash: finite(light.wash, 0.0).clamp(0.0, 1.0),
        gobo: light.gobo.min(2),
        gobo_rotation: finite(light.gobo_rotation, 0.0).rem_euclid(std::f32::consts::TAU),
        haze_gain: finite(light.haze_gain, 1.0).clamp(0.0, 1.0),
        lens: luma_scene::luminaire::Lens {
            radius: finite(light.lens.radius, 0.0)
                .clamp(0.0, luma_scene::luminaire::Lens::MAX_RADIUS_M),
        },
    }
}

/// Pins the scene-encoding closure's signature so its query-set argument and
/// the pass profiler share one lifetime; closure parameters cannot name one.
fn scene_encoder<'q, F>(encode: F) -> F
where
    F: Fn(
        &mut wgpu::CommandEncoder,
        &mut crate::pass_profile::PassQueries<'q>,
        &Gpu,
        Option<&'q wgpu::QuerySet>,
    ),
{
    encode
}
