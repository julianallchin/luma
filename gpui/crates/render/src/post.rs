//! The camera: auto-exposure, lens glow, tone curve and glare.
//!
//! Runs only when a frame's [`Look`] asks for more than the single composite
//! pass does ([`Look::needs_post`]). The composite pass then writes
//! scene-linear light into [`Post::scene_target`] instead of the output, and
//! this chain finishes the frame:
//!
//! 1. **Lens glow** (`post_lens.wgsl`). Each lit cone's lens is a small
//!    camera-facing disc at its apex. It is as bright as the beam's peak when
//!    the camera is within three quarters of the beam angle, and only a faint
//!    glow from the side. It is hidden by nearer geometry. On with the glare.
//! 2. **Metering** (`post_exposure.wgsl`). A 128-bin log-luminance histogram
//!    of a quarter of the pixels, centre-weighted. The mean of the band
//!    between two percentiles of the lit pixels sets the target exposure,
//!    clamped to `[min_ev, max_ev]`; the compensation is added after the
//!    clamp. Near-black pixels are not metered, so a dark room with a few
//!    beams is exposed for what is lit, and the clamps stop it lifting the
//!    room further. A frame with almost nothing lit holds the last exposure.
//!    The exposure moves toward the target with separate speeds up and down,
//!    and snaps on a standalone frame so a capture is a function of its
//!    frame alone.
//! 3. **Glare** (`post_glare.wgsl`, `post_lens.wgsl`). The exposed frame
//!    above a threshold, at half resolution, down a six-level dual-filter
//!    pyramid and back up, each coarser level weighted less, so the glow
//!    gathers near a source with a long faint tail. On top, by
//!    [`GlareStyle`]: a diffraction pattern (`psf.rs`) drawn as a sprite at
//!    every visible lens (iris or eye), or three lines of Kawase streaks at
//!    quarter resolution (star), or nothing (bloom).
//! 4. **Tonemap** (`post_tonemap.wgsl`). Exposure, the tone curve, HDR
//!    expansion, then the glare added over the tone-mapped picture: it
//!    saturates at the display's white, so a white core stays white and the
//!    halo spreads over its surroundings.

use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

use crate::frame::FixtureCone;
use crate::scene_desc::{GlareStyle, Look};

/// The scene-linear target the composite pass writes for this chain.
pub(crate) const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const GLARE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Pyramid levels, the first at half resolution.
const GLARE_LEVELS: usize = 6;
/// Directions of the star's lines, in degrees. Three lines, six points.
const STREAK_ANGLES_DEG: [f32; 3] = [15.0, 75.0, 135.0];
/// Tap spacing of the three streak passes, in quarter-resolution texels:
/// they reach 3 + 9 + 27 = 39 texels each way, a sixth of a 1080p frame.
const STREAK_SPACING: [f32; 3] = [1.0, 3.0, 9.0];
/// Per-texel attenuation along a streak. Short and bright at the core, so a
/// row of lenses reads as a row of stars rather than a lattice.
const STREAK_ATTENUATION: f32 = 0.93;
/// Weight of each coarser bloom level against the one above it. Below one,
/// the glow gathers near the source and fades into a long faint tail.
const BLOOM_FALLOFF: f32 = 0.7;
/// Diffraction sprite half-size, as a fraction of the frame height.
const PSF_EXTENT_IRIS: f32 = 0.22;
const PSF_EXTENT_EYE: f32 = 0.3;
/// Diffraction gain at `star` = 1, over the physical fraction of the lens's
/// light the pattern holds outside its core. The physical fraction alone is
/// invisible next to the core's glare; this is a look, tuned on Get Lucky at
/// Gasworks so the default `star` of 0.5 reads as faint.
const PSF_GAIN: f32 = 60.0;
/// The eye's corona spreads its light over many streaks and a wider sprite;
/// at the iris's gain it veils the whole rig.
const PSF_EYE_GAIN: f32 = 0.35;
/// Bins plus the black slot, as `post_exposure.wgsl` lays them out.
const HISTOGRAM_SLOTS: u64 = 129;

/// The metered band: the lit pixels between these two percentiles. The
/// lower one ignores the dim majority of a dark room, the upper one the
/// lenses and beam cores.
const METER_LOW: f32 = 0.60;
const METER_HIGH: f32 = 0.98;
/// log2 of the scene-linear value the band's mean is exposed to. Set so an
/// ordinary lit frame meters close to 0 EV, the exposure the transport was
/// tuned at.
const METER_KEY_LOG2: f32 = -1.0;
/// Stops per second of the exposure's approach to its target, per stop of
/// difference: opening up (a darker scene) and closing down (a brighter one).
const ADAPT_UP: f32 = 1.2;
const ADAPT_DOWN: f32 = 3.0;
/// Longest step one adaptation takes. A viewport that sat idle for a minute
/// does not jump on its next frame.
const MAX_ADAPT_STEP_S: f32 = 0.1;

/// Lens radius in metres for a hard beam and for a wide wash.
const LENS_RADIUS_BEAM_M: f32 = 0.05;
const LENS_RADIUS_WASH_M: f32 = 0.12;
/// Scene-linear radiance of a lens on its axis, per unit of cone intensity.
const LENS_GAIN: f32 = 60.0;
/// Depth slack for the lens's own housing: the apex sits inside it.
const LENS_OCCLUSION_SLACK_M: f32 = 0.5;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeterUniform {
    exposure: [f32; 4],
    adapt: [f32; 4],
    meter: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LensUniform {
    view_proj: [[f32; 4]; 4],
    camera: [f32; 4],
    viewport: [f32; 4],
    depth: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    psf: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LensInstance {
    position: [f32; 4],
    direction: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlareUniform {
    texel: [f32; 4],
    streak: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TonemapUniform {
    params: [f32; 4],
    glare: [f32; 4],
}

/// One uniform slot per glare pass, at the device's offset alignment.
const GLARE_SLOT: u64 = 256;
const GLARE_PASSES: usize =
    1 + (GLARE_LEVELS - 1) + (GLARE_LEVELS - 1) + STREAK_ANGLES_DEG.len() * STREAK_SPACING.len();

/// Everything the chain needs from the frame.
pub(crate) struct PostFrame<'a> {
    pub look: Look,
    pub cones: &'a [FixtureCone],
    pub view_proj: Mat4,
    pub eye: Vec3,
    pub target: Vec3,
    pub fov_y_deg: f32,
    pub near: f32,
    pub far: f32,
    pub width: u32,
    pub height: u32,
    /// Display headroom; 1 in SDR.
    pub headroom: f32,
    /// Slot of the tonemap pipeline for the output's format
    /// (`Channels::index`).
    pub output: usize,
    /// Whether the exposure adapts from the previous frame's. A standalone
    /// capture snaps.
    pub temporal: bool,
}

/// Shader modules and pipelines, shared by every renderer.
pub(crate) struct Pipelines {
    exposure_layout: wgpu::BindGroupLayout,
    histogram: wgpu::ComputePipeline,
    adapt: wgpu::ComputePipeline,
    lens_layout: wgpu::BindGroupLayout,
    lens: wgpu::RenderPipeline,
    glare_layout: wgpu::BindGroupLayout,
    prefilter: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    streak: wgpu::RenderPipeline,
    streak_add: wgpu::RenderPipeline,
    psf: wgpu::RenderPipeline,
    /// The iris's and the eye's diffraction patterns (`psf.rs`).
    iris: wgpu::TextureView,
    eye: wgpu::TextureView,
    tonemap_layout: wgpu::BindGroupLayout,
    tonemap: Vec<wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
}

fn entry(
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

fn uniform() -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn storage(read_only: bool) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn texture(filterable: bool) -> wgpu::BindingType {
    wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable },
        view_dimension: wgpu::TextureViewDimension::D2,
        multisampled: false,
    }
}

fn sampler() -> wgpu::BindingType {
    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
}

fn module(device: &wgpu::Device, label: &str, source: String) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

const ADDITIVE: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
};

impl Pipelines {
    /// `output_formats` are the output targets the tonemap writes, in
    /// `Channels::index` order; `hdr` marks the one presented as HDR.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output_formats: &[(wgpu::TextureFormat, bool)],
    ) -> Self {
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let compute = wgpu::ShaderStages::COMPUTE;
        let tone = include_str!("shaders/tone.wgsl");

        let exposure_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-exposure"),
            entries: &[
                entry(0, compute, uniform()),
                entry(1, compute, texture(false)),
                entry(2, compute, storage(false)),
                entry(3, compute, storage(false)),
            ],
        });
        let exposure_module = module(
            device,
            "post-exposure",
            include_str!("shaders/post_exposure.wgsl").to_owned(),
        );
        let compute_pipeline = |label: &str, entry_point: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[Some(&exposure_layout)],
                        immediate_size: 0,
                    }),
                ),
                module: &exposure_module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let histogram = compute_pipeline("post-histogram", "histogram_main");
        let adapt = compute_pipeline("post-adapt", "adapt_main");

        let render_pipeline = |label: &str,
                               layout: &wgpu::BindGroupLayout,
                               module: &wgpu::ShaderModule,
                               entry_point: &str,
                               format: wgpu::TextureFormat,
                               blend: Option<wgpu::BlendState>,
                               constants: &[(&str, f64)]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[Some(layout)],
                        immediate_size: 0,
                    }),
                ),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(entry_point),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants,
                        ..Default::default()
                    },
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let lens_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-lens"),
            entries: &[
                entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT, uniform()),
                entry(1, wgpu::ShaderStages::VERTEX, storage(true)),
                entry(
                    2,
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(3, wgpu::ShaderStages::VERTEX, storage(true)),
                entry(4, fragment, texture(true)),
                entry(5, fragment, sampler()),
            ],
        });
        let lens_module = module(
            device,
            "post-lens",
            include_str!("shaders/post_lens.wgsl").to_owned(),
        );
        let lens = render_pipeline(
            "post-lens",
            &lens_layout,
            &lens_module,
            "fs_main",
            SCENE_FORMAT,
            Some(ADDITIVE),
            &[],
        );

        let psf = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("post-psf"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("post-psf"),
                    bind_group_layouts: &[Some(&lens_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &lens_module,
                entry_point: Some("vs_psf"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &lens_module,
                entry_point: Some("fs_psf"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: GLARE_FORMAT,
                    blend: Some(ADDITIVE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let pattern = |label: &str, aperture: crate::psf::Aperture| {
            use wgpu::util::DeviceExt;
            let side = crate::psf::SIZE as u32;
            device
                .create_texture_with_data(
                    queue,
                    &wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: side,
                            height: side,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    bytemuck::cast_slice(&crate::psf::bake(aperture)),
                )
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let iris = pattern("post-psf-iris", crate::psf::Aperture::Iris);
        let eye = pattern("post-psf-eye", crate::psf::Aperture::Eye);

        let glare_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-glare"),
            entries: &[
                entry(0, fragment, uniform()),
                entry(1, fragment, texture(true)),
                entry(2, fragment, sampler()),
                entry(3, fragment, storage(true)),
                entry(4, fragment, texture(true)),
            ],
        });
        let glare_module = module(
            device,
            "post-glare",
            include_str!("shaders/post_glare.wgsl").to_owned(),
        );
        let glare = |label: &str, entry_point: &str, blend: Option<wgpu::BlendState>| {
            render_pipeline(
                label,
                &glare_layout,
                &glare_module,
                entry_point,
                GLARE_FORMAT,
                blend,
                &[],
            )
        };
        let prefilter = glare("post-glare-prefilter", "fs_prefilter", None);
        let down = glare("post-glare-down", "fs_down", None);
        let up = glare("post-glare-up", "fs_up", None);
        let streak = glare("post-streak", "fs_streak", None);
        let streak_add = glare("post-streak-add", "fs_streak", Some(ADDITIVE));

        let tonemap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-tonemap"),
            entries: &[
                entry(0, fragment, uniform()),
                entry(1, fragment, texture(false)),
                entry(2, fragment, texture(true)),
                entry(3, fragment, texture(true)),
                entry(4, fragment, sampler()),
                entry(5, fragment, storage(true)),
            ],
        });
        let tonemap_module = module(
            device,
            "post-tonemap",
            format!("{tone}{}", include_str!("shaders/post_tonemap.wgsl")),
        );
        let tonemap = output_formats
            .iter()
            .map(|&(format, hdr)| {
                render_pipeline(
                    "post-tonemap",
                    &tonemap_layout,
                    &tonemap_module,
                    "fs_main",
                    format,
                    None,
                    if hdr { &[("HDR_OUTPUT", 1.0)] } else { &[] },
                )
            })
            .collect();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post-linear-clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            exposure_layout,
            histogram,
            adapt,
            lens_layout,
            lens,
            glare_layout,
            prefilter,
            down,
            up,
            streak,
            streak_add,
            psf,
            iris,
            eye,
            tonemap_layout,
            tonemap,
            sampler,
        }
    }
}

struct Level {
    size: [u32; 2],
    view: wgpu::TextureView,
}

/// Sized render targets, reallocated on a size change.
struct Targets {
    size: [u32; 2],
    scene: wgpu::TextureView,
    /// Downsampled levels, half resolution first.
    down: Vec<Level>,
    /// Upsampled sums, one per level but the coarsest.
    up: Vec<Level>,
    /// Streak ping-pong and the star they accumulate into, at quarter
    /// resolution.
    streak: [Level; 3],
    /// Diffraction sprites, at half resolution.
    psf: Level,
}

fn level(device: &wgpu::Device, label: &str, size: [u32; 2], format: wgpu::TextureFormat) -> Level {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    Level {
        size,
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
    }
}

impl Targets {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let halve = |s: [u32; 2]| [s[0].div_ceil(2).max(1), s[1].div_ceil(2).max(1)];
        let mut down = Vec::with_capacity(GLARE_LEVELS);
        let mut s = size;
        for _ in 0..GLARE_LEVELS {
            s = halve(s);
            down.push(level(device, "post-glare-down", s, GLARE_FORMAT));
        }
        let up = down[..GLARE_LEVELS - 1]
            .iter()
            .map(|l| level(device, "post-glare-up", l.size, GLARE_FORMAT))
            .collect();
        let quarter = down[1].size;
        let half = down[0].size;
        Self {
            size,
            scene: level(device, "post-scene", size, SCENE_FORMAT).view,
            down,
            up,
            streak: std::array::from_fn(|_| level(device, "post-streak", quarter, GLARE_FORMAT)),
            psf: level(device, "post-psf", half, GLARE_FORMAT),
        }
    }
}

/// One renderer's post state: targets, the exposure it has adapted to, and
/// the per-frame uniform buffers.
pub(crate) struct Post {
    targets: Option<Targets>,
    histogram: wgpu::Buffer,
    state: wgpu::Buffer,
    meter: wgpu::Buffer,
    lens: wgpu::Buffer,
    lenses: Option<(wgpu::Buffer, u64)>,
    glare: wgpu::Buffer,
    tonemap: wgpu::Buffer,
    last_adapt: Option<Instant>,
    /// Metering mode of the last frame; switching it snaps.
    was_auto: Option<bool>,
}

impl Post {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let buffer = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            targets: None,
            histogram: buffer(
                "post-histogram",
                HISTOGRAM_SLOTS * 4,
                wgpu::BufferUsages::STORAGE,
            ),
            state: buffer(
                "post-exposure-state",
                16,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            ),
            meter: buffer(
                "post-meter",
                std::mem::size_of::<MeterUniform>() as u64,
                wgpu::BufferUsages::UNIFORM,
            ),
            lens: buffer(
                "post-lens",
                std::mem::size_of::<LensUniform>() as u64,
                wgpu::BufferUsages::UNIFORM,
            ),
            lenses: None,
            glare: buffer(
                "post-glare",
                GLARE_SLOT * GLARE_PASSES as u64,
                wgpu::BufferUsages::UNIFORM,
            ),
            tonemap: buffer(
                "post-tonemap",
                std::mem::size_of::<TonemapUniform>() as u64,
                wgpu::BufferUsages::UNIFORM,
            ),
            last_adapt: None,
            was_auto: None,
        }
    }

    /// The exposure the last post-chain frame used and the one it was
    /// adapting toward, in stops. Blocks on the GPU; for tests and tools.
    pub(crate) fn read_exposure(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<[f32; 2]> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post-exposure-readback"),
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&self.state, 0, &staging, 0, 16);
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let view = staging.slice(..).get_mapped_range()?;
        let state: [f32; 4] = bytemuck::cast_slice::<u8, f32>(&view)[..4]
            .try_into()
            .expect("four floats");
        Ok([state[0], state[1]])
    }

    /// The scene-linear target the composite pass writes into, at the
    /// output's size.
    pub(crate) fn scene_target(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> wgpu::TextureView {
        let size = [width.max(1), height.max(1)];
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(Targets::new(device, size));
        }
        self.targets.as_ref().expect("just allocated").scene.clone()
    }

    /// Encode the chain after the composite pass has filled
    /// [`Self::scene_target`]. `last` carries the frame's final timestamp:
    /// the tonemap pass is now the sink every other pass feeds.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode<'q>(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &PostFrame<'_>,
        depth: &wgpu::TextureView,
        output: &wgpu::TextureView,
        pass_queries: &mut crate::pass_profile::PassQueries<'q>,
        last: Option<wgpu::RenderPassTimestampWrites<'q>>,
    ) {
        let targets = self
            .targets
            .as_ref()
            .expect("scene_target allocates the targets first");
        let look = frame.look;
        let glare_on = look.glare.on();
        let style = look.glare.style;
        let pattern = match style {
            GlareStyle::Aperture => Some((&pipelines.iris, PSF_EXTENT_IRIS, PSF_GAIN)),
            GlareStyle::Eye => Some((&pipelines.eye, PSF_EXTENT_EYE, PSF_GAIN * PSF_EYE_GAIN)),
            GlareStyle::Bloom | GlareStyle::Star => None,
        };
        let mut lens_bind = None;

        // --- lens glow ------------------------------------------------------
        let lenses: Vec<LensInstance> = if glare_on {
            frame
                .cones
                .iter()
                .filter(|cone| cone.intensity > 0.0)
                .map(|cone| LensInstance {
                    position: cone
                        .position
                        .extend(
                            LENS_RADIUS_BEAM_M
                                + (LENS_RADIUS_WASH_M - LENS_RADIUS_BEAM_M) * cone.wash,
                        )
                        .to_array(),
                    direction: cone.direction.extend(cone.cos_beam).to_array(),
                    color: cone.color.extend(cone.intensity).to_array(),
                })
                .collect()
        } else {
            Vec::new()
        };
        if !lenses.is_empty() {
            let bytes = bytemuck::cast_slice::<_, u8>(&lenses);
            let needed = bytes.len() as u64;
            if self.lenses.as_ref().is_none_or(|(_, size)| *size < needed) {
                let size = needed.next_power_of_two().max(1024);
                self.lenses = Some((
                    device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("post-lenses"),
                        size,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    size,
                ));
            }
            let (lens_buffer, _) = self.lenses.as_ref().expect("just allocated");
            queue.write_buffer(lens_buffer, 0, bytes);
            let forward = (frame.target - frame.eye).normalize_or(Vec3::Y);
            let right = forward.cross(Vec3::Z).normalize_or(Vec3::X);
            let up = right.cross(forward);
            let focal = frame.height as f32 * 0.5 / (frame.fov_y_deg.to_radians() * 0.5).tan();
            queue.write_buffer(
                &self.lens,
                0,
                bytemuck::bytes_of(&LensUniform {
                    view_proj: frame.view_proj.to_cols_array_2d(),
                    camera: frame.eye.extend(1.0).to_array(),
                    viewport: [
                        frame.width as f32,
                        frame.height as f32,
                        focal,
                        LENS_GAIN * look.glare.strength.min(1.0),
                    ],
                    depth: [frame.near, frame.far, LENS_OCCLUSION_SLACK_M, 0.0],
                    right: right.extend(0.0).to_array(),
                    up: up.extend(0.0).to_array(),
                    psf: [
                        pattern.map_or(0.0, |(_, extent, _)| extent),
                        crate::psf::SIZE as f32,
                        pattern.map_or(0.0, |(_, _, gain)| gain) * look.glare.star,
                        0.0,
                    ],
                }),
            );
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post-lens"),
                layout: &pipelines.lens_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.lens.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: lens_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(depth),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.state.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(
                            pattern.map_or(&pipelines.eye, |(view, _, _)| view),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post-lens"),
                color_attachments: &[Some(attachment(&targets.scene, false))],
                depth_stencil_attachment: None,
                timestamp_writes: pass_queries.render("post-lens", None),
                ..Default::default()
            });
            pass.set_pipeline(&pipelines.lens);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..6, 0..lenses.len() as u32);
            drop(pass);
            lens_bind = Some(bind);
        }

        // --- metering ---------------------------------------------------------
        let now = Instant::now();
        let auto = look.exposure.auto;
        let snap = !frame.temporal || self.was_auto != Some(auto) || self.last_adapt.is_none();
        let dt = self.last_adapt.map_or(0.0, |last| {
            now.duration_since(last).as_secs_f32().min(MAX_ADAPT_STEP_S)
        });
        self.last_adapt = Some(now);
        self.was_auto = Some(auto);
        queue.write_buffer(
            &self.meter,
            0,
            bytemuck::bytes_of(&MeterUniform {
                exposure: [
                    f32::from(u8::from(auto)),
                    look.exposure.ev,
                    look.exposure.min_ev,
                    look.exposure.max_ev.max(look.exposure.min_ev),
                ],
                adapt: [dt, f32::from(u8::from(snap)), ADAPT_UP, ADAPT_DOWN],
                meter: [METER_LOW, METER_HIGH, METER_KEY_LOG2, 1.0],
            }),
        );
        let exposure_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post-exposure"),
            layout: &pipelines.exposure_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.meter.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.scene),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.histogram.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.state.as_entire_binding(),
                },
            ],
        });
        if auto {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("post-histogram"),
                timestamp_writes: pass_queries.compute("post-histogram", None),
            });
            pass.set_pipeline(&pipelines.histogram);
            pass.set_bind_group(0, &exposure_bind, &[]);
            let blocks = [frame.width.div_ceil(2), frame.height.div_ceil(2)];
            pass.dispatch_workgroups(blocks[0].div_ceil(16), blocks[1].div_ceil(16), 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("post-adapt"),
                timestamp_writes: pass_queries.compute("post-adapt", None),
            });
            pass.set_pipeline(&pipelines.adapt);
            pass.set_bind_group(0, &exposure_bind, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }

        // --- diffraction ------------------------------------------------------
        // Cleared whenever the glare is on: the tonemap reads it for every
        // style but the drawn star.
        if glare_on && style != GlareStyle::Star {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post-psf"),
                color_attachments: &[Some(attachment(&targets.psf.view, true))],
                depth_stencil_attachment: None,
                timestamp_writes: pass_queries.render("post-psf", None),
                ..Default::default()
            });
            if let (Some(bind), Some(_)) = (&lens_bind, pattern) {
                pass.set_pipeline(&pipelines.psf);
                pass.set_bind_group(0, bind, &[]);
                pass.draw(0..6, 0..lenses.len() as u32);
            }
        }

        // --- glare ------------------------------------------------------------
        if glare_on {
            let mut slots: Vec<GlareUniform> = Vec::with_capacity(GLARE_PASSES);
            let texel = |size: [u32; 2], scale: f32| {
                [
                    scale / size[0] as f32,
                    scale / size[1] as f32,
                    look.glare.threshold,
                    look.glare.threshold * 0.5,
                ]
            };
            // Prefilter reads the full-resolution scene.
            slots.push(GlareUniform {
                texel: texel(targets.size, 1.0),
                streak: [0.0; 4],
            });
            for i in 1..GLARE_LEVELS {
                slots.push(GlareUniform {
                    texel: texel(targets.down[i - 1].size, 1.0),
                    streak: [0.0; 4],
                });
            }
            for i in (0..GLARE_LEVELS - 1).rev() {
                slots.push(GlareUniform {
                    texel: texel(targets.down[i + 1].size, 0.5),
                    streak: [BLOOM_FALLOFF, 0.0, 0.0, 0.0],
                });
            }
            let quarter = targets.down[1].size;
            for angle in STREAK_ANGLES_DEG {
                let (s, c) = angle.to_radians().sin_cos();
                for spacing in STREAK_SPACING {
                    slots.push(GlareUniform {
                        texel: texel(quarter, 1.0),
                        streak: [c, s, spacing, STREAK_ATTENUATION],
                    });
                }
            }
            let mut bytes = vec![0u8; GLARE_SLOT as usize * slots.len()];
            for (i, slot) in slots.iter().enumerate() {
                let at = i * GLARE_SLOT as usize;
                bytes[at..at + std::mem::size_of::<GlareUniform>()]
                    .copy_from_slice(bytemuck::bytes_of(slot));
            }
            queue.write_buffer(&self.glare, 0, &bytes);

            let mut slot = 0u64;
            let mut glare_pass = |encoder: &mut wgpu::CommandEncoder,
                                  pass_queries: &mut crate::pass_profile::PassQueries<'q>,
                                  label: &'static str,
                                  pipeline: &wgpu::RenderPipeline,
                                  source: &wgpu::TextureView,
                                  detail: &wgpu::TextureView,
                                  target: &wgpu::TextureView,
                                  load: bool| {
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &pipelines.glare_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &self.glare,
                                offset: slot * GLARE_SLOT,
                                size: wgpu::BufferSize::new(
                                    std::mem::size_of::<GlareUniform>() as u64
                                ),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(source),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: self.state.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(detail),
                        },
                    ],
                });
                slot += 1;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(label),
                    color_attachments: &[Some(attachment(target, !load))],
                    depth_stencil_attachment: None,
                    timestamp_writes: pass_queries.render(label, None),
                    ..Default::default()
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            };
            // `detail` is unused outside the upsample; any texture that is
            // not this pass's target serves.
            let unused = &targets.streak[2].view;
            glare_pass(
                encoder,
                pass_queries,
                "post-glare-prefilter",
                &pipelines.prefilter,
                &targets.scene,
                unused,
                &targets.down[0].view,
                false,
            );
            for i in 1..GLARE_LEVELS {
                glare_pass(
                    encoder,
                    pass_queries,
                    "post-glare-down",
                    &pipelines.down,
                    &targets.down[i - 1].view,
                    unused,
                    &targets.down[i].view,
                    false,
                );
            }
            for i in (0..GLARE_LEVELS - 1).rev() {
                let coarser = if i + 1 == GLARE_LEVELS - 1 {
                    &targets.down[i + 1].view
                } else {
                    &targets.up[i + 1].view
                };
                glare_pass(
                    encoder,
                    pass_queries,
                    "post-glare-up",
                    &pipelines.up,
                    coarser,
                    &targets.down[i].view,
                    &targets.up[i].view,
                    false,
                );
            }
            for (line, _) in STREAK_ANGLES_DEG
                .iter()
                .enumerate()
                .filter(|_| style == GlareStyle::Star)
            {
                let [a, b, star] = &targets.streak;
                glare_pass(
                    encoder,
                    pass_queries,
                    "post-streak",
                    &pipelines.streak,
                    &targets.down[1].view,
                    unused,
                    &a.view,
                    false,
                );
                glare_pass(
                    encoder,
                    pass_queries,
                    "post-streak",
                    &pipelines.streak,
                    &a.view,
                    unused,
                    &b.view,
                    false,
                );
                let (pipeline, load) = if line == 0 {
                    (&pipelines.streak, false)
                } else {
                    (&pipelines.streak_add, true)
                };
                glare_pass(
                    encoder,
                    pass_queries,
                    "post-streak",
                    pipeline,
                    &b.view,
                    &a.view,
                    &star.view,
                    load,
                );
            }
        }

        // --- tonemap ----------------------------------------------------------
        queue.write_buffer(
            &self.tonemap,
            0,
            bytemuck::bytes_of(&TonemapUniform {
                params: [
                    look.tone.shader_code() as f32,
                    if glare_on { look.glare.strength } else { 0.0 },
                    // The diffraction sprites carry `star` in their own gain.
                    match style {
                        GlareStyle::Bloom => 0.0,
                        GlareStyle::Star => look.glare.star,
                        GlareStyle::Aperture | GlareStyle::Eye => 1.0,
                    },
                    frame.headroom,
                ],
                glare: [
                    1.0 / (0..GLARE_LEVELS)
                        .map(|level| BLOOM_FALLOFF.powi(level as i32))
                        .sum::<f32>(),
                    if style == GlareStyle::Star {
                        1.0 / STREAK_ANGLES_DEG.len() as f32
                    } else {
                        1.0
                    },
                    0.0,
                    0.0,
                ],
            }),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post-tonemap"),
            layout: &pipelines.tonemap_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.tonemap.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.scene),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&targets.up[0].view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(if style == GlareStyle::Star {
                        &targets.streak[2].view
                    } else {
                        &targets.psf.view
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.state.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("post-tonemap"),
            color_attachments: &[Some(attachment(output, true))],
            depth_stencil_attachment: None,
            timestamp_writes: pass_queries.render("post-tonemap", last),
            ..Default::default()
        });
        pass.set_pipeline(&pipelines.tonemap[frame.output]);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn attachment(view: &wgpu::TextureView, clear: bool) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        resolve_target: None,
        depth_slice: None,
        ops: wgpu::Operations {
            load: if clear {
                wgpu::LoadOp::Clear(wgpu::Color::BLACK)
            } else {
                wgpu::LoadOp::Load
            },
            store: wgpu::StoreOp::Store,
        },
    }
}
