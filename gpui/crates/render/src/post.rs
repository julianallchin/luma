//! The camera: auto-exposure, lens glow, tone curve and glare.
//!
//! Runs only when a frame's [`Look`] asks for more than the single composite
//! pass does ([`Look::needs_post`]). The composite pass then writes
//! scene-linear light into [`Post::scene_target`] instead of the output, and
//! this chain finishes the frame:
//!
//! 1. **Lens glow** (`post_lens.wgsl`). Each lit cone's lens is a small
//!    camera-facing disc at its lens centre, the lens's own radius. It is as bright as the beam's peak when
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
//! 3. **Glare** (`post_glare.wgsl`, `psf.rs`). The exposed frame's light
//!    above a threshold, averaged into a padded FFT grid (1024 × 512 at High,
//!    512 × 256 at Low; the frame fills at most its top-left quarter), and
//!    convolved with the glare kernel by FFT: the whole frame at once, so an
//!    extended or oddly shaped bright area glares as its shape does
//!    (Ritschel et al. 2009), not as a sprite per lens. The kernel is the
//!    Vos veil plus, by [`GlareStyle`], a diffraction pattern of an iris, a
//!    hexagonal star or an eye, with the lens's dust and scratches; its
//!    spectrum is computed again only when the style, the diffraction amount
//!    or the field of view changes.
//! 4. **Tonemap** (`post_tonemap.wgsl`). Exposure, the footage look's
//!    sensor noise, the tone curve, HDR expansion, then the glare added over
//!    the tone-mapped picture: it saturates at the display's white, so a
//!    white core stays white and the halo spreads over its surroundings.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

use crate::frame::FixtureCone;
use crate::psf;
use crate::scene_desc::{GlareStyle, Look, Quality};

/// The scene-linear target the composite pass writes for this chain.
pub(crate) const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const GLARE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The glare's FFT grid, texels across and down, by quality. The frame is
/// averaged into at most half of each side, so the kernel can reach across
/// the whole frame before the cyclic convolution folds it back. At High a
/// 16:9 frame is 455 × 256 texels, about 4 pixels each at 1080p: fine enough
/// for a scratch's streak, and 1024 complex texels is the most one row
/// transform holds in 16 KiB of workgroup memory.
const GRID_HIGH: [usize; 2] = [1024, 512];
const GRID_LOW: [usize; 2] = [512, 256];
/// The `star` setting at which the diffraction pattern is physical.
const DIFFRACTION_PHYSICAL: f32 = 0.5;
/// Relative change in the grid's focal length or reach that rebuilds the
/// kernel.
const KERNEL_TOLERANCE: f32 = 0.01;
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

/// Lens radius in metres for a point-source cone (a house lamp, a synthetic
/// test cone), which carries none: a hard beam and a wide wash.
const LENS_RADIUS_BEAM_M: f32 = 0.05;
const LENS_RADIUS_WASH_M: f32 = 0.12;
/// Scene-linear radiance of a lens on its axis, per unit of cone intensity:
/// about 600 times diffuse white. A real lamp's lens is thousands, and its
/// glare comes from that range. The glare kernel is physical, so a
/// reflection on a truss, a few times white, glares only faintly; a gain on
/// the glare itself made every such reflection bloom like a lens.
const LENS_GAIN: f32 = 600.0;
/// Depth slack for the lens's own glass and bezel, which sit at its centre.
const LENS_OCCLUSION_SLACK_M: f32 = 0.1;

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
struct FftUniform {
    region: [u32; 4],
    scene: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TonemapUniform {
    params: [f32; 4],
    glare: [f32; 4],
    /// xyz: toward the sun in camera space (x right, y up, z forward),
    /// w: how much of its glare the tonemap draws (0 while the frame holds it).
    sun: [f32; 4],
    /// rgb: the sun's light times the veil's scale, in exposed light ·
    /// square degrees. w: focal length, output pixels.
    sun_veil: [f32; 4],
}

/// The sensor noise's seed for the frame at `time`: a new grain every frame,
/// and the same grain for the same frame on every run. Below 2^24, so the
/// uniform's `f32` holds it exactly.
fn noise_seed(time: f32) -> f32 {
    let mut x = time.to_bits();
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    (x & 0x00ff_ffff) as f32
}

/// Everything the chain needs from the frame.
pub(crate) struct PostFrame<'a> {
    pub look: Look,
    pub quality: Quality,
    pub cones: &'a [FixtureCone],
    pub view_proj: Mat4,
    pub eye: Vec3,
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
    /// The frame's clock, in seconds. The exposure adapts over its steps, so
    /// a file rendered faster than real time adapts as the viewport does.
    pub time: f32,
    /// Direction toward the sun and its disc's light (radiance × solid
    /// angle), when the sky draws one.
    pub sun: Option<(Vec3, Vec3)>,
}

/// The glare's compute pipelines for one grid size.
struct Fft {
    grid: [usize; 2],
    rows_forward: wgpu::ComputePipeline,
    rows_kernel: wgpu::ComputePipeline,
    columns_forward: wgpu::ComputePipeline,
    columns_convolve: wgpu::ComputePipeline,
    rows_inverse: wgpu::ComputePipeline,
}

impl Fft {
    /// Spectrum columns: the non-negative frequencies of a row.
    fn half(&self) -> u32 {
        (self.grid[0] / 2 + 1) as u32
    }

    /// Bytes of one spectrum: three planes of complex f32.
    fn spectrum_bytes(&self) -> u64 {
        3 * u64::from(self.half()) * self.grid[1] as u64 * 8
    }
}

/// Shader modules and pipelines, shared by every renderer.
pub(crate) struct Pipelines {
    exposure_layout: wgpu::BindGroupLayout,
    histogram: wgpu::ComputePipeline,
    adapt: wgpu::ComputePipeline,
    lens_layout: wgpu::BindGroupLayout,
    lens: wgpu::RenderPipeline,
    fft_layout: wgpu::BindGroupLayout,
    /// High, then Low.
    fft: [Fft; 2],
    /// The apertures' diffraction patterns, baked once on a thread of their
    /// own (about 85 ms on 32 cores, 190 ms on 4) and waited for by the first
    /// frame with glare.
    patterns: std::sync::OnceLock<psf::Patterns>,
    baking: std::sync::Mutex<Option<std::thread::JoinHandle<psf::Patterns>>>,
    tonemap_layout: wgpu::BindGroupLayout,
    tonemap: Vec<wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    /// Bound in place of the glare when there is none.
    no_glare: wgpu::TextureView,
    /// Bound in place of the kernel texels outside a kernel build.
    no_kernel: wgpu::Buffer,
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
        let compute_pipeline = |label: &str,
                                layout: &wgpu::BindGroupLayout,
                                module: &wgpu::ShaderModule,
                                entry_point: &str| {
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
        let exposure_module = module(
            device,
            "post-exposure",
            include_str!("shaders/post_exposure.wgsl").to_owned(),
        );
        let histogram = compute_pipeline(
            "post-histogram",
            &exposure_layout,
            &exposure_module,
            "histogram_main",
        );
        let adapt = compute_pipeline(
            "post-adapt",
            &exposure_layout,
            &exposure_module,
            "adapt_main",
        );

        let render_pipeline = |label: &str,
                               layout: &wgpu::BindGroupLayout,
                               module: &wgpu::ShaderModule,
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
                    entry_point: Some("fs_main"),
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
            SCENE_FORMAT,
            Some(ADDITIVE),
            &[],
        );

        let fft_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-glare"),
            entries: &[
                entry(0, compute, uniform()),
                entry(1, compute, texture(false)),
                entry(2, compute, storage(true)),
                entry(3, compute, storage(false)),
                entry(4, compute, storage(false)),
                entry(5, compute, storage(true)),
                entry(
                    6,
                    compute,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: GLARE_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let fft = [GRID_HIGH, GRID_LOW].map(|grid| {
            let module = module(
                device,
                "post-glare",
                format!(
                    "const ROW: u32 = {}u;\nconst COL: u32 = {}u;\n{}",
                    grid[0],
                    grid[1],
                    include_str!("shaders/post_glare.wgsl")
                ),
            );
            let pipeline = |label: &str, entry_point: &str| {
                compute_pipeline(label, &fft_layout, &module, entry_point)
            };
            Fft {
                grid,
                rows_forward: pipeline("post-glare-rows", "rows_forward"),
                rows_kernel: pipeline("post-glare-kernel-rows", "rows_kernel"),
                columns_forward: pipeline("post-glare-kernel-columns", "columns_forward"),
                columns_convolve: pipeline("post-glare-columns", "columns_convolve"),
                rows_inverse: pipeline("post-glare-inverse", "rows_inverse"),
            }
        });
        let baking = std::thread::Builder::new()
            .name("glare-patterns".into())
            .spawn(psf::Patterns::bake)
            .ok();

        let tonemap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-tonemap"),
            entries: &[
                entry(0, fragment, uniform()),
                entry(1, fragment, texture(false)),
                entry(2, fragment, texture(true)),
                entry(3, fragment, sampler()),
                entry(4, fragment, storage(true)),
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
        let no_glare = {
            use wgpu::util::DeviceExt;
            device
                .create_texture_with_data(
                    queue,
                    &wgpu::TextureDescriptor {
                        label: Some("post-no-glare"),
                        size: wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: GLARE_FORMAT,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &[0; 8],
                )
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let no_kernel = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post-no-kernel"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        Self {
            exposure_layout,
            histogram,
            adapt,
            lens_layout,
            lens,
            fft_layout,
            fft,
            patterns: std::sync::OnceLock::new(),
            baking: std::sync::Mutex::new(baking),
            tonemap_layout,
            tonemap,
            sampler,
            no_glare,
            no_kernel,
        }
    }
}

impl Pipelines {
    fn patterns(&self) -> &psf::Patterns {
        self.patterns.get_or_init(|| {
            let baking = self.baking.lock().map(|mut b| b.take()).ok().flatten();
            baking
                .and_then(|handle| handle.join().ok())
                .unwrap_or_else(psf::Patterns::bake)
        })
    }
}

/// What the glare kernel was built for; a different key builds it again.
#[derive(Clone, Copy, PartialEq)]
struct KernelKey {
    style: GlareStyle,
    diffraction: f32,
    focal: f32,
    reach: [f32; 2],
}

impl KernelKey {
    /// Whether a kernel built for `self` serves `other`. The frame's shape
    /// moves the focal length and the reach a little on every resize; a
    /// percent is invisible, and rebuilding costs 15–30 ms of CPU.
    fn matches(&self, other: &Self) -> bool {
        let near = |a: f32, b: f32| (a / b - 1.0).abs() < KERNEL_TOLERANCE;
        self.style == other.style
            && self.diffraction == other.diffraction
            && near(self.focal, other.focal)
            && near(self.reach[0], other.reach[0])
            && near(self.reach[1], other.reach[1])
    }
}

/// The glare's buffers for one frame size and grid.
struct Glare {
    /// Index into [`Pipelines::fft`].
    fft: usize,
    /// The frame's extent in grid texels, rounded up.
    region: [u32; 2],
    /// Frame pixels per grid texel.
    scale: f32,
    /// The convolved glare, `region` texels.
    texture: wgpu::TextureView,
    spectrum: wgpu::Buffer,
    kernel: wgpu::Buffer,
    built: Option<KernelKey>,
}

impl Glare {
    fn new(device: &wgpu::Device, fft: &Fft, index: usize, size: [u32; 2]) -> Self {
        let [gw, gh] = fft.grid;
        // At most half the grid each way, and never finer than the frame.
        let scale = (size[0] as f32 / (gw / 2) as f32)
            .max(size[1] as f32 / (gh / 2) as f32)
            .max(1.0);
        let region = [
            ((size[0] as f32 / scale).ceil() as u32).clamp(1, gw as u32 / 2),
            ((size[1] as f32 / scale).ceil() as u32).clamp(1, gh as u32 / 2),
        ];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("post-glare"),
            size: wgpu::Extent3d {
                width: region[0],
                height: region[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GLARE_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let buffer = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: fft.spectrum_bytes(),
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        };
        Self {
            fft: index,
            region,
            scale,
            texture: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            spectrum: buffer("post-glare-spectrum"),
            kernel: buffer("post-glare-kernel"),
            built: None,
        }
    }
}

/// Sized render targets, reallocated on a size change.
struct Targets {
    size: [u32; 2],
    scene: wgpu::TextureView,
    /// Allocated on the first frame with glare.
    glare: Option<Glare>,
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
    fft: wgpu::Buffer,
    tonemap: wgpu::Buffer,
    /// The clock of the last metered frame.
    last_adapt: Option<f32>,
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
            fft: buffer(
                "post-glare",
                std::mem::size_of::<FftUniform>() as u64,
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
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("post-scene"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: SCENE_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.targets = Some(Targets {
                size,
                scene: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                glare: None,
            });
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
        let look = frame.look;
        let glare_on = look.glare.on();

        // --- lens glow ------------------------------------------------------
        let lenses: Vec<LensInstance> = if glare_on {
            frame
                .cones
                .iter()
                .filter(|cone| cone.intensity > 0.0)
                .map(|cone| LensInstance {
                    // `position` is the lens centre on the front glass.
                    position: cone
                        .position
                        .extend(if cone.lens.radius > 0.0 {
                            cone.lens.radius.min(crate::luminaire::Lens::MAX_RADIUS_M)
                        } else {
                            LENS_RADIUS_BEAM_M
                                + (LENS_RADIUS_WASH_M - LENS_RADIUS_BEAM_M) * cone.wash
                        })
                        .to_array(),
                    direction: cone.direction.extend(cone.cos_beam).to_array(),
                    color: cone.color.extend(cone.intensity).to_array(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let targets = self
            .targets
            .as_mut()
            .expect("scene_target allocates the targets first");
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
        }

        // --- metering ---------------------------------------------------------
        let auto = look.exposure.auto;
        let snap = !frame.temporal || self.was_auto != Some(auto) || self.last_adapt.is_none();
        let dt = self.last_adapt.map_or(0.0, |last| {
            (frame.time - last).max(0.0).min(MAX_ADAPT_STEP_S)
        });
        self.last_adapt = Some(frame.time);
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

        // --- glare ------------------------------------------------------------
        let index = match frame.quality {
            Quality::High => 0,
            Quality::Low => 1,
        };
        let fft = &pipelines.fft[index];
        if glare_on && targets.glare.as_ref().is_none_or(|g| g.fft != index) {
            targets.glare = Some(Glare::new(device, fft, index, targets.size));
        }
        let glare = targets.glare.as_mut().filter(|_| glare_on);
        let mut uv_scale = [1.0f32; 2];
        let mut texel_px = 1.0f32;
        if let Some(glare) = glare {
            texel_px = glare.scale;
            let frame_texels = [
                targets.size[0] as f32 / glare.scale,
                targets.size[1] as f32 / glare.scale,
            ];
            uv_scale = [
                frame_texels[0] / glare.region[0] as f32,
                frame_texels[1] / glare.region[1] as f32,
            ];
            queue.write_buffer(
                &self.fft,
                0,
                bytemuck::bytes_of(&FftUniform {
                    region: [glare.region[0], glare.region[1], 0, 0],
                    scene: [
                        glare.scale,
                        look.glare.threshold,
                        look.glare.threshold * 0.5,
                        0.0,
                    ],
                }),
            );
            let bind = |kernel_texels: &wgpu::Buffer| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("post-glare"),
                    layout: &pipelines.fft_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.fft.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&targets.scene),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.state.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: glare.spectrum.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: glare.kernel.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: kernel_texels.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(&glare.texture),
                        },
                    ],
                })
            };

            // The kernel's spectrum, when the style, the diffraction or the
            // lens has changed.
            let grid = psf::Grid {
                width: fft.grid[0],
                height: fft.grid[1],
                focal: frame_texels[1] * 0.5 / (frame.fov_y_deg.to_radians() * 0.5).tan(),
                reach: [
                    fft.grid[0] as f32 - frame_texels[0] - 1.0,
                    fft.grid[1] as f32 - frame_texels[1] - 1.0,
                ],
            };
            let key = KernelKey {
                style: look.glare.style,
                diffraction: look.glare.star / DIFFRACTION_PHYSICAL,
                focal: grid.focal,
                reach: grid.reach,
            };
            if glare.built.is_none_or(|built| !built.matches(&key)) {
                use wgpu::util::DeviceExt;
                let texels = psf::kernel(key.style, key.diffraction, &grid, pipelines.patterns());
                let kernel_texels = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("post-glare-kernel-texels"),
                    contents: bytemuck::cast_slice(&texels),
                    usage: wgpu::BufferUsages::STORAGE,
                });
                let bind = bind(&kernel_texels);
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("post-glare-kernel"),
                    timestamp_writes: pass_queries.compute("post-glare-kernel", None),
                });
                pass.set_bind_group(0, &bind, &[]);
                pass.set_pipeline(&fft.rows_kernel);
                pass.dispatch_workgroups(fft.grid[1] as u32, 1, 1);
                pass.set_pipeline(&fft.columns_forward);
                pass.dispatch_workgroups(fft.half(), 1, 1);
                glare.built = Some(key);
            }

            let bind = bind(&pipelines.no_kernel);
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("post-glare"),
                timestamp_writes: pass_queries.compute("post-glare", None),
            });
            pass.set_bind_group(0, &bind, &[]);
            pass.set_pipeline(&fft.rows_forward);
            pass.dispatch_workgroups(glare.region[1], 1, 1);
            pass.set_pipeline(&fft.columns_convolve);
            pass.dispatch_workgroups(fft.half(), 1, 1);
            pass.set_pipeline(&fft.rows_inverse);
            pass.dispatch_workgroups(glare.region[1], 1, 1);
        }

        // --- tonemap ----------------------------------------------------------
        let sun_glare = if glare_on {
            off_frame_sun(frame, texel_px)
        } else {
            ([0.0; 4], [0.0; 4])
        };
        queue.write_buffer(
            &self.tonemap,
            0,
            bytemuck::bytes_of(&TonemapUniform {
                params: [
                    look.tone.shader_code() as f32,
                    if glare_on { look.glare.strength } else { 0.0 },
                    if look.footage.enabled {
                        look.footage.sanitized().noise
                    } else {
                        0.0
                    },
                    frame.headroom,
                ],
                glare: [uv_scale[0], uv_scale[1], noise_seed(frame.time), 0.0],
                sun: sun_glare.0,
                sun_veil: sun_glare.1,
            }),
        );
        let glare_view = targets
            .glare
            .as_ref()
            .filter(|_| glare_on)
            .map_or(&pipelines.no_glare, |g| &g.texture);
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
                    resource: wgpu::BindingResource::TextureView(glare_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
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

/// The sun's glare while it is off the frame, for the tonemap to draw.
///
/// The convolution only sees light in the frame, so a sun just past its
/// edge would take all its glare with it; a real lens is still lit by it.
/// The tonemap draws the sun's veil (not its diffraction rays) from its
/// direction, and fades that in over a few glare texels as the disc leaves
/// the frame, as the convolution's own copy fades out.
fn off_frame_sun(frame: &PostFrame<'_>, texel_px: f32) -> ([f32; 4], [f32; 4]) {
    let Some((direction, light)) = frame.sun else {
        return ([0.0; 4], [0.0; 4]);
    };
    let inverse = frame.view_proj.inverse();
    let at = |x: f32, y: f32| inverse.project_point3(Vec3::new(x, y, 1.0));
    let centre = at(0.0, 0.0);
    let forward = (centre - frame.eye).normalize();
    let right = (at(1.0, 0.0) - centre).normalize();
    let up = (at(0.0, 1.0) - centre).normalize();
    let s = direction.normalize();
    let camera = Vec3::new(s.dot(right), s.dot(up), s.dot(forward));
    let (w, h) = (frame.width as f32, frame.height as f32);
    let focal = h * 0.5 / (frame.fov_y_deg.to_radians() * 0.5).tan();
    // Signed distance of the sun's centre inside the frame, pixels.
    let inside = if camera.z > 1e-4 {
        let x = w * 0.5 + focal * camera.x / camera.z;
        let y = h * 0.5 - focal * camera.y / camera.z;
        x.min(w - x).min(y).min(h - y)
    } else {
        -f32::INFINITY
    };
    // Two glare texels.
    let margin = 2.0 * texel_px;
    let off = 1.0 - ((inside + margin) / (2.0 * margin)).clamp(0.0, 1.0);
    let scale =
        crate::psf::veil_scale(frame.look.glare.style) * (180.0 / std::f32::consts::PI).powi(2);
    let veil = light * scale;
    (
        camera.extend(off).to_array(),
        [veil.x, veil.y, veil.z, focal],
    )
}
