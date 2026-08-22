//! The wgpu device and the frame's pass chain.
//!
//! Offscreen only: nothing here knows about a window or a swapchain. A frame
//! goes shadow → depth → scene (MSAA) → haze (accumulated) → composite+AgX →
//! editor overlays → readback, and comes out as sRGB-encoded 8-bit bytes in
//! the caller's [`Channels`] order.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec2, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::assets::Image;
use crate::clusters::{ClusterCache, CLUSTER_DEPTH_SLICES, CLUSTER_TILE_SIZE};
use crate::environment::EnvironmentSystem;
use crate::frame::{Draw, Frame};
use crate::overlay::{Overlay, OverlayDepth};

/// Three bounded layers cover the part of a venue in which directional
/// shadows remain useful. 2048² per layer costs 48 MiB in `Depth32Float`, versus
/// an unbounded camera-sized allocation or 192 MiB for three legacy 4096 maps.
const SHADOW_SIZE: u32 = 2048;
const CASCADE_COUNT: usize = 3;
const CASCADE_SPLITS: [f32; CASCADE_COUNT] = [12.0, 45.0, 180.0];
const CASCADE_BLEND: f32 = 0.1;

const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MSAA_SAMPLES: u32 = 4;
const CAMERA_NEAR: f32 = 0.1;
const CAMERA_FAR: f32 = 2000.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_view_proj: [[[f32; 4]; 4]; CASCADE_COUNT],
    camera_pos: [f32; 4],
    camera_forward: [f32; 4],
    cascade_splits: [f32; 4],
    ambient: [f32; 4],
    dir_to_light: [f32; 4],
    dir_color: [f32; 4],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    model: [[f32; 4]; 4],
    normal_matrix: [[f32; 4]; 4],
    base_color: [f32; 4],
    emissive: [f32; 4],
    /// x: `flat_shading`, y: normal-map scale, z: AO strength.
    flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OverlayInstance {
    model: [[f32; 4]; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PointLightGpu {
    position: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HazeUniform {
    inv_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    params: [f32; 4],
    tuning: [f32; 4],
    transport: [f32; 4],
    tiles: [f32; 4],
    depth: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightCore {
    position: [f32; 3],
    range: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightRest {
    direction: [f32; 3],
    cos_beam: f32,
    color: [f32; 3],
    intensity: f32,
    cos_field: f32,
    wash: f32,
    gobo: f32,
    gobo_rotation: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SurfaceClusterHeader {
    offset: u32,
    count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SurfaceClusterUniform {
    /// x/y: grid columns/rows, z: tile size, w: depth slices.
    grid: [u32; 4],
    /// x/y: near/far, z: surface lighting enabled, w: occupancy debug.
    depth_and_flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileHeader {
    offset: u32,
    count: u32,
    _pad: [u32; 2],
}

const HAZE_TILE_SIZE: u32 = 16;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniform {
    inv_view_proj: [[f32; 4]; 4],
    params: [f32; 4],
    depth: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TemporalUniform {
    /// x: history weight, y: history valid, z: depth rejection threshold.
    params: [f32; 4],
}

/// Transport constants the look was dialled in on (spec §3.1). They are baked:
/// the keyboard dials that produced them do not port.
struct Transport;
impl Transport {
    const BEAM_GAIN: f32 = 180.0;
    const WHITE_LEAK: f32 = 0.03;
    const PHASE_G: f32 = 0.6;
    const NEAR_CLAMP: f32 = 0.06;
}

/// Byte order a readback is written in.
///
/// This is the *output texture's* format, not a post-processing step: the
/// composite pass writes straight into the order the caller asked for, so the
/// readback is a row memcpy either way. Swizzling on the CPU instead cost about
/// a millisecond per megapixel, for a choice the sampler makes for free.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channels {
    /// What a PNG wants.
    Rgba,
    /// What `gpui::RenderImage` wants.
    Bgra,
}

/// Cumulative immutable-resource uploads made by a renderer instance.
///
/// This is an acceptance probe rather than a timing estimate: a steady-state
/// live rig must leave both values unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadStats {
    /// Combined geometry-bank uploads.
    pub geometry: u64,
    /// Material-map uploads, counted per source/color-space role.
    pub textures: u64,
    /// HDR environment uploads and GPU preprocessing runs.
    pub environments: u64,
}

/// Adapter identity attached to volumetric timing evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct RendererProfile {
    /// Human-readable adapter name.
    pub name: String,
    /// Graphics backend reported by wgpu.
    pub backend: String,
    /// Adapter class (integrated, discrete, CPU, and so on).
    pub device_type: String,
    /// Driver name.
    pub driver: String,
    /// Driver detail string.
    pub driver_info: String,
    /// Whether the adapter can provide hardware timestamp queries.
    pub timestamp_query_supported: bool,
    /// Nanoseconds represented by one timestamp tick, when supported.
    pub timestamp_period_ns: Option<f32>,
}

/// Pass-boundary timings for one production live render.
#[derive(Debug, Clone, Copy)]
pub struct FrameTimings {
    /// GPU time from the first scene pass through the composite pass.
    pub gpu_total_ms: f64,
    /// GPU time for haze accumulation and temporal resolve.
    pub gpu_volumetric_ms: f64,
    /// CPU time for scene preparation, command encoding and queue submission.
    pub cpu_encode_submit_ms: f64,
    /// CPU time spent rebuilding the deterministic surface-light cluster CSR.
    /// A cache hit reports zero.
    pub cpu_cluster_ms: f64,
}

/// Latest bounded clustered-light index metrics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ClusterStats {
    /// Number of clusters containing at least one fixture cone.
    pub occupied_clusters: usize,
    /// Total packed light references across all clusters.
    pub light_references: usize,
    /// Largest list visited by one surface fragment.
    pub max_lights_per_cluster: u32,
    /// Cumulative topology rebuild count for this renderer.
    pub rebuilds: u64,
}

impl Channels {
    fn format(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Bgra => wgpu::TextureFormat::Bgra8UnormSrgb,
        }
    }

    /// Slot in [`Renderer::composite_pipelines`].
    fn index(self) -> usize {
        match self {
            Self::Rgba => 0,
            Self::Bgra => 1,
        }
    }
}

/// Owns the wgpu device and every pipeline. One instance renders any number of
/// frames at any number of sizes; render targets are reallocated on size change.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_profile: RendererProfile,
    scene_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    environment: EnvironmentSystem,
    cluster_layout: wgpu::BindGroupLayout,
    haze_layout: wgpu::BindGroupLayout,
    temporal_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    scene_pipeline: wgpu::RenderPipeline,
    depth_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    haze_pipeline: wgpu::RenderPipeline,
    temporal_pipeline: wgpu::RenderPipeline,
    /// Indexed by [`Channels::index`]: the same pass, targeting each output
    /// format.
    composite_pipelines: [wgpu::RenderPipeline; 2],
    grid_pipeline: wgpu::RenderPipeline,
    overlay_layout: wgpu::BindGroupLayout,
    /// Indexed by [`overlay_pipeline_index`]: the two output formats crossed
    /// with two topologies and two depth behaviours.
    overlay_pipelines: [wgpu::RenderPipeline; 8],
    shadow_map: wgpu::TextureView,
    shadow_layers: [wgpu::TextureView; CASCADE_COUNT],
    hard_shadow_sampler: wgpu::Sampler,
    shadow_sampler: wgpu::Sampler,
    dummy_shadow: wgpu::TextureView,
    linear_sampler: wgpu::Sampler,
    texture_sampler: wgpu::Sampler,
    /// Neutral glTF maps, bound by procedural/depth-only draws.
    white_material: wgpu::BindGroup,
    material_defaults: MaterialDefaults,
    /// Uploaded images by stable source identity and color-space role.
    texture_views: HashMap<TextureKey, wgpu::TextureView>,
    /// Material bind groups by their five immutable map identities.
    ///
    /// A frame of a live rig names the same textures as the frame before it,
    /// and each upload carries a full mip chain built on the CPU. Keeping them
    /// is the difference between paying that once and paying it sixty times a
    /// second. Entries the current frame does not name are dropped, so a venue
    /// change does not accumulate the old venue's textures.
    materials: HashMap<MaterialKey, wgpu::BindGroup>,
    /// Immutable scene geometry resident across live frames. A resolved frame
    /// rebuilds transforms and light state, but its asset/procedural mesh keys
    /// stay stable until the venue changes.
    geometry: Option<ResidentGeometry>,
    upload_stats: UploadStats,
    targets: Option<Targets>,
    haze_history_valid: bool,
    haze_history_index: usize,
    haze_history_key: Option<HazeHistoryKey>,
    last_live_time: Option<f32>,
    live_noise_frame: u32,
    profiler: Option<ProfilerResources>,
    haze_tile_cache: Option<HazeTileCache>,
    cluster_cache: ClusterCache,
    cluster_gpu: Option<SurfaceClusterGpu>,
    cluster_stats: ClusterStats,
}

struct SurfaceClusterGpu {
    headers: wgpu::Buffer,
    indices: wgpu::Buffer,
    columns: u32,
    rows: u32,
    near: f32,
    far: f32,
}

struct ProfilerResources {
    slots: [ProfilerSlot; 3],
    timestamp_period_ns: f32,
}

struct ProfilerSlot {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HazeTileKey {
    size: [u32; 2],
    camera: [u32; 6],
    fov: u32,
    topology: u64,
}

struct HazeTileCache {
    key: HazeTileKey,
    headers: wgpu::Buffer,
    indices: wgpu::Buffer,
    columns: u32,
    rows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HazeHistoryKey {
    output: [u32; 4],
    camera: [u32; 6],
    fov: u32,
    density: u32,
    topology: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TextureEncoding {
    Srgb,
    Linear,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TextureKey {
    source: String,
    encoding: TextureEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MaterialKey {
    base_color: Option<TextureKey>,
    normal: Option<TextureKey>,
    metallic_roughness: Option<TextureKey>,
    occlusion: Option<TextureKey>,
    emissive: Option<TextureKey>,
}

impl MaterialKey {
    fn of(draw: &Draw, frame: &Frame) -> Self {
        let key = |index: Option<usize>, encoding| {
            index.map(|index| TextureKey {
                source: frame.images[index].key.clone(),
                encoding,
            })
        };
        Self {
            base_color: key(draw.textures.base_color, TextureEncoding::Srgb),
            normal: key(draw.textures.normal, TextureEncoding::Linear),
            metallic_roughness: key(draw.textures.metallic_roughness, TextureEncoding::Linear),
            occlusion: key(draw.textures.occlusion, TextureEncoding::Linear),
            emissive: key(draw.textures.emissive, TextureEncoding::Srgb),
        }
    }

    fn textures(&self) -> impl Iterator<Item = &TextureKey> {
        [
            self.base_color.as_ref(),
            self.normal.as_ref(),
            self.metallic_roughness.as_ref(),
            self.occlusion.as_ref(),
            self.emissive.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
}

struct MaterialDefaults {
    base_color: wgpu::TextureView,
    normal: wgpu::TextureView,
    metallic_roughness: wgpu::TextureView,
    occlusion: wgpu::TextureView,
    emissive: wgpu::TextureView,
}

struct ResidentGeometry {
    keys: Vec<String>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    ranges: Vec<(u32, u32, i32)>,
}

impl ResidentGeometry {
    fn matches(&self, frame: &Frame) -> bool {
        self.keys.len() == frame.meshes.len()
            && self
                .keys
                .iter()
                .zip(&frame.meshes)
                .all(|(resident, incoming)| resident == &incoming.key)
    }
}

struct Targets {
    width: u32,
    height: u32,
    /// Size of the haze target, which runs at `haze_resolution` of the output.
    haze_width: u32,
    haze_height: u32,
    channels: Channels,
    msaa_color: wgpu::TextureView,
    msaa_depth: wgpu::TextureView,
    scene: wgpu::TextureView,
    depth: wgpu::TextureView,
    haze: wgpu::TextureView,
    haze_history: [wgpu::TextureView; 2],
    /// Three independent presentation resources. Intermediate passes may be
    /// shared because queue submissions execute in order; each submission's
    /// final output and readback must remain private until its async map retires.
    presentations: [PresentationTarget; 3],
    /// 256-byte-aligned row pitch of [`Self::readback`].
    bytes_per_row: u32,
}

struct PresentationTarget {
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    readback: wgpu::Buffer,
}

pub(crate) struct PendingReadback {
    readback: wgpu::Buffer,
    mapped: mpsc::Receiver<Result<(), String>>,
    width: u32,
    height: u32,
    bytes_per_row: u32,
    started: Instant,
    mapped_result: Option<Result<(), String>>,
    profile: Option<PendingProfile>,
}

struct PendingProfile {
    readback: wgpu::Buffer,
    mapped: mpsc::Receiver<Result<(), String>>,
    mapped_result: Option<Result<(), String>>,
    timestamp_period_ns: f32,
    cpu_encode_submit: Duration,
    cpu_cluster: Duration,
}

pub(crate) struct CompletedReadback {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
    pub(crate) draw_time: Duration,
    pub(crate) profile: Option<FrameTimings>,
}

impl Renderer {
    /// # Errors
    /// Fails when no wgpu adapter or device can be acquired.
    pub fn new() -> anyhow::Result<Self> {
        Self::new_inner(false)
    }

    /// Acquire a renderer with hardware timestamp queries enabled.
    ///
    /// This constructor is deliberately separate: normal and asynchronous
    /// presentation pay no query or mapping overhead.
    ///
    /// # Errors
    /// Fails when no GPU exists or the selected adapter has no timestamp-query
    /// support. Profiling never substitutes CPU wall time for GPU evidence.
    pub fn new_profiled() -> anyhow::Result<Self> {
        Self::new_inner(true)
    }

    fn new_inner(profiled: bool) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))?;
        let timestamp_query_supported =
            adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        anyhow::ensure!(
            !profiled || timestamp_query_supported,
            "selected GPU adapter does not support timestamp queries"
        );
        let required_features = if profiled {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("luma-render"),
                required_features,
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            }))?;
        let adapter_info = adapter.get_info();
        let adapter_profile = RendererProfile {
            name: adapter_info.name,
            backend: format!("{:?}", adapter_info.backend),
            device_type: format!("{:?}", adapter_info.device_type),
            driver: adapter_info.driver,
            driver_info: adapter_info.driver_info,
            timestamp_query_supported,
            timestamp_period_ns: timestamp_query_supported.then(|| queue.get_timestamp_period()),
        };

        let profiler = profiled.then(|| ProfilerResources {
            slots: std::array::from_fn(|_| ProfilerSlot {
                query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("luma-profile-timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 4,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("luma-profile-resolve"),
                    size: 32,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("luma-profile-readback"),
                    size: 32,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
            }),
            timestamp_period_ns: queue.get_timestamp_period(),
        });
        let environment = EnvironmentSystem::new(&device, &queue);

        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                storage_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT),
                storage_entry(2, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let cluster_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("surface-clusters"),
            entries: &[
                storage_entry(0, wgpu::ShaderStages::FRAGMENT),
                storage_entry(1, wgpu::ShaderStages::FRAGMENT),
                storage_entry(2, wgpu::ShaderStages::FRAGMENT),
                storage_entry(3, wgpu::ShaderStages::FRAGMENT),
                uniform_entry(4, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        // Group 1 is the per-draw material texture. glTF's `baseColorTexture`
        // is sRGB-encoded, so the view format decodes on sample and the shader
        // multiplies in linear space, as three does.
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let haze_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                storage_entry(1, wgpu::ShaderStages::FRAGMENT),
                storage_entry(2, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                storage_entry(4, wgpu::ShaderStages::FRAGMENT),
                storage_entry(5, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        let temporal_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze-temporal"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                texture_entry(1),
                texture_entry(2),
            ],
        });

        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        // Both scene-geometry shaders open with the shared bind-group
        // declarations; see `scene_bindings.wgsl`.
        let bindings = include_str!("shaders/scene_bindings.wgsl");
        let fixture_light = include_str!("shaders/fixture_light.wgsl");
        let scene_module = shader(
            &device,
            "scene",
            &format!(
                "{bindings}{fixture_light}{}",
                include_str!("shaders/scene.wgsl")
            ),
        );
        let haze_module = shader(
            &device,
            "haze",
            &format!("{fixture_light}{}", include_str!("shaders/haze.wgsl")),
        );
        let temporal_module = shader(
            &device,
            "haze-temporal",
            include_str!("shaders/haze_temporal.wgsl"),
        );
        let composite_module = shader(&device, "composite", include_str!("shaders/composite.wgsl"));
        let grid_module = shader(
            &device,
            "grid",
            &format!("{bindings}{}", include_str!("shaders/grid.wgsl")),
        );

        let scene_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("scene"),
                bind_group_layouts: &[
                    &scene_layout,
                    &material_layout,
                    environment.scene_layout(),
                    &cluster_layout,
                ],
                push_constant_ranges: &[],
            });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: 48,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 12,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 24,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 3,
                },
            ],
        };

        let scene_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_main"),
                buffers: std::slice::from_ref(&vertex_layout),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &scene_module,
                entry_point: Some("fs_main"),
                targets: &[Some(SCENE_FORMAT.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });

        let depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth-prepass"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_main"),
                buffers: std::slice::from_ref(&vertex_layout),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let grid_vertex_layout = vertex_layout.clone();
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_depth"),
                buffers: &[vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let haze_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("haze"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("haze"),
                    bind_group_layouts: &[&haze_layout],
                    push_constant_ranges: &[],
                }),
            ),
            vertex: wgpu::VertexState {
                module: &haze_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &haze_module,
                entry_point: Some("fs_main"),
                // Subframes accumulate additively; each carries weight 1/K.
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: ADD,
                        alpha: ADD,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let temporal_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("haze-temporal"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("haze-temporal"),
                    bind_group_layouts: &[&temporal_layout],
                    push_constant_ranges: &[],
                }),
            ),
            vertex: wgpu::VertexState {
                module: &temporal_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &temporal_module,
                entry_point: Some("fs_main"),
                targets: &[Some(SCENE_FORMAT.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("composite"),
                bind_group_layouts: &[&composite_layout, environment.scene_layout()],
                push_constant_ranges: &[],
            });
        let composite_pipelines = [Channels::Rgba, Channels::Bgra].map(|channels| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("composite"),
                layout: Some(&composite_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &composite_module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(channels.format().into())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        });

        let grid_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &grid_module,
                entry_point: Some("vs_main"),
                buffers: &[grid_vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &grid_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            // `depthWrite: false` — the grid tests against the stage but never
            // occludes it.
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });

        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX),
                storage_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT),
            ],
        });
        let overlay_module = shader(&device, "overlay", include_str!("shaders/overlay.wgsl"));
        let overlay_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay"),
                bind_group_layouts: &[&overlay_layout],
                push_constant_ranges: &[],
            });
        let overlay_position_layout = wgpu::VertexBufferLayout {
            array_stride: 48,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            }],
        };
        let overlay_pipelines = std::array::from_fn(|i| {
            let channels = if i < 4 {
                Channels::Rgba
            } else {
                Channels::Bgra
            };
            let variant = i % 4;
            let lines = variant & 1 == 1;
            let free = variant & 2 == 2;
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("overlay"),
                layout: Some(&overlay_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &overlay_module,
                    entry_point: Some("vs_main"),
                    buffers: std::slice::from_ref(&overlay_position_layout),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &overlay_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: channels.format(),
                        blend: free.then_some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: if lines {
                        wgpu::PrimitiveTopology::LineList
                    } else {
                        wgpu::PrimitiveTopology::TriangleList
                    },
                    // three's `MeshBasicMaterial` is `FrontSide`; the gizmo's
                    // plane quads are one-sided and the hide rules assume it.
                    cull_mode: (!lines).then_some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(if free {
                    wgpu::DepthStencilState {
                        depth_compare: wgpu::CompareFunction::Always,
                        ..depth_state(false)
                    }
                } else {
                    depth_state(true)
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        });

        let (shadow_map, shadow_layers) = shadow_texture_array(
            &device,
            SHADOW_SIZE,
            SHADOW_SIZE,
            CASCADE_COUNT as u32,
            "shadow-cascades",
        );
        let (dummy_shadow, _) = shadow_texture_array(&device, 1, 1, 1, "shadow-placeholder");
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::GreaterEqual),
            ..Default::default()
        });
        let hard_shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hard-shadow"),
            compare: Some(wgpu::CompareFunction::GreaterEqual),
            ..Default::default()
        });
        // glTF sampler `wrapS/wrapT = REPEAT`, trilinear — three's default for
        // an imported texture, and the mip chain is what keeps the deck's wood
        // grain from aliasing into a bright fizz at grazing angles.
        let texture_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let one_pixel = |rgba: [u8; 4], encoding| {
            upload_texture_view(
                &device,
                &queue,
                &Image {
                    width: 1,
                    height: 1,
                    rgba: std::sync::Arc::from(rgba),
                },
                encoding,
            )
        };
        let material_defaults = MaterialDefaults {
            base_color: one_pixel([255; 4], TextureEncoding::Srgb),
            normal: one_pixel([128, 128, 255, 255], TextureEncoding::Linear),
            metallic_roughness: one_pixel([255; 4], TextureEncoding::Linear),
            occlusion: one_pixel([255; 4], TextureEncoding::Linear),
            // The glTF identity for an absent emissive texture is white. The
            // factor, not a synthetic black map, is what disables emission.
            // This also preserves procedural emissive materials, which never
            // carry a glTF image.
            emissive: one_pixel([255; 4], TextureEncoding::Srgb),
        };
        let white_material = material_bind_group(
            &device,
            &material_layout,
            &texture_sampler,
            &material_defaults.base_color,
            &material_defaults.normal,
            &material_defaults.metallic_roughness,
            &material_defaults.occlusion,
            &material_defaults.emissive,
        );

        Ok(Self {
            device,
            queue,
            adapter_profile,
            scene_layout,
            material_layout,
            environment,
            cluster_layout,
            haze_layout,
            temporal_layout,
            composite_layout,
            scene_pipeline,
            depth_pipeline,
            shadow_pipeline,
            haze_pipeline,
            temporal_pipeline,
            composite_pipelines,
            grid_pipeline,
            overlay_layout,
            overlay_pipelines,
            shadow_map,
            shadow_layers,
            hard_shadow_sampler,
            shadow_sampler,
            dummy_shadow,
            linear_sampler,
            texture_sampler,
            white_material,
            material_defaults,
            texture_views: HashMap::new(),
            materials: HashMap::new(),
            geometry: None,
            upload_stats: UploadStats {
                geometry: 0,
                textures: 0,
                environments: 0,
            },
            targets: None,
            haze_history_valid: false,
            haze_history_index: 0,
            haze_history_key: None,
            last_live_time: None,
            live_noise_frame: 0,
            profiler,
            haze_tile_cache: None,
            cluster_cache: ClusterCache::default(),
            cluster_gpu: None,
            cluster_stats: ClusterStats::default(),
        })
    }

    fn targets(
        &mut self,
        width: u32,
        height: u32,
        haze: (u32, u32),
        channels: Channels,
    ) -> &Targets {
        let stale = self.targets.as_ref().is_none_or(|t| {
            t.width != width
                || t.height != height
                || t.channels != channels
                || (t.haze_width, t.haze_height) != haze
        });
        if stale {
            self.haze_history_valid = false;
            self.haze_history_key = None;
            let color = |w, h, samples, usage, label| {
                self.device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format: SCENE_FORMAT,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            let bytes_per_row = (width * 4).div_ceil(256) * 256;
            let presentations = std::array::from_fn(|slot| {
                let output = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("output"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: channels.format(),
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
                PresentationTarget {
                    output,
                    output_view,
                    readback: self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(match slot {
                            0 => "readback-0",
                            1 => "readback-1",
                            _ => "readback-2",
                        }),
                        size: u64::from(bytes_per_row * height),
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                }
            });
            self.targets = Some(Targets {
                width,
                height,
                haze_width: haze.0,
                haze_height: haze.1,
                channels,
                msaa_color: color(
                    width,
                    height,
                    MSAA_SAMPLES,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                    "scene-msaa",
                ),
                msaa_depth: depth_texture(&self.device, width, height, MSAA_SAMPLES, "depth-msaa"),
                scene: color(
                    width,
                    height,
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    "scene",
                ),
                depth: depth_texture(&self.device, width, height, 1, "depth"),
                haze: color(
                    haze.0,
                    haze.1,
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    "haze",
                ),
                haze_history: std::array::from_fn(|_| {
                    color(
                        haze.0,
                        haze.1,
                        1,
                        wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        "haze-history",
                    )
                }),
                presentations,
                bytes_per_row,
            });
        }
        self.targets.as_ref().expect("just populated")
    }

    /// Render one frame and read it back as sRGB-encoded RGBA8, row-major, no
    /// padding.
    ///
    /// `subframes` is the jitter-accumulation count (spec §6): the same jitter
    /// primitive the live temporal pass uses, applied deterministically.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub fn render(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
    ) -> anyhow::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.render_into(frame, width, height, subframes, Channels::Rgba, &mut out)?;
        Ok(out)
    }

    /// Measure one frame through the production temporal/live pass chain.
    ///
    /// # Errors
    /// Fails if this renderer was not created by [`Self::new_profiled`] or if
    /// either GPU readback cannot be mapped.
    pub fn profile_live_frame(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
    ) -> anyhow::Result<FrameTimings> {
        anyhow::ensure!(
            self.profiler.is_some(),
            "renderer was not created for profiling"
        );
        let mut pending = self.submit_readback(
            frame,
            width.max(1),
            height.max(1),
            subframes,
            Channels::Rgba,
            0,
            true,
            true,
        );
        self.device.poll(wgpu::PollType::Wait)?;
        pending
            .try_complete()?
            .expect("blocking poll must complete timestamp and pixel readbacks")
            .profile
            .ok_or_else(|| anyhow::anyhow!("profile query resources were unavailable"))
    }

    /// Read cumulative immutable-resource upload counts.
    #[must_use]
    pub fn upload_stats(&self) -> UploadStats {
        self.upload_stats
    }

    /// Read metrics for the most recently submitted surface cluster grid.
    #[must_use]
    pub fn cluster_stats(&self) -> ClusterStats {
        self.cluster_stats
    }

    /// Adapter identity for reproducible performance evidence.
    #[must_use]
    pub fn adapter_profile(&self) -> &RendererProfile {
        &self.adapter_profile
    }

    /// [`Self::render`], reading back into a caller-owned buffer in a caller-
    /// chosen channel order.
    ///
    /// Both exist because a viewport draws sixty of these a second: `out` is
    /// reused rather than reallocated, and the swizzle a presentation layer
    /// would otherwise do in a second pass over 25 MB happens inside the one
    /// copy that was already walking the rows.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub(crate) fn render_into(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        channels: Channels,
        out: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        let mut pending =
            self.submit_readback(frame, width, height, subframes, channels, 0, false, false);
        self.device.poll(wgpu::PollType::Wait)?;
        let completed = pending
            .try_complete()?
            .expect("blocking poll must complete the mapped readback");
        *out = completed.pixels;
        Ok(())
    }

    pub(crate) fn render_live_into(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        channels: Channels,
        out: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        let mut pending =
            self.submit_readback(frame, width, height, subframes, channels, 0, true, false);
        self.device.poll(wgpu::PollType::Wait)?;
        let completed = pending
            .try_complete()?
            .expect("blocking poll must complete the mapped readback");
        *out = completed.pixels;
        Ok(())
    }

    pub(crate) fn submit_live(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        slot: usize,
        measure: bool,
    ) -> PendingReadback {
        self.submit_readback(
            frame,
            width,
            height,
            subframes,
            Channels::Bgra,
            slot,
            true,
            measure,
        )
    }

    pub(crate) fn poll_live(&self) -> anyhow::Result<()> {
        self.device.poll(wgpu::PollType::Poll)?;
        Ok(())
    }

    fn submit_readback(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        channels: Channels,
        slot: usize,
        temporal: bool,
        measure: bool,
    ) -> PendingReadback {
        assert!(slot < 3, "presentation slot is bounded to three");
        let started = Instant::now();
        let profile = self.profiler.as_ref().filter(|_| measure).map(|profile| {
            let resources = &profile.slots[slot];
            (
                resources.query_set.clone(),
                resources.resolve.clone(),
                resources.readback.clone(),
                profile.timestamp_period_ns,
            )
        });
        let aspect = width as f32 / height as f32;
        // Passing the bounds in reverse order produces a finite reverse-Z
        // projection: near maps to one and the bounded far plane maps to zero.
        let proj = Mat4::perspective_rh(
            frame.camera.fov_y_deg.to_radians(),
            aspect,
            CAMERA_FAR,
            CAMERA_NEAR,
        );
        let view = Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z);
        let view_proj = proj * view;

        let camera_forward = (frame.camera.target - frame.camera.eye).normalize_or(Vec3::Y);
        let light_view_proj = frame
            .directional
            .map_or([Mat4::IDENTITY; CASCADE_COUNT], |light| {
                cascade_matrices(
                    frame.camera.eye,
                    camera_forward,
                    frame.camera.fov_y_deg.to_radians(),
                    aspect,
                    light.direction,
                )
            });

        let point_lights: Vec<PointLightGpu> = frame
            .point_lights
            .iter()
            .filter(|l| l.intensity > 0.0)
            .map(|l| PointLightGpu {
                position: l.position.extend(l.cutoff_distance).to_array(),
                color: (l.color * l.intensity).extend(0.0).to_array(),
            })
            .collect();

        let fixture_cones: Vec<_> = frame
            .fixture_cones
            .iter()
            .take(crate::frame::MAX_FIXTURE_CONES)
            .map(sanitize_fixture_cone)
            .collect();
        let cores: Vec<LightCore> = fixture_cones
            .iter()
            .map(|light| LightCore {
                position: light.position.to_array(),
                range: light.range.clamp(0.05, 100.0),
            })
            .collect();
        let rests: Vec<LightRest> = fixture_cones
            .iter()
            .map(|light| LightRest {
                direction: light
                    .direction
                    .try_normalize()
                    .unwrap_or(Vec3::NEG_Y)
                    .to_array(),
                cos_beam: light.cos_beam.clamp(-1.0, 1.0),
                color: light.color.to_array(),
                intensity: light.intensity.clamp(0.0, 100.0),
                cos_field: light.cos_field.clamp(-1.0, 1.0),
                wash: light.wash.clamp(0.0, 1.0),
                gobo: light.gobo.min(2) as f32,
                gobo_rotation: light.gobo_rotation.rem_euclid(std::f32::consts::TAU),
            })
            .collect();
        let cluster_started = Instant::now();
        let (rebuilt, columns, rows, cluster_near, cluster_far, headers, indices, stats) = {
            let (grid, rebuilt) = self
                .cluster_cache
                .get_or_build(
                    &fixture_cones,
                    frame.camera,
                    [width, height],
                    CAMERA_NEAR,
                    CAMERA_FAR,
                )
                .expect("bounded render target must produce a safe cluster grid");
            let stats = ClusterStats {
                occupied_clusters: grid
                    .headers
                    .iter()
                    .filter(|header| header.count > 0)
                    .count(),
                light_references: grid.light_indices.len(),
                max_lights_per_cluster: grid
                    .headers
                    .iter()
                    .map(|header| header.count)
                    .max()
                    .unwrap_or(0),
                rebuilds: self.cluster_stats.rebuilds + u64::from(rebuilt),
            };
            (
                rebuilt,
                grid.columns,
                grid.rows,
                grid.near(),
                grid.far(),
                rebuilt.then(|| {
                    grid.headers
                        .iter()
                        .map(|header| SurfaceClusterHeader {
                            offset: header.offset,
                            count: header.count,
                        })
                        .collect::<Vec<_>>()
                }),
                rebuilt.then(|| grid.light_indices.clone()),
                stats,
            )
        };
        self.cluster_stats = stats;
        if rebuilt || self.cluster_gpu.is_none() {
            self.cluster_gpu = Some(SurfaceClusterGpu {
                headers: self.storage(
                    &pad_at_least_one(headers.unwrap_or_default()),
                    wgpu::BufferUsages::STORAGE,
                    "surface-cluster-headers",
                ),
                indices: self.storage(
                    &pad_at_least_one(indices.unwrap_or_default()),
                    wgpu::BufferUsages::STORAGE,
                    "surface-cluster-indices",
                ),
                columns,
                rows,
                near: cluster_near,
                far: cluster_far,
            });
        }
        let cpu_cluster = if rebuilt {
            cluster_started.elapsed()
        } else {
            Duration::ZERO
        };

        let globals = Globals {
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
                point_lights.len() as f32,
                1.0 / SHADOW_SIZE as f32,
                f32::from(u8::from(
                    frame.directional.is_some_and(|light| light.shadows),
                )),
                frame.debug_view.shader_code() as f32,
            ],
        };
        if self
            .environment
            .prepare(&self.device, &self.queue, frame.environment.as_ref())
        {
            self.upload_stats.environments += 1;
        }
        let (_environment_uniform, environment_bg) = self
            .environment
            .bind_group(&self.device, frame.environment.as_ref());

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
            for mesh in &frame.meshes {
                let base_vertex = vertices.len() as i32;
                let first_index = indices.len() as u32;
                vertices.extend_from_slice(&mesh.vertices);
                indices.extend(mesh.indices.iter().copied());
                ranges.push((first_index, indices.len() as u32, base_vertex));
            }
            let geometry = ResidentGeometry {
                keys: frame.meshes.iter().map(|mesh| mesh.key.clone()).collect(),
                vertices: self.storage(&vertices, wgpu::BufferUsages::VERTEX, "vertices"),
                indices: self.storage(&indices, wgpu::BufferUsages::INDEX, "indices"),
                ranges,
            };
            self.geometry = Some(geometry);
            self.upload_stats.geometry += 1;
        }
        let geometry = self.geometry.as_ref().expect("frame geometry uploaded");
        let vertex_buf = geometry.vertices.clone();
        let index_buf = geometry.indices.clone();
        let ranges = geometry.ranges.clone();

        let instances: Vec<Instance> = frame.draws.iter().map(instance_of).collect();
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
                        &self.device,
                        &self.queue,
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
                &self.device,
                &self.material_layout,
                &self.texture_sampler,
                view(key.base_color.as_ref(), &self.material_defaults.base_color),
                view(key.normal.as_ref(), &self.material_defaults.normal),
                view(
                    key.metallic_roughness.as_ref(),
                    &self.material_defaults.metallic_roughness,
                ),
                view(key.occlusion.as_ref(), &self.material_defaults.occlusion),
                view(key.emissive.as_ref(), &self.material_defaults.emissive),
            );
            self.materials.insert(key.clone(), bind_group);
        }
        let materials: Vec<wgpu::BindGroup> = material_keys
            .iter()
            .map(|key| self.materials[key].clone())
            .collect();

        let instance_buf = self.storage(&instances, wgpu::BufferUsages::STORAGE, "instances");
        let point_buf = self.storage(
            &pad_at_least_one(point_lights),
            wgpu::BufferUsages::STORAGE,
            "point-lights",
        );
        let globals_buf = self.storage(&[globals], wgpu::BufferUsages::UNIFORM, "globals");
        let core_buf = self.storage(
            &pad_at_least_one(cores),
            wgpu::BufferUsages::STORAGE,
            "light-core",
        );
        let rest_buf = self.storage(
            &pad_at_least_one(rests),
            wgpu::BufferUsages::STORAGE,
            "light-rest",
        );
        let cluster_gpu = self.cluster_gpu.as_ref().expect("cluster cache populated");
        let cluster_uniform = self.storage(
            &[SurfaceClusterUniform {
                grid: [
                    cluster_gpu.columns,
                    cluster_gpu.rows,
                    CLUSTER_TILE_SIZE,
                    CLUSTER_DEPTH_SLICES,
                ],
                depth_and_flags: [
                    cluster_gpu.near,
                    cluster_gpu.far,
                    f32::from(u8::from(frame.fixture_surface_lighting)),
                    f32::from(u8::from(frame.cluster_debug)),
                ],
            }],
            wgpu::BufferUsages::UNIFORM,
            "surface-cluster-uniform",
        );
        let cluster_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("surface-clusters"),
            layout: &self.cluster_layout,
            entries: &[
                binding(0, core_buf.as_entire_binding()),
                binding(1, rest_buf.as_entire_binding()),
                binding(2, cluster_gpu.headers.as_entire_binding()),
                binding(3, cluster_gpu.indices.as_entire_binding()),
                binding(4, cluster_uniform.as_entire_binding()),
            ],
        });
        let overlay_buf = self.storage(
            &pad_at_least_one(overlay_instances),
            wgpu::BufferUsages::STORAGE,
            "overlays",
        );
        let overlay_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay"),
            layout: &self.overlay_layout,
            entries: &[
                binding(0, globals_buf.as_entire_binding()),
                binding(1, overlay_buf.as_entire_binding()),
            ],
        });

        let hard_shadows = frame
            .directional
            .is_some_and(|light| light.shadow_softness == 0.0);
        let lit_bg =
            self.scene_bind_group(&globals_buf, &instance_buf, &point_buf, true, hard_shadows);
        let unlit_bg = self.scene_bind_group(&globals_buf, &instance_buf, &point_buf, false, false);
        let shadow_bgs: Vec<_> = light_view_proj
            .iter()
            .map(|matrix| {
                let mut shadow_globals = globals;
                shadow_globals.light_view_proj[0] = matrix.to_cols_array_2d();
                let buffer = self.storage(
                    &[shadow_globals],
                    wgpu::BufferUsages::UNIFORM,
                    "shadow-globals",
                );
                (
                    buffer.clone(),
                    self.scene_bind_group(&buffer, &instance_buf, &point_buf, false, false),
                )
            })
            .collect();

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
        let (
            msaa_color,
            msaa_depth,
            scene_view,
            depth_view,
            haze_view,
            haze_history,
            output,
            output_view,
            readback,
            bytes_per_row,
        ) = {
            let t = self.targets(t_width, t_height, haze_size, channels);
            let presentation = &t.presentations[slot];
            (
                t.msaa_color.clone(),
                t.msaa_depth.clone(),
                t.scene.clone(),
                t.depth.clone(),
                t.haze.clone(),
                t.haze_history.clone(),
                presentation.output.clone(),
                presentation.output_view.clone(),
                presentation.readback.clone(),
                t.bytes_per_row,
            )
        };
        let haze_density = frame
            .haze_density
            .is_finite()
            .then_some(frame.haze_density.clamp(0.0, 4.0))
            .unwrap_or(0.0);
        let haze_time = frame.time.is_finite().then_some(frame.time).unwrap_or(0.0);
        let history_key = haze_history_key(frame, width, height, haze_size, haze_density);
        let time_continuous = self
            .last_live_time
            .is_some_and(|previous| (-0.01..=0.25).contains(&(frame.time - previous)));
        let history_valid = temporal
            && self.haze_history_valid
            && time_continuous
            && self.haze_history_key.as_ref() == Some(&history_key);
        let noise_seed = if temporal && history_valid {
            self.live_noise_frame
        } else {
            0
        };
        if temporal {
            self.haze_history_key = Some(history_key);
            self.last_live_time = Some(frame.time);
            self.live_noise_frame = if history_valid {
                self.live_noise_frame.wrapping_add(1)
            } else {
                1
            };
        } else {
            self.haze_history_valid = false;
            self.haze_history_key = None;
            self.last_live_time = None;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        {
            let opaque = frame.draws.len() - frame.grid_draws;
            let draw_range = |pass: &mut wgpu::RenderPass, range: std::ops::Range<usize>| {
                pass.set_vertex_buffer(0, vertex_buf.slice(..));
                pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                let mut bound: Option<usize> = None;
                pass.set_bind_group(1, &self.white_material, &[]);
                for i in range {
                    let draw = &frame.draws[i];
                    if bound != Some(i) {
                        bound = Some(i);
                        pass.set_bind_group(1, &materials[i], &[]);
                    }
                    let (first, last, base) = ranges[draw.mesh];
                    pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                }
            };

            if frame.directional.is_some_and(|light| light.shadows) {
                for (cascade, layer) in self.shadow_layers.iter().enumerate() {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("shadow-cascade"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(depth_attachment(layer)),
                        timestamp_writes: (cascade == 0)
                            .then(|| {
                                profile.as_ref().map(|(queries, ..)| {
                                    wgpu::RenderPassTimestampWrites {
                                        query_set: queries,
                                        beginning_of_pass_write_index: Some(0),
                                        end_of_pass_write_index: None,
                                    }
                                })
                            })
                            .flatten(),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.shadow_pipeline);
                    pass.set_bind_group(0, &shadow_bgs[cascade].1, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_range(&mut pass, 0..opaque);
                }
            }

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("depth-prepass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(depth_attachment(&depth_view)),
                    timestamp_writes: profile.as_ref().and_then(|(queries, ..)| {
                        (!frame.directional.is_some_and(|light| light.shadows)).then_some(
                            wgpu::RenderPassTimestampWrites {
                                query_set: queries,
                                beginning_of_pass_write_index: Some(0),
                                end_of_pass_write_index: None,
                            },
                        )
                    }),
                    ..Default::default()
                });
                pass.set_pipeline(&self.depth_pipeline);
                pass.set_bind_group(0, &unlit_bg, &[]);
                pass.set_bind_group(2, &environment_bg, &[]);
                pass.set_bind_group(3, &cluster_bg, &[]);
                draw_range(&mut pass, 0..opaque);
            }

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("scene"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &msaa_color,
                        resolve_target: Some(&scene_view),
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(frame.clear_color.x),
                                g: f64::from(frame.clear_color.y),
                                b: f64::from(frame.clear_color.z),
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(depth_attachment(&msaa_depth)),
                    ..Default::default()
                });
                pass.set_pipeline(&self.scene_pipeline);
                pass.set_bind_group(0, &lit_bg, &[]);
                pass.set_bind_group(2, &environment_bg, &[]);
                pass.set_bind_group(3, &cluster_bg, &[]);
                draw_range(&mut pass, 0..opaque);
                if frame.grid_draws > 0 {
                    pass.set_pipeline(&self.grid_pipeline);
                    draw_range(&mut pass, opaque..frame.draws.len());
                }
            }
        }

        // --- haze ------------------------------------------------------------
        let tile_key = haze_tile_key(&fixture_cones, frame.camera, haze_size);
        if self
            .haze_tile_cache
            .as_ref()
            .is_none_or(|cache| cache.key != tile_key)
        {
            let (headers, indices, columns, rows) =
                haze_tiles(&fixture_cones, view_proj, haze_size.0, haze_size.1);
            self.haze_tile_cache = Some(HazeTileCache {
                key: tile_key,
                headers: self.storage(
                    &pad_at_least_one(headers),
                    wgpu::BufferUsages::STORAGE,
                    "haze-tile-headers",
                ),
                indices: self.storage(
                    &pad_at_least_one(indices),
                    wgpu::BufferUsages::STORAGE,
                    "haze-tile-indices",
                ),
                columns,
                rows,
            });
        }
        let tile_cache = self.haze_tile_cache.as_ref().expect("tile cache populated");
        let tile_header_buf = tile_cache.headers.clone();
        let tile_index_buf = tile_cache.indices.clone();
        let (tile_columns, tile_rows) = (tile_cache.columns, tile_cache.rows);

        let inv_view_proj = view_proj.inverse();
        let subframes = subframes.max(1);
        let weight = 1.0 / subframes as f32;
        for k in 0..subframes {
            let uniform = HazeUniform {
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
                    (noise_seed.wrapping_add(k) & 4095) as f32,
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
                tiles: [
                    tile_columns as f32,
                    tile_rows as f32,
                    HAZE_TILE_SIZE as f32,
                    0.0,
                ],
                depth: [CAMERA_NEAR, CAMERA_FAR, 0.0, 0.0],
            };
            let haze_buf = self.storage(&[uniform], wgpu::BufferUsages::UNIFORM, "haze");
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("haze"),
                layout: &self.haze_layout,
                entries: &[
                    binding(0, haze_buf.as_entire_binding()),
                    binding(1, core_buf.as_entire_binding()),
                    binding(2, rest_buf.as_entire_binding()),
                    binding(3, wgpu::BindingResource::TextureView(&depth_view)),
                    binding(4, tile_header_buf.as_entire_binding()),
                    binding(5, tile_index_buf.as_entire_binding()),
                ],
            });
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
                timestamp_writes: profile.as_ref().and_then(|(queries, ..)| {
                    (k == 0).then_some(wgpu::RenderPassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: Some(1),
                        end_of_pass_write_index: None,
                    })
                }),
                ..Default::default()
            });
            pass.set_pipeline(&self.haze_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        let composite_haze = if temporal {
            let read = self.haze_history_index;
            let write = 1 - read;
            let temporal_uniform = TemporalUniform {
                // Reject history when the represented surface moves by more
                // than 25 cm in linear view space.
                params: [0.82, f32::from(u8::from(history_valid)), 0.25, 0.0],
            };
            let uniform_buf = self.storage(
                &[temporal_uniform],
                wgpu::BufferUsages::UNIFORM,
                "haze-temporal",
            );
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("haze-temporal"),
                layout: &self.temporal_layout,
                entries: &[
                    binding(0, uniform_buf.as_entire_binding()),
                    binding(1, wgpu::BindingResource::TextureView(&haze_view)),
                    binding(2, wgpu::BindingResource::TextureView(&haze_history[read])),
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("haze-temporal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &haze_history[write],
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_pipeline(&self.temporal_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
            drop(pass);
            self.haze_history_index = write;
            self.haze_history_valid = true;
            haze_history[write].clone()
        } else {
            haze_view.clone()
        };

        // --- composite + readback --------------------------------------------
        let composite_uniform = CompositeUniform {
            inv_view_proj: inv_view_proj.to_cols_array_2d(),
            params: [
                haze_size.0 as f32,
                haze_size.1 as f32,
                // Bilateral sigma in metres of linear view depth.
                0.25,
                frame.debug_view.shader_code() as f32,
            ],
            depth: [CAMERA_NEAR, CAMERA_FAR, 0.0, 0.0],
        };
        let composite_buf = self.storage(
            &[composite_uniform],
            wgpu::BufferUsages::UNIFORM,
            "composite",
        );
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite"),
            layout: &self.composite_layout,
            entries: &[
                binding(0, composite_buf.as_entire_binding()),
                binding(1, wgpu::BindingResource::TextureView(&scene_view)),
                binding(2, wgpu::BindingResource::TextureView(&composite_haze)),
                binding(3, wgpu::BindingResource::Sampler(&self.linear_sampler)),
                binding(4, wgpu::BindingResource::TextureView(&depth_view)),
            ],
        });
        {
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("composite"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &output_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: profile.as_ref().map(|(queries, ..)| {
                        wgpu::RenderPassTimestampWrites {
                            query_set: queries,
                            beginning_of_pass_write_index: Some(2),
                            end_of_pass_write_index: None,
                        }
                    }),
                    ..Default::default()
                });
                pass.set_pipeline(&self.composite_pipelines[channels.index()]);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_bind_group(1, &environment_bg, &[]);
                pass.draw(0..3, 0..1);
            }

            // Editor affordances are display UI, not scene radiance. Drawing
            // into the final sRGB target after AgX makes authored colours
            // independent of stage lighting and exposure. Cages load the
            // full-resolution reverse-Z prepass depth; free gizmos use Always.
            if !frame.overlays.is_empty() {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("editor-overlays"),
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
                        &self.overlay_pipelines[overlay_pipeline_index(overlay, channels)],
                    );
                    let (first, last, base) = ranges[overlay.mesh];
                    pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                }
            }
            if let Some((queries, resolve, profile_readback, _)) = &profile {
                // Metal returns zero for the end timestamp of the final pass
                // and optimizes a fully empty fence away. A zero-vertex draw
                // makes this next ordered pass observable without executing a
                // shader. Its beginning proves composite and editor overlays
                // completed; the fence body remains outside the interval.
                let mut fence = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("luma-profile-fence"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &output_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: Some(3),
                        end_of_pass_write_index: None,
                    }),
                    ..Default::default()
                });
                fence.set_pipeline(&self.composite_pipelines[channels.index()]);
                fence.set_bind_group(0, &bind_group, &[]);
                fence.set_bind_group(1, &environment_bg, &[]);
                fence.draw(0..0, 0..1);
                drop(fence);
                encoder.resolve_query_set(queries, 0..4, resolve, 0);
                encoder.copy_buffer_to_buffer(resolve, 0, profile_readback, 0, 32);
            }
            encoder.copy_texture_to_buffer(
                output.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
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

        self.queue.submit([encoder.finish()]);
        let cpu_encode_submit = started.elapsed();
        let (mapped_tx, mapped) = mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = mapped_tx.send(result.map_err(|error| error.to_string()));
            });
        let pending_profile = profile.map(|(_, _, profile_readback, timestamp_period_ns)| {
            let (mapped_tx, mapped) = mpsc::sync_channel(1);
            profile_readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = mapped_tx.send(result.map_err(|error| error.to_string()));
                });
            PendingProfile {
                readback: profile_readback,
                mapped,
                mapped_result: None,
                timestamp_period_ns,
                cpu_encode_submit,
                cpu_cluster,
            }
        });
        PendingReadback {
            readback,
            mapped,
            width: t_width,
            height: t_height,
            bytes_per_row,
            started,
            mapped_result: None,
            profile: pending_profile,
        }
    }

    fn scene_bind_group(
        &self,
        globals: &wgpu::Buffer,
        instances: &wgpu::Buffer,
        point_lights: &wgpu::Buffer,
        shadows: bool,
        hard_shadows: bool,
    ) -> wgpu::BindGroup {
        // The shadow map is a render target during the shadow pass, so the
        // passes that write depth bind a 1x1 placeholder instead.
        let map = if shadows {
            &self.shadow_map
        } else {
            &self.dummy_shadow
        };
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout: &self.scene_layout,
            entries: &[
                binding(0, globals.as_entire_binding()),
                binding(1, instances.as_entire_binding()),
                binding(2, point_lights.as_entire_binding()),
                binding(3, wgpu::BindingResource::TextureView(map)),
                binding(
                    4,
                    wgpu::BindingResource::Sampler(if hard_shadows {
                        &self.hard_shadow_sampler
                    } else {
                        &self.shadow_sampler
                    }),
                ),
            ],
        })
    }

    fn storage<T: Pod>(&self, data: &[T], usage: wgpu::BufferUsages, label: &str) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(data),
                usage,
            })
    }
}

/// Upload one immutable material map with a full mip chain. Color maps use an
/// sRGB view, while normal/metallic-roughness/AO maps remain linear data.
fn upload_texture_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &Image,
    encoding: TextureEncoding,
) -> wgpu::TextureView {
    let levels = 32 - image.width.max(image.height).leading_zeros();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("material-map"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: match encoding {
            TextureEncoding::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            TextureEncoding::Linear => wgpu::TextureFormat::Rgba8Unorm,
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let mut level = (image.width, image.height, image.rgba.to_vec());
    for mip in 0..levels {
        let (w, h, ref pixels) = level;
        // Rows go up 256-byte aligned: that is the copy alignment every backend
        // agrees on, and a tightly packed odd width otherwise lands skewed.
        let row = (w * 4).div_ceil(256) * 256;
        let mut padded = vec![0u8; (row * h) as usize];
        for y in 0..h as usize {
            let src = y * (w * 4) as usize;
            let dst = y * row as usize;
            padded[dst..dst + (w * 4) as usize]
                .copy_from_slice(&pixels[src..src + (w * 4) as usize]);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        if mip + 1 < levels {
            level = downsample(w, h, pixels, encoding);
        }
    }

    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn material_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    base_color: &wgpu::TextureView,
    normal: &wgpu::TextureView,
    metallic_roughness: &wgpu::TextureView,
    occlusion: &wgpu::TextureView,
    emissive: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material"),
        layout,
        entries: &[
            binding(0, wgpu::BindingResource::TextureView(base_color)),
            binding(1, wgpu::BindingResource::TextureView(normal)),
            binding(2, wgpu::BindingResource::TextureView(metallic_roughness)),
            binding(3, wgpu::BindingResource::TextureView(occlusion)),
            binding(4, wgpu::BindingResource::TextureView(emissive)),
            binding(5, wgpu::BindingResource::Sampler(sampler)),
        ],
    })
}

/// One 2x2 box-filter step. Odd dimensions collapse to 1 and then repeat the
/// row/column, which is how GL's mip chain treats them.
fn downsample(w: u32, h: u32, pixels: &[u8], encoding: TextureEncoding) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let (x0, y0) = (x * 2, y * 2);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            for c in 0..4 {
                let at = |px: u32, py: u32| pixels[((py * w + px) * 4 + c) as usize];
                if encoding == TextureEncoding::Srgb && c < 3 {
                    let linear = |value: u8| {
                        let value = f32::from(value) / 255.0;
                        if value <= 0.04045 {
                            value / 12.92
                        } else {
                            ((value + 0.055) / 1.055).powf(2.4)
                        }
                    };
                    let mean = (linear(at(x0, y0))
                        + linear(at(x1, y0))
                        + linear(at(x0, y1))
                        + linear(at(x1, y1)))
                        * 0.25;
                    let encoded = if mean <= 0.003_130_8 {
                        mean * 12.92
                    } else {
                        1.055 * mean.powf(1.0 / 2.4) - 0.055
                    };
                    out.push((encoded * 255.0).round().clamp(0.0, 255.0) as u8);
                } else {
                    let mean = u32::from(at(x0, y0))
                        + u32::from(at(x1, y0))
                        + u32::from(at(x0, y1))
                        + u32::from(at(x1, y1));
                    out.push(((mean + 2) / 4) as u8);
                }
            }
        }
    }
    (nw, nh, out)
}

const ADD: wgpu::BlendComponent = wgpu::BlendComponent {
    src_factor: wgpu::BlendFactor::One,
    dst_factor: wgpu::BlendFactor::One,
    operation: wgpu::BlendOperation::Add,
};

/// Slot of the overlay's pipeline in [`Renderer::overlay_pipelines`]: bit 0 is
/// line topology, bit 1 is [`OverlayDepth::Free`], bit 2 is BGRA output.
fn overlay_pipeline_index(overlay: &Overlay, channels: Channels) -> usize {
    usize::from(overlay.lines)
        | (usize::from(overlay.depth == OverlayDepth::Free) << 1)
        | (channels.index() << 2)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::{Mat4, Vec3};

    use crate::assets::{Material, Vertex};
    use crate::coords::hex_srgb;
    use crate::frame::{
        Camera, DirectionalLight, Draw, FixtureCone, Frame, MaterialTextures, MeshData,
    };
    use crate::overlay::{Overlay, OverlayDepth};
    use crate::scene_desc::DebugView;

    use super::{
        cascade_matrices, downsample, haze_tile_key, haze_tiles, Channels, CompositeUniform,
        Globals, HazeUniform, LightCore, LightRest, Renderer, TextureEncoding, TileHeader,
        CAMERA_FAR, CAMERA_NEAR, CASCADE_COUNT, CLUSTER_DEPTH_SLICES, CLUSTER_TILE_SIZE,
        HAZE_TILE_SIZE, SHADOW_SIZE,
    };

    #[test]
    fn volumetric_cpu_layouts_match_wgsl_storage_and_uniform_strides() {
        assert_eq!(std::mem::size_of::<Globals>(), 368);
        assert_eq!(std::mem::size_of::<HazeUniform>(), 160);
        assert_eq!(std::mem::size_of::<CompositeUniform>(), 96);
        assert_eq!(std::mem::size_of::<LightCore>(), 16);
        assert_eq!(std::mem::size_of::<LightRest>(), 48);
        assert_eq!(std::mem::size_of::<TileHeader>(), 16);
    }

    #[test]
    fn reverse_z_projection_is_monotonic_and_maps_bounded_planes() {
        let projection =
            Mat4::perspective_rh(48f32.to_radians(), 16.0 / 9.0, CAMERA_FAR, CAMERA_NEAR);
        let depth = |distance: f32| projection.project_point3(Vec3::new(0.0, 0.0, -distance)).z;
        assert!((depth(CAMERA_NEAR) - 1.0).abs() < 1e-5);
        assert!(depth(CAMERA_FAR).abs() < 1e-6);
        let samples = [CAMERA_NEAR, 1.0, 10.0, 100.0, CAMERA_FAR];
        assert!(samples
            .windows(2)
            .all(|pair| depth(pair[0]) > depth(pair[1])));
    }

    #[test]
    fn cascades_are_finite_bounded_and_texel_stable_under_small_translation() {
        let eye = Vec3::new(4.5, -5.0, 3.0);
        let forward = (Vec3::new(0.0, 0.8, 0.0) - eye).normalize();
        let to_light = Vec3::new(2.0, -3.0, 6.0).normalize();
        let matrices = cascade_matrices(eye, forward, 48f32.to_radians(), 16.0 / 9.0, to_light);
        assert_eq!(matrices.len(), CASCADE_COUNT);
        assert!(matrices.iter().all(Mat4::is_finite));

        // Move perpendicular to the light by much less than a near-cascade
        // texel. The snapped world-to-shadow transform must not shimmer.
        let lateral = to_light.cross(Vec3::Z).normalize_or(Vec3::X);
        let shifted = cascade_matrices(
            eye + lateral * (1.0 / SHADOW_SIZE as f32),
            forward,
            48f32.to_radians(),
            16.0 / 9.0,
            to_light,
        );
        let max_delta = matrices[0]
            .to_cols_array()
            .into_iter()
            .zip(shifted[0].to_cols_array())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_delta < 1e-5,
            "sub-texel camera motion changed the near cascade by {max_delta}"
        );
    }

    #[test]
    fn tile_cache_key_ignores_shading_only_changes_and_tracks_projected_volume() {
        let camera = Camera {
            eye: Vec3::new(4.5, -5.0, 3.0),
            target: Vec3::ZERO,
            fov_y_deg: 48.0,
        };
        let light = FixtureCone {
            position: Vec3::ZERO,
            range: 5.0,
            direction: Vec3::Z,
            cos_beam: 0.98,
            color: Vec3::ONE,
            intensity: 1.0,
            cos_field: 0.95,
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
        };
        let original = haze_tile_key(&[light], camera, (320, 180));

        let mut shading = light;
        shading.color = Vec3::new(0.2, 0.4, 0.8);
        shading.intensity = 0.25;
        shading.cos_beam = 0.99;
        shading.wash = 1.0;
        shading.gobo = 2;
        shading.gobo_rotation = 1.5;
        assert_eq!(original, haze_tile_key(&[shading], camera, (320, 180)));

        let mut moved = light;
        moved.position.x += 0.1;
        assert_ne!(original, haze_tile_key(&[moved], camera, (320, 180)));
        let mut widened = light;
        widened.cos_field -= 0.01;
        assert_ne!(original, haze_tile_key(&[widened], camera, (320, 180)));
        let mut moved_camera = camera;
        moved_camera.eye.x += 0.1;
        assert_ne!(original, haze_tile_key(&[light], moved_camera, (320, 180)));
        assert_ne!(original, haze_tile_key(&[light], camera, (321, 180)));
    }

    #[test]
    fn material_mips_filter_color_in_linear_light_and_data_as_bytes() {
        let pixels = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let (_, _, color) = downsample(2, 2, &pixels, TextureEncoding::Srgb);
        let (_, _, data) = downsample(2, 2, &pixels, TextureEncoding::Linear);
        assert_eq!(color, [188, 188, 188, 255]);
        assert_eq!(data, [128, 128, 128, 255]);
    }

    #[test]
    fn projected_cone_lists_are_local_not_global() {
        let view_proj = Mat4::perspective_rh(48f32.to_radians(), 16.0 / 9.0, 0.1, 100.0)
            * Mat4::look_at_rh(Vec3::new(4.5, -5.0, 3.0), Vec3::ZERO, Vec3::Z);
        let light = FixtureCone {
            position: Vec3::ZERO,
            range: 5.0,
            direction: Vec3::Z,
            cos_beam: 0.98,
            color: Vec3::ONE,
            intensity: 1.0,
            cos_field: 0.95,
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
        };
        let (headers, indices, _, _) = haze_tiles(&[light], view_proj, 320, 180);
        assert!(!indices.is_empty());
        assert!(indices.len() < headers.len());
    }

    #[test]
    fn projected_cone_lists_have_no_sampled_false_negatives_or_oob_ranges() {
        const WIDTH: u32 = 96;
        const HEIGHT: u32 = 54;
        let eye = Vec3::new(4.5, -5.0, 3.0);
        let view_proj = Mat4::perspective_rh(48f32.to_radians(), 16.0 / 9.0, 0.1, 100.0)
            * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z);
        let inv_view_proj = view_proj.inverse();
        let lights: Vec<_> = (0..16)
            .map(|index| {
                let x = (index % 4) as f32 - 1.5;
                let y = (index / 4) as f32 - 1.5;
                let direction = (Vec3::new(x * 0.25, y * 0.2, 3.0)
                    - Vec3::new(x * 0.5, y * 0.5, 0.0))
                .normalize();
                FixtureCone {
                    position: Vec3::new(x * 0.5, y * 0.5, 0.0),
                    range: 5.0 + index as f32 * 0.1,
                    direction,
                    cos_beam: 0.97,
                    color: Vec3::ONE,
                    intensity: 1.0,
                    cos_field: 0.88 + (index % 3) as f32 * 0.025,
                    wash: 0.0,
                    gobo: 0,
                    gobo_rotation: 0.0,
                }
            })
            .collect();
        let (headers, indices, columns, _) = haze_tiles(&lights, view_proj, WIDTH, HEIGHT);

        for header in &headers {
            let end = header.offset as usize + header.count as usize;
            assert!(end <= indices.len(), "tile header escaped its index buffer");
            assert!(indices[header.offset as usize..end]
                .iter()
                .all(|index| (*index as usize) < lights.len()));
        }

        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let ndc = Vec3::new(
                    ((x as f32 + 0.5) / WIDTH as f32) * 2.0 - 1.0,
                    1.0 - ((y as f32 + 0.5) / HEIGHT as f32) * 2.0,
                    0.5,
                );
                let world = inv_view_proj.project_point3(ndc);
                let ray = (world - eye).normalize();
                let tile_x = x / HAZE_TILE_SIZE;
                let tile_y = y / HAZE_TILE_SIZE;
                let header = headers[(tile_y * columns + tile_x) as usize];
                let listed =
                    &indices[header.offset as usize..(header.offset + header.count) as usize];
                for (index, light) in lights.iter().enumerate() {
                    if sampled_ray_hits_cone(eye, ray, light) {
                        assert!(
                            listed.contains(&(index as u32)),
                            "tile ({tile_x},{tile_y}) omitted cone {index} at pixel ({x},{y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn post_agx_overlay_keeps_authored_srgb_across_scene_lighting_and_output_formats(
    ) -> anyhow::Result<()> {
        const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
        let mut renderer = Renderer::new()?;
        let dark = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::ZERO);
        let lit = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::splat(40.0));

        let mut rgba_dark = Vec::new();
        renderer.render_into(&dark, 96, 96, 1, Channels::Rgba, &mut rgba_dark)?;
        let mut rgba_lit = Vec::new();
        renderer.render_into(&lit, 96, 96, 1, Channels::Rgba, &mut rgba_lit)?;
        let mut bgra_lit = Vec::new();
        renderer.render_into(&lit, 96, 96, 1, Channels::Bgra, &mut bgra_lit)?;

        assert_eq!(center_rgb(&rgba_dark, Channels::Rgba), AUTHORED);
        assert_eq!(center_rgb(&rgba_lit, Channels::Rgba), AUTHORED);
        assert_eq!(center_rgb(&bgra_lit, Channels::Bgra), AUTHORED);
        let bgra_offset = center_offset(96, 96);
        assert_eq!(
            &bgra_lit[bgra_offset..bgra_offset + 4],
            &[AUTHORED[2], AUTHORED[1], AUTHORED[0], 255]
        );
        Ok(())
    }

    #[test]
    fn post_agx_overlays_load_reverse_z_depth_while_free_gizmos_ignore_it() -> anyhow::Result<()> {
        const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
        let mut renderer = Renderer::new()?;
        let occluder_only = overlay_test_frame(true, OverlayDepth::Tested, 1.0, Vec3::ZERO);
        let mut without_overlay = overlay_test_frame(true, OverlayDepth::Tested, 1.0, Vec3::ZERO);
        without_overlay.overlays.clear();

        let mut background = Vec::new();
        renderer.render_into(&without_overlay, 96, 96, 1, Channels::Rgba, &mut background)?;
        let mut tested_behind = Vec::new();
        renderer.render_into(
            &occluder_only,
            96,
            96,
            1,
            Channels::Rgba,
            &mut tested_behind,
        )?;
        assert_eq!(
            center_rgb(&tested_behind, Channels::Rgba),
            center_rgb(&background, Channels::Rgba),
            "a tested cage behind opaque geometry must remain occluded"
        );

        let tested_front = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::ZERO);
        let mut front = Vec::new();
        renderer.render_into(&tested_front, 96, 96, 1, Channels::Rgba, &mut front)?;
        assert_eq!(center_rgb(&front, Channels::Rgba), AUTHORED);

        let free_behind = overlay_test_frame(true, OverlayDepth::Free, 1.0, Vec3::ZERO);
        let mut free = Vec::new();
        renderer.render_into(&free_behind, 96, 96, 1, Channels::Rgba, &mut free)?;
        assert_eq!(
            center_rgb(&free, Channels::Rgba),
            AUTHORED,
            "a free gizmo must use Always and paint over opaque geometry"
        );
        Ok(())
    }

    #[test]
    fn clustered_fixture_light_is_local_and_surface_toggle_is_independent() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(1);
        frame.fixture_cones[0].color = Vec3::new(1.0, 0.08, 0.02);
        let lit = renderer.render(&frame, 160, 120, 1)?;
        frame.fixture_surface_lighting = false;
        let dark = renderer.render(&frame, 160, 120, 1)?;

        let central = region_mean(&lit, 160, 52..108, 38..94);
        let central_dark = region_mean(&dark, 160, 52..108, 38..94);
        let corner = region_mean(&lit, 160, 0..24, 88..120);
        assert!(
            central > central_dark + 2.0,
            "fixture surface light did not contribute locally: {central:.2} vs {central_dark:.2}"
        );
        assert!(
            central > corner + 1.0,
            "finite cone illuminated the whole surface: center {central:.2}, corner {corner:.2}"
        );
        Ok(())
    }

    #[test]
    fn clustered_surface_uses_shared_cone_and_gobo_photometry() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let open = fixture_surface_frame(1);
        let open_pixels = renderer.render(&open, 160, 120, 1)?;
        let mut spokes = fixture_surface_frame(1);
        spokes.fixture_cones[0].gobo = 1;
        spokes.fixture_cones[0].gobo_rotation = 0.31;
        let spokes_pixels = renderer.render(&spokes, 160, 120, 1)?;
        let mut outside = fixture_surface_frame(1);
        outside.fixture_cones[0].direction = Vec3::new(0.0, 1.0, -0.08).normalize();
        let outside_pixels = renderer.render(&outside, 160, 120, 1)?;

        let open_energy = region_mean(&open_pixels, 160, 42..118, 32..104);
        let spokes_energy = region_mean(&spokes_pixels, 160, 42..118, 32..104);
        let outside_energy = region_mean(&outside_pixels, 160, 42..118, 32..104);
        assert!(
            open_energy > spokes_energy + 0.5,
            "shared gobo did not remove surface energy: {open_energy:.2} vs {spokes_energy:.2}"
        );
        assert!(
            open_energy > outside_energy + 1.0,
            "shared cone cutoff did not reject an off-axis surface: {open_energy:.2} vs {outside_energy:.2}"
        );
        Ok(())
    }

    #[test]
    fn cluster_gpu_cache_reuses_topology_for_shading_only_changes() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(32);
        renderer.render(&frame, 192, 128, 1)?;
        let first = renderer.cluster_stats();
        assert_eq!(first.rebuilds, 1);

        for light in &mut frame.fixture_cones {
            light.color = Vec3::new(0.1, 0.8, 0.3);
            light.intensity *= 0.4;
            light.cos_beam = 0.99;
            light.wash = 1.0;
            light.gobo = 2;
            light.gobo_rotation = 1.7;
        }
        renderer.render(&frame, 192, 128, 1)?;
        assert_eq!(renderer.cluster_stats().rebuilds, first.rebuilds);

        frame.fixture_cones[0].position.x += 0.1;
        renderer.render(&frame, 192, 128, 1)?;
        assert_eq!(renderer.cluster_stats().rebuilds, first.rebuilds + 1);
        Ok(())
    }

    #[test]
    fn clustered_surface_lists_remain_bounded_at_32_128_and_512_cones() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        for count in [32, 128, 512] {
            let frame = fixture_surface_frame(count);
            renderer.render(&frame, 320, 180, 1)?;
            let stats = renderer.cluster_stats();
            let clusters = 320_u32.div_ceil(CLUSTER_TILE_SIZE) as usize
                * 180_u32.div_ceil(CLUSTER_TILE_SIZE) as usize
                * CLUSTER_DEPTH_SLICES as usize;
            assert!(stats.occupied_clusters <= clusters);
            assert!(stats.max_lights_per_cluster <= count as u32);
            assert!(stats.light_references <= clusters * count);
            assert!(stats.light_references > 0);
        }
        Ok(())
    }

    #[test]
    fn cluster_occupancy_debug_is_topology_only() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(32);
        frame.cluster_debug = true;
        frame.fixture_surface_lighting = false;
        let first = renderer.render(&frame, 160, 120, 1)?;
        for light in &mut frame.fixture_cones {
            light.color = Vec3::ZERO;
            light.intensity = 0.0;
            light.gobo = 2;
        }
        let shading_only = renderer.render(&frame, 160, 120, 1)?;
        assert_eq!(first, shading_only);
        assert!(first.chunks_exact(4).any(|pixel| pixel[2] > 32));
        Ok(())
    }

    fn fixture_surface_frame(count: usize) -> Frame {
        let vertex = |position: [f32; 3]| Vertex {
            position,
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let mesh = MeshData {
            key: "::clustered-surface-floor".into(),
            vertices: vec![
                vertex([-5.0, -5.0, 0.0]),
                vertex([5.0, -5.0, 0.0]),
                vertex([5.0, 5.0, 0.0]),
                vertex([-5.0, 5.0, 0.0]),
            ]
            .into(),
            indices: Arc::from([0, 1, 2, 0, 2, 3]),
        };
        let fixture_cones = (0..count)
            .map(|index| {
                let column = (index % 32) as f32;
                let row = (index / 32) as f32;
                FixtureCone {
                    position: Vec3::new((column - 15.5) * 0.08, (row - 7.5) * 0.08, 3.0),
                    range: 5.0,
                    direction: Vec3::NEG_Z,
                    cos_beam: 0.96,
                    color: Vec3::ONE,
                    intensity: 3.0 / count.max(1) as f32,
                    cos_field: 0.88,
                    wash: 0.0,
                    gobo: 0,
                    gobo_rotation: 0.0,
                }
            })
            .collect();
        Frame {
            meshes: vec![mesh],
            images: Vec::new(),
            draws: vec![Draw {
                mesh: 0,
                model: Mat4::IDENTITY,
                material: Material {
                    base_color: Vec3::splat(0.65),
                    roughness: 0.55,
                    ..Material::default()
                },
                textures: MaterialTextures::default(),
            }],
            grid_draws: 0,
            overlays: Vec::new(),
            point_lights: Vec::new(),
            fixture_cones,
            fixture_surface_lighting: true,
            cluster_debug: false,
            clear_color: Vec3::ZERO,
            ambient: Vec3::splat(0.002),
            environment: None,
            directional: None,
            haze_density: 0.0,
            haze_steps: 1,
            haze_resolution: 0.5,
            time: 0.0,
            debug_view: DebugView::Pbr,
            camera: Camera {
                eye: Vec3::new(0.0, -6.0, 4.5),
                target: Vec3::ZERO,
                fov_y_deg: 48.0,
            },
        }
    }

    fn region_mean(
        pixels: &[u8],
        width: usize,
        xs: std::ops::Range<usize>,
        ys: std::ops::Range<usize>,
    ) -> f64 {
        let mut sum = 0_u64;
        let mut samples = 0_u64;
        for y in ys {
            for x in xs.clone() {
                let offset = (y * width + x) * 4;
                sum += u64::from(pixels[offset])
                    + u64::from(pixels[offset + 1])
                    + u64::from(pixels[offset + 2]);
                samples += 3;
            }
        }
        sum as f64 / samples as f64
    }

    fn overlay_test_frame(
        occluder: bool,
        depth: OverlayDepth,
        overlay_y: f32,
        scene_radiance: Vec3,
    ) -> Frame {
        let vertex = |position: [f32; 3]| Vertex {
            position,
            normal: [0.0, -1.0, 0.0],
            uv: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let mesh = MeshData {
            key: "::post-agx-overlay-test".into(),
            vertices: vec![
                vertex([-2.0, 0.0, -2.0]),
                vertex([2.0, 0.0, -2.0]),
                vertex([0.0, 0.0, 2.0]),
            ]
            .into(),
            indices: Arc::from([0, 1, 2]),
        };
        let draws = occluder
            .then(|| Draw {
                mesh: 0,
                model: Mat4::IDENTITY,
                material: Material {
                    base_color: Vec3::splat(0.35),
                    roughness: 1.0,
                    ..Material::default()
                },
                textures: MaterialTextures::default(),
                editor_object: None,
            })
            .into_iter()
            .collect();
        Frame {
            meshes: vec![mesh],
            images: Vec::new(),
            draws,
            grid_draws: 0,
            overlays: vec![Overlay {
                mesh: 0,
                model: Mat4::from_translation(Vec3::Y * overlay_y),
                lines: false,
                color: hex_srgb(0x33_99_e6),
                opacity: 1.0,
                depth,
            }],
            point_lights: Vec::new(),
            fixture_cones: Vec::new(),
            fixture_surface_lighting: false,
            cluster_debug: false,
            environment: None,
            clear_color: scene_radiance,
            ambient: scene_radiance,
            directional: Some(DirectionalLight {
                direction: Vec3::new(0.0, -1.0, 1.0).normalize(),
                radiance: scene_radiance,
                shadow_eye: Vec3::new(0.0, -4.0, 5.0),
                shadows: false,
                shadow_softness: 1.0,
            }),
            haze_density: 0.0,
            haze_steps: 1,
            haze_resolution: 1.0,
            time: 0.0,
            debug_view: DebugView::Pbr,
            camera: Camera {
                eye: Vec3::new(0.0, -5.0, 0.0),
                target: Vec3::ZERO,
                fov_y_deg: 48.0,
            },
        }
    }

    fn center_offset(width: usize, height: usize) -> usize {
        ((height / 2) * width + width / 2) * 4
    }

    fn center_rgb(pixels: &[u8], channels: Channels) -> [u8; 3] {
        let offset = center_offset(96, 96);
        match channels {
            Channels::Rgba => [pixels[offset], pixels[offset + 1], pixels[offset + 2]],
            Channels::Bgra => [pixels[offset + 2], pixels[offset + 1], pixels[offset]],
        }
    }

    /// Independent conservative oracle: numerical samples can prove a ray is
    /// inside a finite cone, but never claim a miss. Every proven hit must be
    /// present in the CPU-generated tile list.
    fn sampled_ray_hits_cone(eye: Vec3, ray: Vec3, light: &FixtureCone) -> bool {
        let oc = eye - light.position;
        let b = oc.dot(ray);
        let disc = b * b - (oc.length_squared() - light.range * light.range);
        if disc <= 0.0 {
            return false;
        }
        let root = disc.sqrt();
        let start = (-b - root).max(0.0);
        let end = -b + root;
        if end <= start {
            return false;
        }
        (0..=32).any(|sample| {
            let t = start + (end - start) * sample as f32 / 32.0;
            let q = oc + ray * t;
            let axial = q.dot(light.direction);
            axial > 0.0 && axial * axial >= light.cos_field * light.cos_field * q.length_squared()
        })
    }
}

impl PendingReadback {
    pub(crate) fn try_complete(&mut self) -> anyhow::Result<Option<CompletedReadback>> {
        if self.mapped_result.is_none() {
            self.mapped_result = match self.mapped.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(anyhow::anyhow!("GPU map callback disconnected"));
                }
            };
        }
        if let Some(profile) = &mut self.profile {
            if profile.mapped_result.is_none() {
                profile.mapped_result = match profile.mapped.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(anyhow::anyhow!("GPU timestamp map callback disconnected"));
                    }
                };
            }
        }
        if self.mapped_result.is_none()
            || self
                .profile
                .as_ref()
                .is_some_and(|profile| profile.mapped_result.is_none())
        {
            return Ok(None);
        }
        self.mapped_result
            .take()
            .expect("mapped result checked above")
            .map_err(anyhow::Error::msg)?;
        let profile = if let Some(profile) = &mut self.profile {
            profile
                .mapped_result
                .take()
                .expect("profile map result checked above")
                .map_err(anyhow::Error::msg)?;
            let view = profile.readback.slice(..32).get_mapped_range();
            let mut timestamps = [0_u64; 4];
            for (timestamp, bytes) in timestamps.iter_mut().zip(view.chunks_exact(8)) {
                *timestamp =
                    u64::from_ne_bytes(bytes.try_into().expect("timestamp is eight bytes"));
            }
            drop(view);
            profile.readback.unmap();
            anyhow::ensure!(
                timestamps.windows(2).all(|pair| pair[0] <= pair[1]),
                "non-monotonic GPU pass timestamps: {timestamps:?}"
            );
            let milliseconds = f64::from(profile.timestamp_period_ns) / 1_000_000.0;
            Some(FrameTimings {
                gpu_total_ms: (timestamps[3] - timestamps[0]) as f64 * milliseconds,
                gpu_volumetric_ms: (timestamps[2] - timestamps[1]) as f64 * milliseconds,
                cpu_encode_submit_ms: profile.cpu_encode_submit.as_secs_f64() * 1000.0,
                cpu_cluster_ms: profile.cpu_cluster.as_secs_f64() * 1000.0,
            })
        } else {
            None
        };
        let view = self.readback.slice(..).get_mapped_range();
        let row_bytes = (self.width * 4) as usize;
        let mut pixels = Vec::with_capacity(row_bytes * self.height as usize);
        if self.bytes_per_row as usize == row_bytes {
            pixels.extend_from_slice(&view);
        } else {
            for row in 0..self.height {
                let start = (row * self.bytes_per_row) as usize;
                pixels.extend_from_slice(&view[start..start + row_bytes]);
            }
        }
        drop(view);
        self.readback.unmap();
        Ok(Some(CompletedReadback {
            width: self.width,
            height: self.height,
            pixels,
            draw_time: self.started.elapsed(),
            profile,
        }))
    }
}

fn instance_of(draw: &Draw) -> Instance {
    Instance {
        model: draw.model.to_cols_array_2d(),
        normal_matrix: draw.model.inverse().transpose().to_cols_array_2d(),
        base_color: draw
            .material
            .base_color
            .extend(draw.material.metallic)
            .to_array(),
        emissive: draw
            .material
            .emissive
            .extend(draw.material.roughness)
            .to_array(),
        flags: [
            f32::from(u8::from(draw.material.flat_shading)),
            if draw.textures.normal.is_some() {
                draw.material.normal_scale
            } else {
                0.0
            },
            draw.material.occlusion_strength,
            0.0,
        ],
    }
}

/// wgpu rejects a zero-sized storage buffer; an empty light list still needs a
/// binding.
fn pad_at_least_one<T: Pod + Zeroable>(mut v: Vec<T>) -> Vec<T> {
    if v.is_empty() {
        v.push(T::zeroed());
    }
    v
}

/// Build a conservative screen-space light list for fixed 16x16 haze tiles.
/// The projected AABB encloses each range sphere; if it crosses the eye plane,
/// every tile receives the light rather than risking a false negative.
fn haze_tiles(
    lights: &[crate::frame::FixtureCone],
    view_proj: Mat4,
    width: u32,
    height: u32,
) -> (Vec<TileHeader>, Vec<u32>, u32, u32) {
    let columns = width.div_ceil(HAZE_TILE_SIZE).max(1);
    let rows = height.div_ceil(HAZE_TILE_SIZE).max(1);
    let mut tiles = vec![Vec::<u32>::new(); (columns * rows) as usize];
    for (light_index, light) in lights.iter().enumerate() {
        let range = light.range.clamp(0.05, 100.0);
        let (bounds_min, bounds_max) = if light.cos_field > 0.05 {
            let base = light.position + light.direction * range;
            let radius =
                range * (1.0 - light.cos_field * light.cos_field).max(0.0).sqrt() / light.cos_field;
            (
                light.position.min(base - Vec3::splat(radius)),
                light.position.max(base + Vec3::splat(radius)),
            )
        } else {
            (
                light.position - Vec3::splat(range),
                light.position + Vec3::splat(range),
            )
        };
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        let mut behind_eye = false;
        let mut in_front = false;
        for z in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for x in [-1.0, 1.0] {
                    let corner = Vec3::new(
                        if x < 0.0 { bounds_min.x } else { bounds_max.x },
                        if y < 0.0 { bounds_min.y } else { bounds_max.y },
                        if z < 0.0 { bounds_min.z } else { bounds_max.z },
                    );
                    let clip = view_proj * corner.extend(1.0);
                    if clip.w <= 1e-4 {
                        behind_eye = true;
                        continue;
                    }
                    in_front = true;
                    let ndc = clip.truncate() / clip.w;
                    let pixel = Vec2::new(
                        (ndc.x * 0.5 + 0.5) * width as f32,
                        (0.5 - ndc.y * 0.5) * height as f32,
                    );
                    min = min.min(pixel);
                    max = max.max(pixel);
                }
            }
        }
        if !in_front {
            continue;
        }
        let (x0, y0, x1, y1) = if behind_eye {
            (0, 0, columns - 1, rows - 1)
        } else {
            if max.x < 0.0 || max.y < 0.0 || min.x >= width as f32 || min.y >= height as f32 {
                continue;
            }
            (
                (min.x.max(0.0) as u32 / HAZE_TILE_SIZE).min(columns - 1),
                (min.y.max(0.0) as u32 / HAZE_TILE_SIZE).min(rows - 1),
                (max.x.max(0.0) as u32 / HAZE_TILE_SIZE).min(columns - 1),
                (max.y.max(0.0) as u32 / HAZE_TILE_SIZE).min(rows - 1),
            )
        };
        for tile_y in y0..=y1 {
            for tile_x in x0..=x1 {
                tiles[(tile_y * columns + tile_x) as usize].push(light_index as u32);
            }
        }
    }

    let mut headers = Vec::with_capacity(tiles.len());
    let mut indices = Vec::new();
    for tile in tiles {
        headers.push(TileHeader {
            offset: indices.len() as u32,
            count: tile.len() as u32,
            _pad: [0; 2],
        });
        indices.extend(tile);
    }
    (headers, indices, columns, rows)
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
    }
}

/// History is valid only while projection-affecting state and the physical
/// light volumes are identical. Color/intensity may animate through history;
/// moving a cone, changing a gobo, resizing, orbiting, or changing density
/// resets it. Time continuity is checked separately at submission.
fn haze_history_key(
    frame: &Frame,
    width: u32,
    height: u32,
    haze: (u32, u32),
    density: f32,
) -> HazeHistoryKey {
    let mut topology = 0xcbf2_9ce4_8422_2325u64;
    let mut push = |value: u32| {
        topology = (topology ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    };
    push(frame.fixture_cones.len() as u32);
    for light in &frame.fixture_cones {
        for value in light.position.to_array() {
            push(value.to_bits());
        }
        push(light.range.to_bits());
        for value in light.direction.to_array() {
            push(value.to_bits());
        }
        push(light.cos_beam.to_bits());
        push(light.cos_field.to_bits());
        push(light.wash.to_bits());
        push(light.gobo);
        push(light.gobo_rotation.to_bits());
    }
    HazeHistoryKey {
        output: [width, height, haze.0, haze.1],
        camera: [
            frame.camera.eye.x.to_bits(),
            frame.camera.eye.y.to_bits(),
            frame.camera.eye.z.to_bits(),
            frame.camera.target.x.to_bits(),
            frame.camera.target.y.to_bits(),
            frame.camera.target.z.to_bits(),
        ],
        fov: frame.camera.fov_y_deg.to_bits(),
        density: density.to_bits(),
        topology,
    }
}

fn haze_tile_key(
    lights: &[crate::frame::FixtureCone],
    camera: crate::frame::Camera,
    size: (u32, u32),
) -> HazeTileKey {
    let mut topology = 0xcbf2_9ce4_8422_2325_u64;
    let mut push = |value: u32| {
        topology = (topology ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    };
    push(lights.len() as u32);
    for light in lights {
        for value in light.position.to_array() {
            push(value.to_bits());
        }
        push(light.range.to_bits());
        for value in light.direction.to_array() {
            push(value.to_bits());
        }
        push(light.cos_field.to_bits());
    }
    HazeTileKey {
        size: [size.0, size.1],
        camera: [
            camera.eye.x.to_bits(),
            camera.eye.y.to_bits(),
            camera.eye.z.to_bits(),
            camera.target.x.to_bits(),
            camera.target.y.to_bits(),
            camera.target.z.to_bits(),
        ],
        fov: camera.fov_y_deg.to_bits(),
        topology,
    }
}

fn shader(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
    })
}

/// Fit three stable orthographic sun cameras to bounded view-frustum slices.
///
/// Each slice uses a quantized bounding sphere instead of a tight AABB. Camera
/// translation then changes only the snapped light-space centre; small camera
/// rotations cannot resize the projection and make every shadow texel swim.
fn cascade_matrices(
    eye: Vec3,
    forward: Vec3,
    fov_y: f32,
    aspect: f32,
    to_light: Vec3,
) -> [Mat4; CASCADE_COUNT] {
    let forward = forward.normalize_or(Vec3::Y);
    let world_up = if forward.z.abs() > 0.99 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let right = forward.cross(world_up).normalize_or(Vec3::X);
    let up = right.cross(forward).normalize_or(Vec3::Z);
    let light_dir = to_light.normalize_or(Vec3::new(0.4, -0.6, 0.7));
    let light_up = if light_dir.z.abs() > 0.99 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let tan_half = (fov_y * 0.5).tan();

    std::array::from_fn(|cascade| {
        let near = if cascade == 0 {
            CAMERA_NEAR
        } else {
            CASCADE_SPLITS[cascade - 1]
        };
        let far = CASCADE_SPLITS[cascade];
        let corners = [near, far].map(|distance| {
            let center = eye + forward * distance;
            let half_y = tan_half * distance;
            let half_x = half_y * aspect;
            [
                center - right * half_x - up * half_y,
                center + right * half_x - up * half_y,
                center - right * half_x + up * half_y,
                center + right * half_x + up * half_y,
            ]
        });
        let corners = [
            corners[0][0],
            corners[0][1],
            corners[0][2],
            corners[0][3],
            corners[1][0],
            corners[1][1],
            corners[1][2],
            corners[1][3],
        ];
        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        // Quantization prevents floating-point radius noise from changing the
        // projection scale. 1/16 m is well below one far-cascade texel.
        let radius = corners
            .iter()
            .map(|corner| corner.distance(center))
            .fold(0.0_f32, f32::max);
        let radius = (radius * 16.0).ceil() / 16.0;
        // Keep light orientation and origin independent of the camera. The
        // orthographic bounds carry the moving slice; this is what lets their
        // centre snap in world-shadow space instead of remaining perpetually
        // zero in a camera-following look-at matrix.
        let light_view = Mat4::look_at_rh(light_dir, Vec3::ZERO, light_up);
        let center_light = light_view.transform_point3(center);
        let texel = (2.0 * radius) / SHADOW_SIZE as f32;
        let snapped_x = (center_light.x / texel).round() * texel;
        let snapped_y = (center_light.y / texel).round() * texel;
        let (min_depth, max_depth) = corners
            .iter()
            .map(|corner| -light_view.transform_point3(*corner).z)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), depth| {
                (min.min(depth), max.max(depth))
            });
        // Swap near/far to map the nearest receiver to one and the furthest to
        // zero, matching the scene's GreaterEqual reverse-Z convention. The
        // signed distances are valid for an orthographic projection and let
        // this fixed light frame cover venues on either side of its origin.
        Mat4::orthographic_rh(
            snapped_x - radius,
            snapped_x + radius,
            snapped_y - radius,
            snapped_y + radius,
            max_depth + 25.0,
            min_depth - 25.0,
        ) * light_view
    })
}

fn depth_state(write: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: write,
        // Equal preserves the intentional coplanar photo panels in stage GLBs.
        // Greater is the single convention for camera and sun reverse-Z.
        depth_compare: wgpu::CompareFunction::GreaterEqual,
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn depth_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    samples: u32,
    label: &str,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn depth_attachment(view: &wgpu::TextureView) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

/// Load the full-resolution prepass depth for post-composite editor cages.
fn depth_attachment_load(view: &wgpu::TextureView) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

fn shadow_texture_array(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    layers: u32,
    label: &str,
) -> (wgpu::TextureView, [wgpu::TextureView; CASCADE_COUNT]) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let array = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(label),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        base_array_layer: 0,
        array_layer_count: Some(layers),
        ..Default::default()
    });
    let render_layers = std::array::from_fn(|index| {
        let layer = (index as u32).min(layers - 1);
        texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("shadow-cascade-layer"),
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    });
    (array, render_layers)
}

fn binding(index: u32, resource: wgpu::BindingResource) -> wgpu::BindGroupEntry {
    wgpu::BindGroupEntry {
        binding: index,
        resource,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
