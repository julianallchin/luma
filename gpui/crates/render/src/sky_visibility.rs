//! Ambient visibility: how much of the sky and of the lit ground a surface
//! point sees, and the screen-space occlusion around it.
//!
//! Three estimates end up in one per-pixel texture the scene pass reads
//! (`shaders/ambient_occlusion.wgsl` documents its channels):
//!
//! - **Height field** (outdoors). The stage seen from straight above and from
//!   straight below, over its own footprint, gives one slab per texel. It is
//!   rendered only when the stage geometry changes, keyed like the fixture
//!   shadow maps: fixture bodies are left out, so moving heads never redraw it.
//!   Texel size is the stage footprint over [`MAX_FIELD_TEXELS`], but never
//!   finer than [`MIN_FIELD_TEXEL_M`]: 5 cm up to a 51 m stage, 10 cm for a
//!   100 m one.
//! - **Ground light** (outdoors). Sun and sky visibility of the bare ground
//!   under the stage, at twice the height field's texel, box-filtered into
//!   mips. Recomputed when the field or the sun changes.
//! - **GTAO** (always). On the single-sample depth prepass, every frame.
//!
//! What this does not see: the ground plane is assumed to be `z = 0`, a slab
//! is one span per texel (a second deck under the first reads as solid), and
//! geometry outside the stage footprint (the venue's own terrain) is open sky.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::frame::{EditorObject, Frame};

/// Largest height-field side, in texels.
pub(crate) const MAX_FIELD_TEXELS: f32 = 1024.0;
/// Finest height-field texel, in metres.
pub(crate) const MIN_FIELD_TEXEL_M: f32 = 0.05;

const HEIGHT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
const VISIBILITY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct HeightParams {
    origin: [f32; 4],
    size: [f32; 4],
    sun: [f32; 4],
    march: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct AoParams {
    camera: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    forward: [f32; 4],
    viewport: [f32; 4],
}

/// The stage slab's placement over the ground, derived from its geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FieldLayout {
    /// World XY of the minimum corner.
    pub(crate) origin: [f32; 2],
    /// Texel size in metres.
    pub(crate) texel: f32,
    /// Resolution in texels.
    pub(crate) size: [u32; 2],
    /// Top of the depth range in metres; the bottom is the ground.
    pub(crate) top: f32,
}

impl FieldLayout {
    /// Fit the field to world-space bounds `(min, max)` of the stage, with a
    /// margin so the ground just past the edge still sees it.
    pub(crate) fn fit(min: Vec3, max: Vec3) -> Option<Self> {
        if !(min.is_finite() && max.is_finite()) || max.z <= 0.0 {
            return None;
        }
        let span = (max - min).truncate();
        let margin = 2.0 + 0.1 * span.max_element();
        let lo = min.truncate() - margin;
        let extent = span + 2.0 * margin;
        let texel = (extent.max_element() / MAX_FIELD_TEXELS).max(MIN_FIELD_TEXEL_M);
        let size = (extent / texel)
            .ceil()
            .as_uvec2()
            .min(glam::UVec2::splat(MAX_FIELD_TEXELS as u32));
        Some(Self {
            origin: lo.to_array(),
            texel,
            size: size.to_array(),
            top: max.z + 0.5,
        })
    }

    /// Orthographic projections for the two height maps. Row `j` of the map is
    /// world `y = origin.y + (j + 0.5) * texel`, the same indexing both passes
    /// and the shaders use. Reverse-Z like every depth target here: `above`
    /// keeps the highest surface, `below` the lowest.
    pub(crate) fn matrices(&self) -> [Mat4; 2] {
        let half = glam::Vec2::new(self.size[0] as f32, self.size[1] as f32) * self.texel * 0.5;
        let centre = glam::Vec2::from(self.origin) + half;
        let plane = |scale: f32, offset: f32| {
            Mat4::from_cols(
                Vec4::new(1.0 / half.x, 0.0, 0.0, 0.0),
                Vec4::new(0.0, -1.0 / half.y, 0.0, 0.0),
                Vec4::new(0.0, 0.0, scale, 0.0),
                Vec4::new(-centre.x / half.x, centre.y / half.y, offset, 1.0),
            )
        };
        [plane(1.0 / self.top, 0.0), plane(-1.0 / self.top, 1.0)]
    }

    fn ground_size(&self) -> [u32; 2] {
        self.size.map(|side| side.div_ceil(2).max(1))
    }

    fn ground_mips(&self) -> u32 {
        let [w, h] = self.ground_size();
        32 - w.max(h).leading_zeros()
    }

    fn params(&self, sun: Option<Vec3>) -> HeightParams {
        let extent = self.size[0].max(self.size[1]) as f32 * self.texel;
        // Twelve radii from two texels out to the far side of the stage.
        let first = 2.0 * self.texel;
        let reach = extent.max(10.0);
        HeightParams {
            origin: [self.origin[0], self.origin[1], self.texel, self.top],
            size: [
                self.size[0] as f32,
                self.size[1] as f32,
                2.0 * self.texel,
                self.ground_mips() as f32,
            ],
            sun: sun.map_or([0.0; 4], |sun| sun.extend(1.0).to_array()),
            march: [first, (reach / first).powf(1.0 / 11.0), 0.02, 0.0],
        }
    }
}

/// Whether `draw` stands in the height field: stage geometry, not the ground
/// plane (the field's own `z = 0`) and not fixture bodies, which move every
/// cue and are too small to shade a sky.
pub(crate) fn in_field(frame: &Frame, index: usize) -> bool {
    let draw = &frame.draws[index];
    let key = frame.meshes[draw.mesh].key.as_str();
    !matches!(draw.editor_object, Some(EditorObject::Fixture(_)))
        && key != "::floor"
        && key != "::outdoor-floor"
}

/// Consecutive runs `(start, end)` of opaque draws of one mesh that stand in
/// the field, and the world bounds they cover.
pub(crate) fn field_draws(
    frame: &Frame,
    opaque: usize,
    mesh_bounds: &[(Vec3, f32)],
) -> (Vec<(usize, usize)>, Vec3, Vec3) {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for index in 0..opaque {
        if !in_field(frame, index) {
            continue;
        }
        let draw = &frame.draws[index];
        let (centre, radius) = mesh_bounds[draw.mesh];
        let scale = [draw.model.x_axis, draw.model.y_axis, draw.model.z_axis]
            .map(|axis| axis.truncate().length())
            .into_iter()
            .fold(0.0_f32, f32::max);
        let world = draw.model.transform_point3(centre);
        let reach = Vec3::splat(radius * scale);
        min = min.min(world - reach);
        max = max.max(world + reach);
        match runs.last_mut() {
            Some((_, end)) if *end == index && frame.draws[index - 1].mesh == draw.mesh => {
                *end = index + 1;
            }
            _ => runs.push((index, index + 1)),
        }
    }
    (runs, min, max)
}

/// Device-level pipelines, the sampler and the placeholders bound when there
/// is no height field.
pub(crate) struct Pipelines {
    convert_layout: wgpu::BindGroupLayout,
    ground_layout: wgpu::BindGroupLayout,
    mip_layout: wgpu::BindGroupLayout,
    visibility_layout: wgpu::BindGroupLayout,
    denoise_layout: wgpu::BindGroupLayout,
    convert: wgpu::ComputePipeline,
    ground: wgpu::ComputePipeline,
    mip: wgpu::ComputePipeline,
    visibility: wgpu::ComputePipeline,
    denoise: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    empty_heights: wgpu::TextureView,
    empty_ground: wgpu::TextureView,
}

fn entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

fn uniform(binding: u32) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
    )
}

fn texture(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
    view_dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::Texture {
            sample_type,
            view_dimension,
            multisampled: false,
        },
    )
}

fn storage(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
    )
}

fn view(resource: &wgpu::TextureView) -> wgpu::BindingResource<'_> {
    wgpu::BindingResource::TextureView(resource)
}

fn bind(index: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding: index,
        resource,
    }
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        use wgpu::TextureSampleType as Sample;
        use wgpu::TextureViewDimension as Dim;
        let heights = || texture(1, Sample::Float { filterable: false }, Dim::D2);
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let convert_layout = layout(
            "sky-height-convert",
            &[
                uniform(0),
                texture(2, Sample::Depth, Dim::D2Array),
                storage(3, HEIGHT_FORMAT),
            ],
        );
        let ground_layout = layout(
            "sky-ground",
            &[uniform(0), heights(), storage(4, VISIBILITY_FORMAT)],
        );
        let mip_layout = layout(
            "sky-ground-mip",
            &[
                storage(4, VISIBILITY_FORMAT),
                texture(5, Sample::Float { filterable: true }, Dim::D2),
            ],
        );
        let visibility_layout = layout(
            "ambient-visibility",
            &[
                uniform(0),
                heights(),
                uniform(2),
                texture(3, Sample::Depth, Dim::D2),
                texture(4, Sample::Float { filterable: true }, Dim::D2),
                entry(
                    5,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                storage(6, VISIBILITY_FORMAT),
            ],
        );
        let denoise_layout = layout(
            "ambient-denoise",
            &[
                uniform(2),
                texture(3, Sample::Depth, Dim::D2),
                texture(7, Sample::Float { filterable: false }, Dim::D2),
                storage(8, VISIBILITY_FORMAT),
            ],
        );
        let common = include_str!("shaders/sky_height.wgsl");
        let ground_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky-ground"),
            source: wgpu::ShaderSource::Wgsl(
                format!("{common}{}", include_str!("shaders/sky_ground.wgsl")).into(),
            ),
        });
        let ao_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ambient-occlusion"),
            source: wgpu::ShaderSource::Wgsl(
                format!("{common}{}", include_str!("shaders/ambient_occlusion.wgsl")).into(),
            ),
        });
        let pipeline = |label, layout: &wgpu::BindGroupLayout, module, entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[Some(layout)],
                        immediate_size: 0,
                    }),
                ),
                module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let convert = pipeline(
            "sky-height-convert",
            &convert_layout,
            &ground_module,
            "convert",
        );
        let ground = pipeline("sky-ground", &ground_layout, &ground_module, "ground");
        let mip = pipeline("sky-ground-mip", &mip_layout, &ground_module, "mip");
        let visibility = pipeline(
            "ambient-visibility",
            &visibility_layout,
            &ao_module,
            "visibility",
        );
        let denoise = pipeline("ambient-denoise", &denoise_layout, &ao_module, "denoise");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sky-ground"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let one_texel = |label, format, bytes: &[u8]| {
            device
                .create_texture_with_data(
                    queue,
                    &wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    bytes,
                )
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let open = bytemuck::cast_slice::<f32, u8>(&[0.0, 1.0e9]).to_vec();
        Self {
            empty_heights: one_texel("sky-heights-empty", HEIGHT_FORMAT, &open),
            empty_ground: one_texel("sky-ground-empty", VISIBILITY_FORMAT, &[255; 4]),
            convert_layout,
            ground_layout,
            mip_layout,
            visibility_layout,
            denoise_layout,
            convert,
            ground,
            mip,
            visibility,
            denoise,
            sampler,
        }
    }
}

struct Field {
    caster_hash: u64,
    layout: FieldLayout,
    /// The two height maps' render layers and the array the convert pass reads.
    depth_layers: [wgpu::TextureView; 2],
    depth_array: wgpu::TextureView,
    heights: wgpu::TextureView,
    ground: wgpu::TextureView,
    ground_levels: Vec<wgpu::TextureView>,
    /// Sun the ground map was computed for (all ones for no sun); `None`
    /// until it has been.
    ground_sun: Option<[u32; 3]>,
}

struct Screen {
    size: [u32; 2],
    raw: wgpu::TextureView,
    output: wgpu::TextureView,
}

/// The height-map passes one frame must encode before the depth prepass.
pub(crate) struct FieldRedraw {
    pub(crate) matrices: [Mat4; 2],
    pub(crate) layers: [wgpu::TextureView; 2],
    pub(crate) runs: Vec<(usize, usize)>,
}

/// One renderer's resident height field and per-pixel targets.
#[derive(Default)]
pub(crate) struct SkyVisibility {
    field: Option<Field>,
    screen: Option<Screen>,
    /// The field needs its height maps converted after this frame's redraw.
    converted: bool,
}

impl SkyVisibility {
    /// Keep the height field in step with the frame. Returns the height-map
    /// passes to encode when the stage geometry changed; nothing otherwise.
    /// `outdoor` false drops the field: indoors there is no sky to occlude.
    pub(crate) fn update_field(
        &mut self,
        device: &wgpu::Device,
        frame: &Frame,
        opaque: usize,
        caster_hash: u64,
        mesh_bounds: &[(Vec3, f32)],
        outdoor: bool,
    ) -> Option<FieldRedraw> {
        if !outdoor {
            self.field = None;
            return None;
        }
        if self
            .field
            .as_ref()
            .is_some_and(|field| field.caster_hash == caster_hash)
        {
            return None;
        }
        let (runs, min, max) = field_draws(frame, opaque, mesh_bounds);
        let Some(layout) = FieldLayout::fit(min, max) else {
            self.field = None;
            return None;
        };
        let reuse = self
            .field
            .take()
            .filter(|field| field.layout.size == layout.size);
        let field = match reuse {
            Some(field) => Field {
                caster_hash,
                layout,
                ground_sun: None,
                ..field
            },
            None => allocate(device, caster_hash, layout),
        };
        let layers = field.depth_layers.clone();
        self.field = Some(field);
        self.converted = false;
        Some(FieldRedraw {
            matrices: layout.matrices(),
            layers,
            runs,
        })
    }

    /// Whether this frame's surfaces read the height field.
    pub(crate) fn active(&self) -> bool {
        self.field.is_some()
    }

    /// Uniform for the field passes and the per-pixel pass.
    pub(crate) fn height_params(&self, sun: Option<Vec3>) -> HeightParams {
        self.field
            .as_ref()
            .map_or_else(HeightParams::default, |field| field.layout.params(sun))
    }

    /// Size the per-pixel targets and return the one the scene pass reads.
    pub(crate) fn output(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> wgpu::TextureView {
        if self
            .screen
            .as_ref()
            .is_none_or(|screen| screen.size != [width, height])
        {
            let target = |label| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: VISIBILITY_FORMAT,
                        usage: wgpu::TextureUsages::STORAGE_BINDING
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            self.screen = Some(Screen {
                size: [width, height],
                raw: target("ambient-visibility-raw"),
                output: target("ambient-visibility"),
            });
        }
        self.screen.as_ref().expect("just sized").output.clone()
    }

    /// Encode the field's compute work that this frame needs: converting new
    /// height maps, and the ground map when the field or the sun moved.
    pub(crate) fn encode_field(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        params: &wgpu::Buffer,
        sun: Option<Vec3>,
        pass_queries: &mut crate::pass_profile::PassQueries<'_>,
    ) {
        let converted = std::mem::replace(&mut self.converted, true);
        let Some(field) = self.field.as_mut() else {
            return;
        };
        let sun_key = sun.map_or([u32::MAX; 3], |sun| sun.to_array().map(f32::to_bits));
        let [w, h] = field.layout.size;
        if !converted {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky-height-convert"),
                layout: &pipelines.convert_layout,
                entries: &[
                    bind(0, params.as_entire_binding()),
                    bind(2, view(&field.depth_array)),
                    bind(3, view(&field.heights)),
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sky-height-convert"),
                timestamp_writes: pass_queries.compute("sky-height-convert", None),
            });
            pass.set_pipeline(&pipelines.convert);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
        if field.ground_sun == Some(sun_key) {
            return;
        }
        field.ground_sun = Some(sun_key);
        let [gw, gh] = field.layout.ground_size();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky-ground"),
            layout: &pipelines.ground_layout,
            entries: &[
                bind(0, params.as_entire_binding()),
                bind(1, view(&field.heights)),
                bind(4, view(&field.ground_levels[0])),
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("sky-ground"),
            timestamp_writes: pass_queries.compute("sky-ground", None),
        });
        pass.set_pipeline(&pipelines.ground);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(gw.div_ceil(8), gh.div_ceil(8), 1);
        pass.set_pipeline(&pipelines.mip);
        for level in 1..field.ground_levels.len() {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky-ground-mip"),
                layout: &pipelines.mip_layout,
                entries: &[
                    bind(4, view(&field.ground_levels[level])),
                    bind(5, view(&field.ground_levels[level - 1])),
                ],
            });
            pass.set_bind_group(0, &group, &[]);
            let (lw, lh) = ((gw >> level).max(1), (gh >> level).max(1));
            pass.dispatch_workgroups(lw.div_ceil(8), lh.div_ceil(8), 1);
        }
    }

    /// Encode the per-pixel visibility and its denoise, reading `depth`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_screen(
        &self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        params: &wgpu::Buffer,
        ao_params: &wgpu::Buffer,
        depth: &wgpu::TextureView,
        pass_queries: &mut crate::pass_profile::PassQueries<'_>,
    ) {
        let screen = self.screen.as_ref().expect("sized before encoding");
        let (heights, ground) = self.field.as_ref().map_or(
            (&pipelines.empty_heights, &pipelines.empty_ground),
            |field| (&field.heights, &field.ground),
        );
        let visibility = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ambient-visibility"),
            layout: &pipelines.visibility_layout,
            entries: &[
                bind(0, params.as_entire_binding()),
                bind(1, view(heights)),
                bind(2, ao_params.as_entire_binding()),
                bind(3, view(depth)),
                bind(4, view(ground)),
                bind(5, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                bind(6, view(&screen.raw)),
            ],
        });
        let denoise = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ambient-denoise"),
            layout: &pipelines.denoise_layout,
            entries: &[
                bind(2, ao_params.as_entire_binding()),
                bind(3, view(depth)),
                bind(7, view(&screen.raw)),
                bind(8, view(&screen.output)),
            ],
        });
        let [w, h] = screen.size;
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ambient-visibility"),
                timestamp_writes: pass_queries.compute("ambient-visibility", None),
            });
            pass.set_pipeline(&pipelines.visibility);
            pass.set_bind_group(0, &visibility, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("ambient-denoise"),
            timestamp_writes: pass_queries.compute("ambient-denoise", None),
        });
        pass.set_pipeline(&pipelines.denoise);
        pass.set_bind_group(0, &denoise, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }
}

/// Per-pixel pass uniform for a camera at `eye` looking along `forward`.
pub(crate) fn ao_params(
    eye: Vec3,
    forward: Vec3,
    fov_y: f32,
    size: [u32; 2],
    planes: [f32; 2],
    outdoor: bool,
) -> AoParams {
    let right = forward.cross(Vec3::Z).normalize_or(Vec3::X);
    let up = right.cross(forward).normalize_or(Vec3::Z);
    let tan_y = (fov_y * 0.5).tan();
    let tan_x = tan_y * size[0] as f32 / size[1] as f32;
    AoParams {
        camera: eye.extend(0.5 * size[1] as f32 / tan_y).to_array(),
        right: right.extend(tan_x).to_array(),
        up: up.extend(tan_y).to_array(),
        forward: forward.extend(f32::from(u8::from(outdoor))).to_array(),
        viewport: [size[0] as f32, size[1] as f32, planes[0], planes[1]],
    }
}

fn allocate(device: &wgpu::Device, caster_hash: u64, layout: FieldLayout) -> Field {
    let [w, h] = layout.size;
    let extent = |layers| wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: layers,
    };
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sky-height-depth"),
        size: extent(2),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: crate::gpu::DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let depth_layers = [0, 1].map(|layer| {
        depth.create_view(&wgpu::TextureViewDescriptor {
            label: Some("sky-height-layer"),
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    });
    let depth_array = depth.create_view(&wgpu::TextureViewDescriptor {
        label: Some("sky-height-depth"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let heights = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("sky-heights"),
            size: extent(1),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HEIGHT_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default());
    let [gw, gh] = layout.ground_size();
    let mips = layout.ground_mips();
    let ground = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sky-ground"),
        size: wgpu::Extent3d {
            width: gw,
            height: gh,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: VISIBILITY_FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let ground_levels = (0..mips)
        .map(|level| {
            ground.create_view(&wgpu::TextureViewDescriptor {
                label: Some("sky-ground-level"),
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect();
    Field {
        caster_hash,
        layout,
        depth_layers,
        depth_array,
        heights,
        ground: ground.create_view(&wgpu::TextureViewDescriptor::default()),
        ground_levels,
        ground_sun: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_texel_follows_the_stage_footprint() {
        let small = FieldLayout::fit(Vec3::new(-4.0, -2.0, 0.0), Vec3::new(8.0, 2.0, 2.0))
            .expect("a stage above the ground has a field");
        assert!((small.texel - MIN_FIELD_TEXEL_M).abs() < 1e-6);
        let large = FieldLayout::fit(Vec3::new(-50.0, -50.0, 0.0), Vec3::new(50.0, 50.0, 12.0))
            .expect("a stage above the ground has a field");
        assert!(large.size[0] <= MAX_FIELD_TEXELS as u32);
        assert!(large.texel > 0.1 && large.texel < 0.13, "{}", large.texel);
        assert!(FieldLayout::fit(Vec3::ZERO, Vec3::new(1.0, 1.0, -0.5)).is_none());
    }

    #[test]
    fn height_matrices_index_rows_by_world_y_and_keep_both_extremes() {
        let layout = FieldLayout {
            origin: [-10.0, 5.0],
            texel: 0.5,
            size: [40, 20],
            top: 4.0,
        };
        let [above, below] = layout.matrices();
        // Texel (i, j) centre maps to its own NDC centre in both maps.
        let world = Vec3::new(-10.0 + 3.5 * 0.5, 5.0 + 7.5 * 0.5, 1.0);
        for matrix in [above, below] {
            let clip = matrix * world.extend(1.0);
            let pixel = ((clip.x * 0.5 + 0.5) * 40.0, (0.5 - clip.y * 0.5) * 20.0);
            assert!(
                (pixel.0 - 3.5).abs() < 1e-4 && (pixel.1 - 7.5).abs() < 1e-4,
                "{pixel:?}"
            );
        }
        // Reverse-Z with a Greater test: `above` keeps the highest surface,
        // `below` the lowest, and each clears to the empty value zero.
        let depth = |matrix: Mat4, z: f32| (matrix * Vec3::new(0.0, 10.0, z).extend(1.0)).z;
        assert!(depth(above, 3.0) > depth(above, 1.0));
        assert!(depth(below, 1.0) > depth(below, 3.0));
        assert!(depth(above, 0.0).abs() < 1e-6 && depth(below, 4.0).abs() < 1e-6);
    }
}
