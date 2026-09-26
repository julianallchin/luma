//! Relightable reflection probes: a grid of cube maps over the stage and the
//! audience that shiny and matte surfaces take their surroundings from.
//!
//! The sky probe (`environment.rs`) knows the sky and the floor's mean
//! colour, and nothing nearer. A galvanised barrier on dirt reflected grey
//! air, and a truss reflected nothing of the lit deck under it. These probes
//! see the stage, and the way Call of Duty and Frostbite relight theirs for
//! a moving sun, they keep what the stage *is* and not how it is lit:
//!
//! - **Placement** ([`place`]): a grid over the bounds of the stage's
//!   geometry: 12 probes to a layer and two layers on High, 8 on one on Low, and
//!   a parallax box round it all, which indoors is the room.
//! - **Capture** (`probe_capture.wgsl`): each face as albedo, roughness,
//!   normal, metal and distance, into one atlas. Only when the stage's
//!   geometry, its materials, the floor or the sky changes ([`key`]), and a
//!   probe a frame, so a new layout never stalls a frame.
//! - **Relight** (`probe_relight.wgsl`): every texel every frame, by the
//!   frame's sun, sky, clouds and fixtures, through the scene pass's own
//!   functions. A fixture that turns red is red in the probes that frame.
//!   A texel holds what the stage changes of the sky probe's light that way:
//!   the stage's own light less the sky it hides, and nothing where the
//!   probe sees open sky or open ground.
//! - **Prefilter** (`probe_filter.wgsl`): the roughness mips, from the base
//!   down, eight samples a texel.
//! - **Sample** (`probe_sample.wgsl`): the scene pass blends the probes
//!   round a point, parallax-corrected, and adds their change to the sky
//!   probe's specular and diffuse light. Open ground takes the sky probe's
//!   light alone, inside the grid as past it, so the grid has no edge.
//!
//! Beams and haze stay out: the probes hold surfaces, and the air is drawn
//! over the frame after them.

use bytemuck::{Pod, Zeroable};
use glam::{UVec3, Vec3};

use crate::frame::{Draw, EditorObject, Frame};
use crate::scene_desc::Quality;

/// Face size of a probe on High, texels. Low relights and samples mip 1.
/// 48, not 64: the relight is every texel every frame, and 64 put the
/// probes over their share of the frame for reflections a rough truss
/// cannot show the difference in.
pub(crate) const PROBE_SIZE: u32 = 48;
/// Probes the textures and the grid uniform hold.
pub(crate) const PROBE_CAPACITY: u32 = 32;
/// Mips of the radiance cubes, 48 texels down to one.
const MIPS: u32 = 6;
const LAYERS: u32 = PROBE_CAPACITY * 6;
/// The capture atlas: one face per cell.
const ATLAS_COLUMNS: u32 = 16;
const ATLAS_ROWS: u32 = LAYERS.div_ceil(ATLAS_COLUMNS);
/// Bytes between face records: the dynamic-offset alignment.
const FACE_STRIDE: u64 = 256;
/// Faces captured per frame: one probe.
const FACES_PER_FRAME: u32 = 6;

const ALBEDO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const RADIANCE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Probes a layer holds, and layers, by quality.
fn grid_budget(quality: Quality) -> (u32, u32) {
    match quality {
        Quality::High => (12, 2),
        Quality::Low => (8, 1),
    }
}

/// The base mip the capture, relight and sampling use.
fn base_mip(quality: Quality) -> u32 {
    match quality {
        Quality::High => 0,
        Quality::Low => 1,
    }
}

/// Constants and shared WGSL every probe shader opens with.
pub(crate) fn prelude() -> String {
    format!(
        "const PROBE_CAPACITY: u32 = {PROBE_CAPACITY}u;\n\
         const PROBE_SIZE: u32 = {PROBE_SIZE}u;\n\
         const PROBE_ATLAS_COLUMNS: u32 = {ATLAS_COLUMNS}u;\n\
         const PROBE_AMBIENT_LEN: u32 = {}u;\n{}",
        PROBE_CAPACITY * 6,
        include_str!("shaders/probe_common.wgsl")
    )
}

/// `ProbeGrid` in `probe_common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GridUniform {
    origin: [f32; 4],
    step: [f32; 4],
    dims: [f32; 4],
    box_min: [f32; 4],
    box_max: [f32; 4],
    ground: [f32; 4],
    positions: [[f32; 4]; PROBE_CAPACITY as usize],
}

/// `ProbeFace` in `probe_capture.wgsl`, padded to its dynamic offset.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FaceUniform {
    probe: [f32; 4],
    ground: [f32; 4],
    _pad: [[f32; 4]; 14],
}

/// `ProbeFilterParams` in `probe_filter.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FilterParams {
    size: u32,
    layers: u32,
    roughness: f32,
    source_size: f32,
    source_mips: f32,
    _pad: [f32; 3],
}

/// Where the probes stand and what they see, for one stage.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Layout {
    pub origin: Vec3,
    pub step: Vec3,
    pub dims: UVec3,
    /// Each probe, world, in grid order (x fastest).
    pub positions: Vec<Vec3>,
    /// The parallax box.
    pub box_min: Vec3,
    pub box_max: Vec3,
    /// Metres past the grid over which it fades to the sky probe.
    pub fade: f32,
    /// The relight's reach round a probe, metres.
    pub reach: f32,
    pub base_mip: u32,
    /// The ground's albedo in the captures: the floor's mean colour.
    pub ground: Vec3,
    /// Whether the stage stands on a ground plane, which the relight extends
    /// to the horizon.
    pub has_ground: bool,
    /// Whether the sky probe holds the ground: open air, where its lower
    /// half is the floor under the sun. The probes then keep only what the
    /// stage changes of it (`probe_relight.wgsl`).
    pub open_air: bool,
}

impl Layout {
    pub(crate) fn count(&self) -> u32 {
        self.dims.x * self.dims.y * self.dims.z
    }

    /// Roughness mips from the base: to one texel on High, to four on Low,
    /// where each small dispatch costs more than its texels.
    fn mips(&self) -> u32 {
        if self.base_mip == 0 { MIPS } else { 4 }
    }

    /// Whether `other` puts the same probes in the same places: a capture of
    /// one is a (stale) capture of the other.
    fn same_placement(&self, other: &Layout) -> bool {
        self.dims == other.dims
            && self.positions == other.positions
            && self.box_min == other.box_min
            && self.box_max == other.box_max
            && self.base_mip == other.base_mip
    }

    fn uniform(&self, live: bool) -> GridUniform {
        let mut positions = [[0.0; 4]; PROBE_CAPACITY as usize];
        for (slot, position) in positions.iter_mut().zip(&self.positions) {
            *slot = position.extend(1.0).to_array();
        }
        GridUniform {
            origin: self.origin.extend(f32::from(u8::from(live))).to_array(),
            step: self.step.extend(self.base_mip as f32).to_array(),
            dims: [
                self.dims.x as f32,
                self.dims.y as f32,
                self.dims.z as f32,
                self.mips() as f32,
            ],
            box_min: self.box_min.extend(self.fade).to_array(),
            box_max: self.box_max.extend(self.reach).to_array(),
            ground: self
                .ground
                .extend(match (self.has_ground, self.open_air) {
                    (false, _) => 0.0,
                    (true, false) => 1.0,
                    (true, true) => 2.0,
                })
                .to_array(),
            positions,
        }
    }
}

/// A draw's world-space box, from its mesh's local box.
fn world_box(draw: &Draw, local: (Vec3, Vec3)) -> (Vec3, Vec3) {
    let (lo, hi) = local;
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for corner in 0..8 {
        let p = Vec3::new(
            if corner & 1 == 0 { lo.x } else { hi.x },
            if corner & 2 == 0 { lo.y } else { hi.y },
            if corner & 4 == 0 { lo.z } else { hi.z },
        );
        let world = draw.model.transform_point3(p);
        min = min.min(world);
        max = max.max(world);
    }
    (min, max)
}

fn local_box(frame: &Frame, mesh: usize) -> (Vec3, Vec3) {
    let vertices = &frame.meshes[mesh].vertices;
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for vertex in vertices.iter() {
        let p = Vec3::from(vertex.position);
        min = min.min(p);
        max = max.max(p);
    }
    if !min.is_finite() || !max.is_finite() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (min, max)
}

fn is_fixture(draw: &Draw) -> bool {
    matches!(draw.editor_object, Some(EditorObject::Fixture(_)))
}

/// Everything the captures depend on: the stage's geometry and materials
/// (fixtures by count only, so a moving head does not recapture every
/// frame), the floor, whether there is a sky, and the quality. Fixture
/// light, the sun and the clouds are the relight's, and change nothing here.
pub(crate) fn key(frame: &Frame, opaque: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut push = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let mut fixtures = 0u32;
    for draw in frame.draws.iter().take(opaque) {
        if is_fixture(draw) {
            fixtures += 1;
            continue;
        }
        push(frame.meshes[draw.mesh].key.as_bytes());
        for value in draw.model.to_cols_array() {
            push(&value.to_bits().to_le_bytes());
        }
        let m = &draw.material;
        for value in [
            m.base_color.x,
            m.base_color.y,
            m.base_color.z,
            m.metallic,
            m.roughness,
        ] {
            push(&value.to_bits().to_le_bytes());
        }
    }
    push(&fixtures.to_le_bytes());
    push(format!("{:?}", frame.floor.map(|floor| floor.floor)).as_bytes());
    push(&[
        u8::from(frame.sky.is_some()),
        u8::from(frame.quality == Quality::High),
    ]);
    hash
}

/// Where the probes go for `frame`, or `None` when there is nothing to see.
pub(crate) fn place(frame: &Frame, opaque: usize) -> Option<Layout> {
    let draws = &frame.draws[..opaque];
    let is_ground = |draw: &Draw| crate::frame::is_ground(&frame.meshes[draw.mesh].key);
    // One pass over a mesh's vertices however many times it is drawn.
    let mut local: Vec<Option<(Vec3, Vec3)>> = vec![None; frame.meshes.len()];
    let boxes: Vec<(Vec3, Vec3, bool)> = draws
        .iter()
        .filter(|draw| !is_ground(draw))
        .map(|draw| {
            let bounds = *local[draw.mesh].get_or_insert_with(|| local_box(frame, draw.mesh));
            let (min, max) = world_box(draw, bounds);
            (min, max, is_fixture(draw))
        })
        .filter(|(min, max, _)| min.is_finite() && max.is_finite())
        .collect();
    // The stage, not the rig hung over it; the rig alone when there is
    // nothing else.
    let staged: Vec<_> = boxes.iter().filter(|(.., fixture)| !fixture).collect();
    let chosen: Vec<_> = if staged.is_empty() {
        boxes.iter().collect()
    } else {
        staged
    };
    let (mut min, mut max) = chosen.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(lo, hi), (a, b, _)| (lo.min(*a), hi.max(*b)),
    );
    if !min.is_finite() || !max.is_finite() {
        return None;
    }
    // A venue model can be a whole park; the probes are for the stage, so
    // past 80 m they centre on the rig when there is one.
    let rig = boxes.iter().filter(|(.., fixture)| *fixture).fold(
        None,
        |acc: Option<(Vec3, Vec3)>, (a, b, _)| {
            Some(acc.map_or((*a, *b), |(lo, hi)| (lo.min(*a), hi.max(*b))))
        },
    );
    let mut centre = (min + max) * 0.5;
    if let Some((lo, hi)) = rig {
        let rig_centre = (lo + hi) * 0.5;
        let extent = max - min;
        if extent.x > 80.0 {
            centre.x = rig_centre.x;
        }
        if extent.y > 80.0 {
            centre.y = rig_centre.y;
        }
    }
    let half = ((max - min) * 0.5).min(Vec3::new(40.0, 40.0, 30.0));
    min = centre - half;
    max = centre + half;
    let margin = 2.0;
    let lo = Vec3::new(min.x - margin, min.y - margin, 0.0);
    let hi = Vec3::new(max.x + margin, max.y + margin, max.z.max(4.0) + 1.0);

    let (per_layer, layers) = grid_budget(frame.quality);
    let extent = (hi - lo).truncate().max(glam::Vec2::splat(1.0));
    let nx = ((per_layer as f32 * extent.x / extent.y).sqrt().round() as u32).clamp(1, per_layer);
    let ny = (per_layer / nx).max(1);
    let step_xy = extent / glam::Vec2::new(nx as f32, ny as f32);
    let (z0, step_z) = if layers > 1 {
        let upper = (max.z * 0.6).clamp(3.0, 12.0);
        (1.5, upper - 1.5)
    } else {
        (1.8, 1.0)
    };
    let origin = Vec3::new(lo.x + 0.5 * step_xy.x, lo.y + 0.5 * step_xy.y, z0);
    let step = Vec3::new(step_xy.x, step_xy.y, step_z);
    let dims = UVec3::new(nx, ny, layers);
    // A grid point inside a piece of the stage sees its inside. Move it up
    // over the piece. Boxes as big as a room are the room, and stay round it.
    let solid: Vec<_> = boxes
        .iter()
        .filter(|(a, b, _)| {
            let size = *b - *a;
            size.x < 30.0 && size.y < 30.0
        })
        .collect();
    let mut positions = Vec::new();
    for z in 0..dims.z {
        for y in 0..dims.y {
            for x in 0..dims.x {
                let mut p = origin + UVec3::new(x, y, z).as_vec3() * step;
                for _ in 0..4 {
                    let inside = solid
                        .iter()
                        .find(|(a, b, _)| p.cmpgt(*a + 0.05).all() && p.cmplt(*b - 0.05).all());
                    match inside {
                        Some((_, b, _)) => p.z = b.z + 0.25,
                        None => break,
                    }
                }
                positions.push(p);
            }
        }
    }
    let ground = frame
        .floor
        .map(|floor| crate::floor::mean_color(floor.floor))
        .or_else(|| {
            draws
                .iter()
                .find(|draw| is_ground(draw))
                .map(|draw| draw.material.base_color)
        })
        .unwrap_or(Vec3::splat(0.1));
    Some(Layout {
        origin,
        step,
        dims,
        positions,
        box_min: lo,
        box_max: hi,
        fade: step_xy.max_element(),
        reach: step.length().max(8.0),
        base_mip: base_mip(frame.quality),
        ground,
        has_ground: draws.iter().any(|draw| is_ground(draw)),
        open_air: frame.sky.is_some(),
    })
}

/// A face's axis, right and down in cube space (`probe_face_direction`).
const FACE_AXES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]),
    ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
    ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
    ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
    ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
];

/// Whether a ball of `radius` at `rel` (world, from the probe) is in
/// `face`'s view and more than about a texel across there.
pub(crate) fn face_sees(face: u32, rel: Vec3, radius: f32, texels: u32) -> bool {
    let distance = rel.length();
    if distance <= radius {
        return true;
    }
    // Under a texel: the capture would drop it or alias it anyway.
    if radius / distance < 1.5 / texels as f32 {
        return false;
    }
    let c = Vec3::new(rel.x, rel.z, -rel.y);
    let (axis, right, down) = FACE_AXES[face as usize];
    let (axis, right, down) = (Vec3::from(axis), Vec3::from(right), Vec3::from(down));
    let reach = radius * std::f32::consts::SQRT_2;
    let forward = c.dot(axis);
    forward + reach > c.dot(right).abs().max(c.dot(down).abs()) - reach && forward > -radius
}

/// The device's probe pipelines.
pub(crate) struct Pipelines {
    capture_layout: wgpu::BindGroupLayout,
    capture: wgpu::RenderPipeline,
    clear: wgpu::RenderPipeline,
    relight_layout: wgpu::BindGroupLayout,
    relight: wgpu::ComputePipeline,
    filter_layout: wgpu::BindGroupLayout,
    filter: wgpu::ComputePipeline,
    ambient: wgpu::ComputePipeline,
    filter_base_layout: wgpu::BindGroupLayout,
    filter_base: wgpu::ComputePipeline,
    dummy_target: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// Bound in the relight's environment group in place of the probes it
    /// is writing.
    dummy_cubes: wgpu::TextureView,
}

/// What the probe pipelines are built against.
pub(crate) struct PipelineSources<'a> {
    pub material_layout: &'a wgpu::BindGroupLayout,
    pub scene_layout: &'a wgpu::BindGroupLayout,
    pub environment_layout: &'a wgpu::BindGroupLayout,
    pub cluster_layout: &'a wgpu::BindGroupLayout,
    pub vertex_layout: wgpu::VertexBufferLayout<'a>,
    /// The scene module's source, which the relight is appended to.
    pub scene_source: &'a str,
}

/// The capture module's source: the scene's group-0 declarations for the
/// instance array, then the probe prelude and the capture.
pub(crate) fn capture_wgsl() -> String {
    format!(
        "{}{}{}{}{}",
        crate::haze_field::prelude(),
        include_str!("shaders/medium.wgsl"),
        include_str!("shaders/scene_bindings.wgsl"),
        prelude(),
        include_str!("shaders/probe_capture.wgsl")
    )
}

/// The relight module's source: the scene module and the relight's entries.
pub(crate) fn relight_wgsl(scene: &str) -> String {
    format!("{scene}{}", include_str!("shaders/probe_relight.wgsl"))
}

/// The prefilter module's source.
pub(crate) fn filter_wgsl() -> String {
    format!("{}{}", prelude(), include_str!("shaders/probe_filter.wgsl"))
}

fn layout_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    ty: wgpu::BindingType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty,
        count: None,
    }
}

fn storage(read_only: bool) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn texture(dimension: wgpu::TextureViewDimension, filterable: bool) -> wgpu::BindingType {
    wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable },
        view_dimension: dimension,
        multisampled: false,
    }
}

fn uniform() -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn storage_texture() -> wgpu::BindingType {
    wgpu::BindingType::StorageTexture {
        access: wgpu::StorageTextureAccess::WriteOnly,
        format: RADIANCE_FORMAT,
        view_dimension: wgpu::TextureViewDimension::D2Array,
    }
}

fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device, sources: &PipelineSources<'_>) -> Self {
        let shader = |label: &str, source: String| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        };
        let vertex_fragment = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let compute = wgpu::ShaderStages::COMPUTE;

        // Capture.
        let capture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe-capture"),
            entries: &[
                layout_entry(1, vertex_fragment, storage(true)),
                layout_entry(
                    2,
                    vertex_fragment,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                ),
            ],
        });
        let capture_module = shader("probe-capture", capture_wgsl());
        let targets = [Some(ALBEDO_FORMAT.into()), Some(NORMAL_FORMAT.into())];
        let capture = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("probe-capture"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("probe-capture"),
                    bind_group_layouts: &[Some(&capture_layout), Some(sources.material_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &capture_module,
                entry_point: Some("vs_capture"),
                buffers: &[Some(sources.vertex_layout.clone())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &capture_module,
                entry_point: Some("fs_capture"),
                targets: &targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // Clearing one atlas cell: a triangle over the viewport at the far
        // depth, zero in both targets.
        let clear_module = shader(
            "probe-clear",
            "struct Cleared { @location(0) albedo: vec4<f32>, @location(1) normal: vec4<f32> };\n\
             @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {\n\
                 let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));\n\
                 return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);\n\
             }\n\
             @fragment fn fs() -> Cleared { return Cleared(vec4<f32>(0.0), vec4<f32>(0.0)); }\n"
                .to_string(),
        );
        let clear = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("probe-clear"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("probe-clear"),
                    bind_group_layouts: &[],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &clear_module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &clear_module,
                entry_point: Some("fs"),
                targets: &targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Cull and relight, in the scene module.
        let relight_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe-relight"),
            entries: &[
                layout_entry(16, compute, texture(wgpu::TextureViewDimension::D2, false)),
                layout_entry(18, compute, texture(wgpu::TextureViewDimension::D2, false)),
                layout_entry(19, compute, storage_texture()),
                layout_entry(
                    20,
                    compute,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        let relight_module = shader("probe-relight", relight_wgsl(sources.scene_source));
        let compute_pipeline = |label: &str, group1: &wgpu::BindGroupLayout, entry_point: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[
                            Some(sources.scene_layout),
                            Some(group1),
                            Some(sources.environment_layout),
                            Some(sources.cluster_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &relight_module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let relight = compute_pipeline("probe-relight", &relight_layout, "probe_relight");

        // Prefilter.
        let filter_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe-filter"),
            entries: &[
                layout_entry(
                    0,
                    compute,
                    texture(wgpu::TextureViewDimension::CubeArray, true),
                ),
                layout_entry(
                    1,
                    compute,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
                layout_entry(2, compute, storage_texture()),
                layout_entry(
                    3,
                    compute,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                layout_entry(4, compute, storage(false)),
                layout_entry(
                    5,
                    compute,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        // Low's one-dispatch chain: the base mip in, three mips out.
        let filter_base_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("probe-filter-base"),
                entries: &[
                    layout_entry(
                        0,
                        compute,
                        texture(wgpu::TextureViewDimension::CubeArray, true),
                    ),
                    layout_entry(
                        1,
                        compute,
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    ),
                    layout_entry(2, compute, storage_texture()),
                    layout_entry(3, compute, uniform()),
                    layout_entry(4, compute, storage(false)),
                    layout_entry(5, compute, uniform()),
                    layout_entry(6, compute, storage_texture()),
                    layout_entry(7, compute, storage_texture()),
                ],
            });
        let filter_module = shader("probe-filter", filter_wgsl());
        let filter_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("probe-filter"),
                bind_group_layouts: &[Some(&filter_layout)],
                immediate_size: 0,
            });
        let filter_pipeline = |label: &str, entry_point: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&filter_pipeline_layout),
                module: &filter_module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let filter = filter_pipeline("probe-filter", "probe_filter");
        let ambient = filter_pipeline("probe-ambient", "probe_ambient");
        let filter_base = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("probe-filter-base"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("probe-filter-base"),
                    bind_group_layouts: &[Some(&filter_base_layout)],
                    immediate_size: 0,
                }),
            ),
            module: &filter_module,
            entry_point: Some("probe_filter_base"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        // The ambient pass reads every mip, so its unused storage target is
        // this one texel, outside the probes.
        let dummy_target = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("probe-dummy-target"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: RADIANCE_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("probe-filter"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe-dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: RADIANCE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_cubes = dummy.create_view(&wgpu::TextureViewDescriptor {
            label: Some("probe-dummy"),
            dimension: Some(wgpu::TextureViewDimension::CubeArray),
            ..Default::default()
        });
        Self {
            capture_layout,
            capture,
            clear,
            relight_layout,
            relight,
            filter_layout,
            filter,
            ambient,
            filter_base_layout,
            filter_base,
            dummy_target,
            sampler,
            dummy_cubes,
        }
    }

    /// The cube array the relight's environment group binds instead of the
    /// probes it writes.
    pub(crate) fn dummy_cubes(&self) -> &wgpu::TextureView {
        &self.dummy_cubes
    }
}

/// One renderer's probes: their textures, their layout and how far the
/// capture of it has got.
pub(crate) struct Probes {
    albedo: wgpu::TextureView,
    normal: wgpu::TextureView,
    /// The atlases themselves, for [`Probes::read_back`].
    albedo_texture: wgpu::Texture,
    normal_texture: wgpu::Texture,
    depth: wgpu::TextureView,
    radiance: wgpu::Texture,
    cubes: wgpu::TextureView,
    grid: wgpu::Buffer,
    /// Each probe face's mean radiance (`probe_ambient`): written by the
    /// prefilter, read by the scene pass as a uniform.
    ambient: wgpu::Buffer,
    /// Every fixture cone of the frame in source order, for the relight's
    /// group 3, and how many there are.
    cones: Option<(wgpu::Buffer, wgpu::Buffer, usize)>,
    light_total: wgpu::Buffer,
    faces: wgpu::Buffer,
    key: Option<u64>,
    layout: Option<Layout>,
    /// Faces captured since the stage last changed, up to all of them.
    captured: u32,
    /// The next face to capture: the capture runs round the probes, a probe
    /// a frame, so a stage piece dragged every frame still refreshes them all.
    cursor: u32,
    /// Whether the grid uniform says the probes are live. A new placement
    /// goes live when every face of it has been captured once.
    live: bool,
    /// Bind groups of the current layout.
    groups: Option<Groups>,
    /// Which half of the probes Low relights this frame.
    tick: bool,
    /// The frame's [`crate::scene_desc::ProbeView`]: off, the scene pass
    /// has only the sky probe.
    enabled: bool,
    /// Draw each probe as a ball showing its cube.
    debug: bool,
}

/// Vertices of one debug ball (`probe_debug.wgsl`): 12 rings of 24 quads.
pub(crate) const DEBUG_BALL_VERTICES: u32 = 12 * 24 * 6;

/// The relight's and the prefilter's bind groups, which name only the
/// probes' own textures and buffers, so they last as long as the layout.
struct Groups {
    relight: wgpu::BindGroup,
    /// Each prefiltered mip's output size and group.
    filter: Vec<(u32, wgpu::BindGroup)>,
    /// The ambient cube's reduction of the lowest mip.
    ambient: wgpu::BindGroup,
    /// Low: the whole chain from the base mip, and its workgroups.
    base: Option<(u32, wgpu::BindGroup)>,
}

/// What [`Probes::prepare`] found this frame.
pub(crate) struct CaptureWork {
    /// The faces to capture this frame, as layers (probe * 6 + face).
    pub capture: std::ops::Range<u32>,
}

impl Probes {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let atlas = |label: &str, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: ATLAS_COLUMNS * PROBE_SIZE,
                    height: ATLAS_ROWS * PROBE_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let albedo_texture = atlas("probe-albedo", ALBEDO_FORMAT);
        let normal_texture = atlas("probe-normal", NORMAL_FORMAT);
        let view =
            |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());
        let radiance = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe-radiance"),
            size: wgpu::Extent3d {
                width: PROBE_SIZE,
                height: PROBE_SIZE,
                depth_or_array_layers: LAYERS,
            },
            mip_level_count: MIPS,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: RADIANCE_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let cubes = radiance.create_view(&wgpu::TextureViewDescriptor {
            label: Some("probe-cubes"),
            dimension: Some(wgpu::TextureViewDimension::CubeArray),
            ..Default::default()
        });
        let ambient = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe-ambient"),
            size: u64::from(LAYERS) * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let grid = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe-grid"),
            size: std::mem::size_of::<GridUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_total = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe-light-total"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let faces = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe-faces"),
            size: u64::from(FACES_PER_FRAME) * FACE_STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            albedo: view(&albedo_texture),
            normal: view(&normal_texture),
            depth: view(&atlas("probe-depth", DEPTH_FORMAT)),
            albedo_texture,
            normal_texture,
            radiance,
            cubes,
            grid,
            ambient,
            cones: None,
            light_total,
            faces,
            key: None,
            layout: None,
            captured: 0,
            cursor: 0,
            live: false,
            groups: None,
            tick: false,
            enabled: true,
            debug: false,
        }
    }

    /// The cube array the scene pass samples, and its grid.
    pub(crate) fn bindings(&self) -> (&wgpu::TextureView, &wgpu::Buffer, &wgpu::Buffer) {
        (&self.cubes, &self.grid, &self.ambient)
    }

    /// Upload every fixture cone of the frame, in source order. The camera's
    /// light index holds only the cones in view, and a probe sees behind the
    /// camera.
    pub(crate) fn set_lights<C: Pod, R: Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cores: &[C],
        rests: &[R],
    ) {
        let count = cores.len().min(rests.len());
        if self
            .cones
            .as_ref()
            .is_none_or(|(.., capacity)| *capacity < count)
        {
            let capacity = count.next_power_of_two().max(64);
            let buffer = |label: &str, stride: usize| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: (capacity * stride) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            };
            self.cones = Some((
                buffer("probe-cores", std::mem::size_of::<C>()),
                buffer("probe-rests", std::mem::size_of::<R>()),
                capacity,
            ));
        }
        let (core_buffer, rest_buffer, _) = self.cones.as_ref().expect("allocated above");
        if count > 0 {
            queue.write_buffer(core_buffer, 0, bytemuck::cast_slice(&cores[..count]));
            queue.write_buffer(rest_buffer, 0, bytemuck::cast_slice(&rests[..count]));
        }
        queue.write_buffer(
            &self.light_total,
            0,
            bytemuck::cast_slice(&[count as u32, 0, 0, 0]),
        );
    }

    /// The source-order cone buffers [`Self::set_lights`] filled, for the
    /// relight's copy of the cluster group.
    pub(crate) fn cone_buffers(&self) -> Option<(&wgpu::Buffer, &wgpu::Buffer)> {
        self.cones.as_ref().map(|(cores, rests, _)| (cores, rests))
    }

    /// How many debug balls to draw this frame, when the debug view is on.
    pub(crate) fn debug_balls(&self) -> Option<u32> {
        (self.debug && self.enabled)
            .then_some(self.layout.as_ref())
            .flatten()
            .map(Layout::count)
    }

    /// Whether this frame has probes to relight.
    pub(crate) fn live(&self) -> bool {
        self.enabled && self.layout.is_some()
    }

    /// Faces still to capture since the stage last changed.
    pub(crate) fn pending(&self) -> u32 {
        if !self.enabled {
            return 0;
        }
        self.layout
            .as_ref()
            .map_or(0, |layout| layout.count() * 6 - self.captured)
    }

    /// Read one probe's six faces back as linear RGBA, laid side by side
    /// (+X, -X, +Y, -Y, +Z, -Z of cube space): its relit radiance at `mip`
    /// (from the base), or with `what` 1 and 2 its captured albedo and its
    /// normal (xy octahedral, z metal, w distance). For diagnosis only.
    pub(crate) fn read_back(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        probe: u32,
        what: u32,
        mip: u32,
    ) -> anyhow::Result<(u32, u32, Vec<[f32; 4]>)> {
        let layout = self
            .layout
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no probes placed"))?;
        anyhow::ensure!(probe < layout.count(), "no probe {probe}");
        let base = layout.base_mip;
        let size = match what {
            0 => (PROBE_SIZE >> (base + mip)).max(1),
            _ => PROBE_SIZE >> base,
        };
        let texel = if what == 1 { 4 } else { 8 };
        let row = (size * texel).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe-read-back"),
            size: u64::from(row * size * 6),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        for face in 0..6 {
            let layer = probe * 6 + face;
            let (texture, mip_level, origin) = match what {
                0 => (
                    &self.radiance,
                    base + mip,
                    wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                ),
                _ => (
                    if what == 1 {
                        &self.albedo_texture
                    } else {
                        &self.normal_texture
                    },
                    0,
                    wgpu::Origin3d {
                        x: (layer % ATLAS_COLUMNS) * PROBE_SIZE,
                        y: (layer / ATLAS_COLUMNS) * PROBE_SIZE,
                        z: 0,
                    },
                ),
            };
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: u64::from(row * size * face),
                        bytes_per_row: Some(row),
                        rows_per_image: Some(size),
                    },
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(anyhow::Error::msg)?;
        let bytes = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(anyhow::Error::msg)?
            .to_vec();
        let width = size * 6;
        let mut out = vec![[0.0; 4]; (width * size) as usize];
        for face in 0..6 {
            for y in 0..size {
                for x in 0..size {
                    let o = (row * size * face + row * y + x * texel) as usize;
                    let value = if what == 1 {
                        let srgb = |b: u8| {
                            let c = f32::from(b) / 255.0;
                            if c <= 0.04045 {
                                c / 12.92
                            } else {
                                ((c + 0.055) / 1.055).powf(2.4)
                            }
                        };
                        [
                            srgb(bytes[o]),
                            srgb(bytes[o + 1]),
                            srgb(bytes[o + 2]),
                            f32::from(bytes[o + 3]) / 255.0,
                        ]
                    } else {
                        std::array::from_fn(|c| {
                            half::f16::from_le_bytes([bytes[o + 2 * c], bytes[o + 2 * c + 1]])
                                .to_f32()
                        })
                    };
                    out[(y * width + face * size + x) as usize] = value;
                }
            }
        }
        Ok((width, size, out))
    }

    /// Where the probes stand, world.
    pub(crate) fn positions(&self) -> Vec<Vec3> {
        self.layout
            .as_ref()
            .map_or_else(Vec::new, |layout| layout.positions.clone())
    }

    /// Turn the probes on or off from this frame. Off keeps the placement
    /// and the captures, so turning them back on is immediate: the scene
    /// pass just stops sampling them and nothing relights them.
    fn set_enabled(&mut self, queue: &wgpu::Queue, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            let uniform = match &self.layout {
                Some(layout) if enabled => layout.uniform(self.live),
                _ => GridUniform::zeroed(),
            };
            queue.write_buffer(&self.grid, 0, bytemuck::bytes_of(&uniform));
        }
    }

    /// Take `frame`'s stage and decide this frame's capture.
    ///
    /// A change to the stage that leaves the probes where they stand keeps
    /// them live and recaptures them round-robin, a probe a frame. One that
    /// moves them takes them out of use until each is captured once, and the
    /// sky probe lights the stage meanwhile.
    pub(crate) fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        frame: &Frame,
        opaque: usize,
    ) -> CaptureWork {
        // Low lights from the sky probe alone: the probes cost about 0.17 ms
        // there against a 0.1 ms budget.
        self.set_enabled(queue, frame.probes.enabled && frame.quality == Quality::High);
        self.debug = frame.probes.debug;
        if !self.enabled {
            return CaptureWork { capture: 0..0 };
        }
        let key = Some(key(frame, opaque));
        if key != self.key {
            self.key = key;
            let layout = key.and_then(|_| place(frame, opaque));
            let kept = match (&self.layout, &layout) {
                (Some(old), Some(new)) => old.same_placement(new),
                _ => false,
            };
            if !kept {
                self.cursor = 0;
                self.live = false;
                self.groups = None;
            }
            self.captured = 0;
            self.layout = layout;
            let uniform = self
                .layout
                .as_ref()
                .map_or_else(GridUniform::zeroed, |layout| layout.uniform(self.live));
            queue.write_buffer(&self.grid, 0, bytemuck::bytes_of(&uniform));
        }
        let Some(layout) = &self.layout else {
            return CaptureWork { capture: 0..0 };
        };
        let total = layout.count() * 6;
        if self.captured >= total {
            return CaptureWork { capture: 0..0 };
        }
        let start = self.cursor;
        let count = FACES_PER_FRAME.min(total - start);
        self.cursor = (start + count) % total;
        self.captured = (self.captured + count).min(total);
        if self.captured == total && !self.live {
            // This frame's capture completes the set, and it is encoded
            // before this frame's relight and scene pass.
            self.live = true;
            queue.write_buffer(&self.grid, 0, bytemuck::bytes_of(&layout.uniform(true)));
        }
        CaptureWork {
            capture: start..start + count,
        }
    }

    /// Record the capture of `faces`.
    ///
    /// `draw` issues the draws a face sees, given the pass and the face's
    /// probe position and face index; the caller owns the frame's geometry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_capture(
        &self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        instances: &wgpu::Buffer,
        faces: std::ops::Range<u32>,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
        mut draw: impl FnMut(&mut wgpu::RenderPass<'_>, Vec3, u32, u32),
    ) {
        let Some(layout) = &self.layout else {
            return;
        };
        if faces.is_empty() {
            return;
        }
        let records: Vec<FaceUniform> = faces
            .clone()
            .map(|layer| FaceUniform {
                probe: layout.positions[(layer / 6) as usize]
                    .extend((layer % 6) as f32)
                    .to_array(),
                ground: layout.ground.extend(0.0).to_array(),
                _pad: [[0.0; 4]; 14],
            })
            .collect();
        queue.write_buffer(&self.faces, 0, bytemuck::cast_slice(&records));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("probe-capture"),
            layout: &pipelines.capture_layout,
            entries: &[
                entry(1, instances.as_entire_binding()),
                entry(
                    2,
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.faces,
                        offset: 0,
                        size: wgpu::BufferSize::new(32),
                    }),
                ),
            ],
        });
        let size = PROBE_SIZE >> layout.base_mip;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("probe-capture"),
            color_attachments: &[
                Some(wgpu::RenderPassColorAttachment {
                    view: &self.albedo,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                }),
                Some(wgpu::RenderPassColorAttachment {
                    view: &self.normal,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                }),
            ],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: timestamps,
            ..Default::default()
        });
        for (slot, layer) in faces.enumerate() {
            let x = (layer % ATLAS_COLUMNS) * PROBE_SIZE;
            let y = (layer / ATLAS_COLUMNS) * PROBE_SIZE;
            pass.set_viewport(x as f32, y as f32, size as f32, size as f32, 0.0, 1.0);
            pass.set_scissor_rect(x, y, size, size);
            pass.set_pipeline(&pipelines.clear);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&pipelines.capture);
            pass.set_bind_group(0, &group, &[(slot as u64 * FACE_STRIDE) as u32]);
            draw(
                &mut pass,
                layout.positions[(layer / 6) as usize],
                layer % 6,
                size,
            );
        }
    }

    /// Record this frame's relight and prefilter of every probe.
    ///
    /// `environment` is the scene's environment group with
    /// [`Pipelines::dummy_cubes`] in place of the probes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_relight(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &wgpu::BindGroup,
        environment: &wgpu::BindGroup,
        clusters: &wgpu::BindGroup,
        relight_timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
        filter_timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let Some(layout) = &self.layout else {
            return;
        };
        let count = layout.count();
        let size = PROBE_SIZE >> layout.base_mip;
        let layers = count * 6;
        if self.groups.is_none() {
            self.groups = Some(self.build_groups(pipelines, device, queue));
        }
        // On Low half the probes a frame: the first half, then the rest.
        let (first, relit) = if layout.base_mip > 0 && count > 1 {
            let half = count.div_ceil(2);
            self.tick = !self.tick;
            if self.tick {
                (half, count - half)
            } else {
                (0, half)
            }
        } else {
            (0, count)
        };
        queue.write_buffer(&self.light_total, 4, bytemuck::bytes_of(&(first * 6)));
        let relit_layers = relit * 6;
        let groups = self.groups.as_ref().expect("built above");
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("probe-relight"),
                timestamp_writes: relight_timestamps,
            });
            pass.set_bind_group(0, scene, &[]);
            pass.set_bind_group(2, environment, &[]);
            pass.set_bind_group(3, clusters, &[]);
            pass.set_pipeline(&pipelines.relight);
            pass.set_bind_group(1, &groups.relight, &[]);
            pass.dispatch_workgroups(size.div_ceil(8), size.div_ceil(8), relit_layers);
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("probe-filter"),
            timestamp_writes: filter_timestamps,
        });
        if let Some((workgroups, group)) = &groups.base {
            pass.set_pipeline(&pipelines.filter_base);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(*workgroups, 1, relit_layers);
            return;
        }
        pass.set_pipeline(&pipelines.filter);
        for (mip_size, group) in &groups.filter {
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(mip_size.div_ceil(8), mip_size.div_ceil(8), relit_layers);
        }
        pass.set_pipeline(&pipelines.ambient);
        pass.set_bind_group(0, &groups.ambient, &[]);
        pass.dispatch_workgroups(layers.div_ceil(64), 1, 1);
    }

    /// The current layout's relight and prefilter groups.
    fn build_groups(
        &self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Groups {
        let layout = self.layout.as_ref().expect("a layout to build for");
        let base = layout.base_mip;
        let size = PROBE_SIZE >> base;
        let layers = layout.count() * 6;
        let storage_view = |mip: u32| {
            self.radiance.create_view(&wgpu::TextureViewDescriptor {
                label: Some("probe-radiance-mip"),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                base_mip_level: mip,
                mip_level_count: Some(1),
                base_array_layer: 0,
                array_layer_count: Some(layers),
                ..Default::default()
            })
        };
        let target = storage_view(base);
        let relight = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("probe-relight"),
            layout: &pipelines.relight_layout,
            entries: &[
                entry(16, wgpu::BindingResource::TextureView(&self.albedo)),
                entry(18, wgpu::BindingResource::TextureView(&self.normal)),
                entry(19, wgpu::BindingResource::TextureView(&target)),
                entry(20, self.light_total.as_entire_binding()),
            ],
        });
        let mips = layout.mips();
        let filter = (1..mips)
            .map(|level| {
                let record = FilterParams {
                    size: (size >> level).max(1),
                    layers,
                    roughness: level as f32 / (mips - 1) as f32,
                    source_size: size as f32,
                    source_mips: level as f32,
                    _pad: [0.0; 3],
                };
                let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("probe-filter"),
                    size: std::mem::size_of::<FilterParams>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&record));
                // The mips above this one: the filter reads the finer mip
                // whose texel matches each sample's solid angle.
                let source = self.radiance.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("probe-filter-source"),
                    dimension: Some(wgpu::TextureViewDimension::CubeArray),
                    base_mip_level: base,
                    mip_level_count: Some(level),
                    base_array_layer: 0,
                    array_layer_count: Some(layers),
                    ..Default::default()
                });
                let target = storage_view(base + level);
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("probe-filter"),
                    layout: &pipelines.filter_layout,
                    entries: &[
                        entry(0, wgpu::BindingResource::TextureView(&source)),
                        entry(1, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                        entry(2, wgpu::BindingResource::TextureView(&target)),
                        entry(3, uniform.as_entire_binding()),
                        entry(4, self.ambient.as_entire_binding()),
                        entry(5, self.light_total.as_entire_binding()),
                    ],
                });
                (record.size, group)
            })
            .collect();
        let ambient = {
            let record = FilterParams {
                size: 1,
                layers,
                roughness: 1.0,
                source_size: size as f32,
                source_mips: mips as f32,
                _pad: [0.0; 3],
            };
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("probe-ambient"),
                size: std::mem::size_of::<FilterParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&record));
            let source = self.radiance.create_view(&wgpu::TextureViewDescriptor {
                label: Some("probe-ambient-source"),
                dimension: Some(wgpu::TextureViewDimension::CubeArray),
                base_mip_level: base,
                mip_level_count: Some(mips),
                base_array_layer: 0,
                array_layer_count: Some(layers),
                ..Default::default()
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("probe-ambient"),
                layout: &pipelines.filter_layout,
                entries: &[
                    entry(0, wgpu::BindingResource::TextureView(&source)),
                    entry(1, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                    entry(
                        2,
                        wgpu::BindingResource::TextureView(&pipelines.dummy_target),
                    ),
                    entry(3, uniform.as_entire_binding()),
                    entry(4, self.ambient.as_entire_binding()),
                    entry(5, self.light_total.as_entire_binding()),
                ],
            })
        };
        let base_group = (base > 0).then(|| {
            let record = FilterParams {
                size: size / 2,
                layers,
                roughness: 0.0,
                source_size: size as f32,
                source_mips: 1.0,
                _pad: [0.0; 3],
            };
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("probe-filter-base"),
                size: std::mem::size_of::<FilterParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&record));
            let source = self.radiance.create_view(&wgpu::TextureViewDescriptor {
                label: Some("probe-filter-base-source"),
                dimension: Some(wgpu::TextureViewDimension::CubeArray),
                base_mip_level: base,
                mip_level_count: Some(1),
                base_array_layer: 0,
                array_layer_count: Some(layers),
                ..Default::default()
            });
            let targets = [1, 2, 3].map(|level| storage_view(base + level));
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("probe-filter-base"),
                layout: &pipelines.filter_base_layout,
                entries: &[
                    entry(0, wgpu::BindingResource::TextureView(&source)),
                    entry(1, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                    entry(2, wgpu::BindingResource::TextureView(&targets[0])),
                    entry(3, uniform.as_entire_binding()),
                    entry(4, self.ambient.as_entire_binding()),
                    entry(5, self.light_total.as_entire_binding()),
                    entry(6, wgpu::BindingResource::TextureView(&targets[1])),
                    entry(7, wgpu::BindingResource::TextureView(&targets[2])),
                ],
            });
            let s1 = size / 2;
            let (s2, s3) = ((s1 / 2).max(1), (s1 / 4).max(1));
            ((s1 * s1 + s2 * s2 + s3 * s3).div_ceil(64), group)
        });
        Groups {
            relight,
            filter,
            ambient,
            base: base_group,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_sees_what_lies_along_its_axis_and_nothing_behind() {
        for face in 0..6u32 {
            let (axis, ..) = FACE_AXES[face as usize];
            let cube = Vec3::from(axis);
            // Back to world: cube (x, y, z) is world (x, -z, y).
            let world = Vec3::new(cube.x, -cube.z, cube.y);
            assert!(face_sees(face, world * 10.0, 1.0, 64), "face {face}");
            assert!(
                !face_sees(face, -world * 10.0, 1.0, 64),
                "face {face} behind"
            );
        }
    }

    #[test]
    fn a_ball_under_a_texel_is_not_drawn() {
        let far = Vec3::new(100.0, 0.0, 0.0);
        assert!(!face_sees(0, far, 0.1, 64));
        assert!(face_sees(0, far, 5.0, 64));
    }
}
