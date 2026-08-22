//! The wgpu device and the frame's pass chain.
//!
//! Offscreen only: nothing here knows about a window or a swapchain. A frame
//! goes shadow → depth → scene (MSAA) → haze (accumulated) → composite+AgX →
//! readback, and comes out as sRGB-encoded 8-bit bytes in the caller's
//! [`Channels`] order.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::assets::Image;
use crate::frame::{Draw, Frame};
use crate::overlay::{Overlay, OverlayDepth};

/// Shadow map edge, matching `shadow-mapSize-{width,height}={4096}`.
const SHADOW_SIZE: u32 = 4096;

const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MSAA_SAMPLES: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
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
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniform {
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
    scene_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    haze_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    scene_pipeline: wgpu::RenderPipeline,
    depth_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    haze_pipeline: wgpu::RenderPipeline,
    /// Indexed by [`Channels::index`]: the same pass, targeting each output
    /// format.
    composite_pipelines: [wgpu::RenderPipeline; 2],
    grid_pipeline: wgpu::RenderPipeline,
    overlay_layout: wgpu::BindGroupLayout,
    /// Indexed by [`overlay_pipeline_index`]: the two topologies crossed with
    /// the two depth behaviours of [`OverlayDepth`].
    overlay_pipelines: [wgpu::RenderPipeline; 4],
    shadow_map: wgpu::TextureView,
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
}

pub(crate) struct CompletedReadback {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
    pub(crate) draw_time: Duration,
}

impl Renderer {
    /// # Errors
    /// Fails when no wgpu adapter or device can be acquired.
    pub fn new() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("luma-render"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            }))?;

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
                        view_dimension: wgpu::TextureViewDimension::D2,
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
        let scene_module = shader(
            &device,
            "scene",
            &format!("{bindings}{}", include_str!("shaders/scene.wgsl")),
        );
        let haze_module = shader(&device, "haze", include_str!("shaders/haze.wgsl"));
        let composite_module = shader(&device, "composite", include_str!("shaders/composite.wgsl"));
        let grid_module = shader(
            &device,
            "grid",
            &format!("{bindings}{}", include_str!("shaders/grid.wgsl")),
        );

        let scene_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("scene"),
                bind_group_layouts: &[&scene_layout, &material_layout],
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

        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("composite"),
                bind_group_layouts: &[&composite_layout],
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
            let lines = i & 1 == 1;
            let free = i & 2 == 2;
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
                        format: SCENE_FORMAT,
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
                multisample: wgpu::MultisampleState {
                    count: MSAA_SAMPLES,
                    ..Default::default()
                },
                multiview: None,
                cache: None,
            })
        });

        let shadow_map = depth_texture(&device, SHADOW_SIZE, SHADOW_SIZE, 1, "shadow");
        let dummy_shadow = depth_texture(&device, 1, 1, 1, "shadow-placeholder");
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
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
            scene_layout,
            material_layout,
            haze_layout,
            composite_layout,
            scene_pipeline,
            depth_pipeline,
            shadow_pipeline,
            haze_pipeline,
            composite_pipelines,
            grid_pipeline,
            overlay_layout,
            overlay_pipelines,
            shadow_map,
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
            },
            targets: None,
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

    /// Read cumulative immutable-resource upload counts.
    #[must_use]
    pub fn upload_stats(&self) -> UploadStats {
        self.upload_stats
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
        let mut pending = self.submit_readback(frame, width, height, subframes, channels, 0);
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
    ) -> PendingReadback {
        self.submit_readback(frame, width, height, subframes, Channels::Bgra, slot)
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
    ) -> PendingReadback {
        assert!(slot < 3, "presentation slot is bounded to three");
        let started = Instant::now();
        let aspect = width as f32 / height as f32;
        // three `PerspectiveCamera` defaults, and the same reversed-Y handedness
        // wgpu clip space wants.
        let proj = Mat4::perspective_rh(frame.camera.fov_y_deg.to_radians(), aspect, 0.1, 2000.0);
        let view = Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z);
        let view_proj = proj * view;

        // `shadow-camera-{left,right,top,bottom}=±15`, near 0.5, far 60, from
        // wherever `<directionalLight position>` puts the light.
        let light_view_proj = frame.directional.map_or(Mat4::IDENTITY, |light| {
            let eye = light.shadow_eye;
            let up = if eye.normalize().z.abs() > 0.99 {
                Vec3::Y
            } else {
                Vec3::Z
            };
            Mat4::orthographic_rh(-15.0, 15.0, -15.0, 15.0, 0.5, 60.0)
                * Mat4::look_at_rh(eye, Vec3::ZERO, up)
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

        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            light_view_proj: light_view_proj.to_cols_array_2d(),
            camera_pos: frame.camera.eye.extend(1.0).to_array(),
            ambient: frame.ambient.extend(0.0).to_array(),
            dir_to_light: frame
                .directional
                .map_or(Vec4::ZERO, |light| light.direction.extend(1.0))
                .to_array(),
            dir_color: frame
                .directional
                .map_or(Vec4::ZERO, |light| light.radiance.extend(0.0))
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

        let lit_bg = self.scene_bind_group(&globals_buf, &instance_buf, &point_buf, true);
        let unlit_bg = self.scene_bind_group(&globals_buf, &instance_buf, &point_buf, false);

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
                presentation.output.clone(),
                presentation.output_view.clone(),
                presentation.readback.clone(),
                t.bytes_per_row,
            )
        };
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
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadow"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(depth_attachment(&self.shadow_map)),
                    ..Default::default()
                });
                pass.set_pipeline(&self.shadow_pipeline);
                pass.set_bind_group(0, &unlit_bg, &[]);
                draw_range(&mut pass, 0..opaque);
            }

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("depth-prepass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(depth_attachment(&depth_view)),
                    ..Default::default()
                });
                pass.set_pipeline(&self.depth_pipeline);
                pass.set_bind_group(0, &unlit_bg, &[]);
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
                draw_range(&mut pass, 0..opaque);
                if frame.grid_draws > 0 {
                    pass.set_pipeline(&self.grid_pipeline);
                    draw_range(&mut pass, opaque..frame.draws.len());
                }

                // Editor affordances last, in the order `overlay::build`
                // emitted them — which is three's paint order.
                if !frame.overlays.is_empty() {
                    pass.set_bind_group(0, &overlay_bg, &[]);
                    for (i, overlay) in frame.overlays.iter().enumerate() {
                        pass.set_pipeline(&self.overlay_pipelines[overlay_pipeline_index(overlay)]);
                        let (first, last, base) = ranges[overlay.mesh];
                        pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                    }
                }
            }
        }

        // --- haze ------------------------------------------------------------
        let cores: Vec<LightCore> = frame
            .haze_lights
            .iter()
            .map(|l| LightCore {
                position: l.position.to_array(),
                range: l.range,
            })
            .collect();
        let rests: Vec<LightRest> = frame
            .haze_lights
            .iter()
            .map(|l| LightRest {
                direction: l.direction.to_array(),
                cos_beam: l.cos_beam,
                color: l.color.to_array(),
                intensity: l.intensity,
                cos_field: l.cos_field,
                wash: l.wash,
                _pad: [0.0; 2],
            })
            .collect();
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

        let inv_view_proj = view_proj.inverse();
        let subframes = subframes.max(1);
        let weight = 1.0 / subframes as f32;
        for k in 0..subframes {
            let uniform = HazeUniform {
                inv_view_proj: inv_view_proj.to_cols_array_2d(),
                camera_pos: frame.camera.eye.extend(1.0).to_array(),
                params: [
                    frame.haze_lights.len() as f32,
                    frame.haze_density,
                    frame.haze_steps as f32,
                    // Feeding track time makes the noise drift identical on
                    // every run (spec §6).
                    frame.time,
                ],
                tuning: [
                    k as f32,
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
                ..Default::default()
            });
            pass.set_pipeline(&self.haze_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        // --- composite + readback --------------------------------------------
        let composite_uniform = CompositeUniform {
            params: [
                haze_size.0 as f32,
                haze_size.1 as f32,
                // Depth sigma in raw-depth units, as `HazeCompositeEffect` has it.
                0.005,
                frame.debug_view.shader_code() as f32,
            ],
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
                binding(2, wgpu::BindingResource::TextureView(&haze_view)),
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
                    ..Default::default()
                });
                pass.set_pipeline(&self.composite_pipelines[channels.index()]);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
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
        let (mapped_tx, mapped) = mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = mapped_tx.send(result.map_err(|error| error.to_string()));
            });
        PendingReadback {
            readback,
            mapped,
            width: t_width,
            height: t_height,
            bytes_per_row,
            started,
        }
    }

    fn scene_bind_group(
        &self,
        globals: &wgpu::Buffer,
        instances: &wgpu::Buffer,
        point_lights: &wgpu::Buffer,
        shadows: bool,
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
                binding(4, wgpu::BindingResource::Sampler(&self.shadow_sampler)),
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
/// the line topology, bit 1 is [`OverlayDepth::Free`].
fn overlay_pipeline_index(overlay: &Overlay) -> usize {
    usize::from(overlay.lines) | (usize::from(overlay.depth == OverlayDepth::Free) << 1)
}

#[cfg(test)]
mod tests {
    use super::{downsample, TextureEncoding};

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
}

impl PendingReadback {
    pub(crate) fn try_complete(&mut self) -> anyhow::Result<Option<CompletedReadback>> {
        let mapped = match self.mapped.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(anyhow::anyhow!("GPU map callback disconnected"));
            }
        };
        mapped.map_err(anyhow::Error::msg)?;
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

fn shader(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
    })
}

fn depth_state(write: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: write,
        // three's `material.depthFunc` default is `LessEqualDepth`, and the
        // stage GLBs lean on it: the speaker's photo panels are modelled
        // exactly coplanar with the cabinet they sit on, so `Less` would keep
        // whichever the draw order happened to reach first.
        depth_compare: wgpu::CompareFunction::LessEqual,
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
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
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
