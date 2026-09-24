//! HDR10 output for the wgpu compositor.
//!
//! LUMA LOCAL EDIT: not upstream.
//!
//! # The contract
//!
//! In HDR mode the swapchain is BT.2100 PQ, but none of the compositor's own
//! pipelines change. They draw into an `Rgba16Float` scene target holding the
//! same sRGB-encoded values an SDR frame holds, so every blend gives the same
//! result it gives in SDR. One last full-screen pass ([`HdrEncoder`]) decodes
//! that target as extended sRGB, converts BT.709 to BT.2020, scales colour
//! value 1.0 to SDR white and PQ-encodes. The user interface (values <= 1)
//! therefore shows at exactly the luminance the display gives every other SDR
//! window. A painted surface in `Rgba16Float` holds *linear* values, which
//! may exceed 1.0; the compositor encodes it to extended sRGB as it samples
//! it (`fs_surface_linear`), so it lands in the scene target in the same
//! encoding as everything else.
//!
//! # Where the luminance range comes from
//!
//! [`HdrOutput`] describes the display. Today it is read from the variables a
//! wlroots compositor (Sway) configures its HDR output with. It is one value
//! passed to the one place that needs it, so the Wayland color-management
//! protocol can supply it later instead.

use bytemuck::{Pod, Zeroable};
use gpui::HdrOutput;

/// The scene target's format in HDR mode.
pub(crate) const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// SDR white where the compositor does not say: BT.2408's reference white,
/// and wlroots' own default.
const DEFAULT_SDR_WHITE_NITS: f32 = 203.0;
/// Display peak where the compositor does not say.
const DEFAULT_PEAK_NITS: f32 = 1000.0;

/// The display's luminance range, as the compositor's environment states it.
///
/// `WLR_SDR_NITS` is the luminance wlroots maps SDR windows to; it is used
/// only in 50..=1000. `WLR_HDR_MAX_NITS` is the display peak.
pub fn display_from_env() -> HdrOutput {
    display_from(
        std::env::var("WLR_SDR_NITS").ok().as_deref(),
        std::env::var("WLR_HDR_MAX_NITS").ok().as_deref(),
    )
}

fn display_from(sdr_white: Option<&str>, peak: Option<&str>) -> HdrOutput {
    let parse = |value: Option<&str>| value.and_then(|value| value.trim().parse::<f32>().ok());
    HdrOutput {
        sdr_white_nits: parse(sdr_white)
            .filter(|nits| (50.0..=1000.0).contains(nits))
            .unwrap_or(DEFAULT_SDR_WHITE_NITS),
        peak_nits: parse(peak)
            .filter(|nits| nits.is_finite() && *nits > 0.0 && *nits <= 10_000.0)
            .unwrap_or(DEFAULT_PEAK_NITS),
    }
}

/// The swapchain format to present PQ in, if the surface offers one.
///
/// 10-bit is preferred: PQ spends its code values where the eye can see
/// them, so 10 bits are enough and half the bandwidth of half-float.
pub(crate) fn pq_surface_format(caps: &wgpu::SurfaceCapabilities) -> Option<wgpu::TextureFormat> {
    [
        wgpu::TextureFormat::Rgb10a2Unorm,
        wgpu::TextureFormat::Rgba16Float,
    ]
    .into_iter()
    .find(|format| {
        caps.color_spaces(*format)
            .contains(wgpu::SurfaceColorSpaces::BT2100_PQ)
    })
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    sdr_white_nits: f32,
    peak_nits: f32,
    premultiplied: u32,
    _pad: u32,
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// The HDR scene target and the pass that encodes it into the swapchain.
pub(crate) struct HdrEncoder {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
    target: Option<Target>,
}

impl HdrEncoder {
    /// Build the encode pass for a swapchain of `surface_format`.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hdr encode layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<Params>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hdr encode shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hdr encode pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hdr encode"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hdr encode parameters"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            layout,
            pipeline,
            params,
            target: None,
        }
    }

    /// The scene target at `size`, allocated on first use and on resize.
    pub fn scene_view(&mut self, device: &wgpu::Device, size: [u32; 2]) -> &wgpu::TextureView {
        if self.target.as_ref().is_none_or(|target| {
            target.texture.width() != size[0] || target.texture.height() != size[1]
        }) {
            if let Some(target) = self.target.take() {
                target.texture.destroy();
            }
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("hdr scene"),
                size: wgpu::Extent3d {
                    width: size[0].max(1),
                    height: size[1].max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: SCENE_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            self.target = Some(Target { texture, view });
        }
        &self.target.as_ref().expect("allocated above").view
    }

    /// Encode the scene target into `swapchain`. Call after the frame has
    /// been drawn into [`Self::scene_view`].
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        swapchain: &wgpu::TextureView,
        display: HdrOutput,
        premultiplied: bool,
    ) {
        let Some(target) = &self.target else {
            return;
        };
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                sdr_white_nits: display.sdr_white_nits,
                peak_nits: display.peak_nits,
                premultiplied: premultiplied as u32,
                _pad: 0,
            }),
        );
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hdr encode bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&target.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hdr encode"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: swapchain,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..3, 0..1);
    }
}

const SHADER: &str = concat!(include_str!("srgb_extended.wgsl"), include_str!("hdr.wgsl"));

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use anyhow::Result;

    /// SMPTE ST 2084 inverse EOTF, the reference the shader is checked against.
    fn pq_encode(nits: f32) -> f32 {
        let (m1, m2) = (0.159_301_76_f32, 78.843_75_f32);
        let (c1, c2, c3) = (0.835_937_5_f32, 18.851_563_f32, 18.6875_f32);
        let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(m1);
        ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
    }

    fn srgb_to_linear_extended(encoded: f32) -> f32 {
        let magnitude = encoded.abs();
        let linear = if magnitude <= 0.04045 {
            magnitude / 12.92
        } else {
            ((magnitude + 0.055) / 1.055).powf(2.4)
        };
        linear.copysign(encoded)
    }

    fn linear_to_srgb_extended(linear: f32) -> f32 {
        let magnitude = linear.abs();
        let encoded = if magnitude <= 0.003_130_8 {
            magnitude * 12.92
        } else {
            1.055 * magnitude.powf(1.0 / 2.4) - 0.055
        };
        encoded.copysign(linear)
    }

    #[test]
    fn pq_encode_matches_st_2084() {
        assert_eq!(pq_encode(0.0), pq_encode(-5.0));
        assert!(pq_encode(0.0) < 1e-6, "PQ(0) = {}", pq_encode(0.0));
        assert!((pq_encode(10_000.0) - 1.0).abs() < 1e-6);
        assert!(
            (pq_encode(203.0) - 0.5807).abs() < 1e-4,
            "PQ(203) = {}",
            pq_encode(203.0)
        );
        assert!((pq_encode(100.0) - 0.5081).abs() < 1e-4);
    }

    #[test]
    fn extended_srgb_round_trips_past_one_and_below_zero() {
        assert_eq!(srgb_to_linear_extended(0.0), 0.0);
        assert!((srgb_to_linear_extended(1.0) - 1.0).abs() < 1e-6);
        assert!((srgb_to_linear_extended(0.5) - 0.214_041).abs() < 1e-5);
        assert!((srgb_to_linear_extended(-0.5) + 0.214_041).abs() < 1e-5);
        for linear in [-3.0, -0.002, 0.0, 0.002, 0.18, 1.0, 2.0, 5.0] {
            let back = srgb_to_linear_extended(linear_to_srgb_extended(linear));
            assert!((back - linear).abs() < 1e-5 * linear.abs().max(1.0));
        }
        assert!(linear_to_srgb_extended(2.0) > 1.0);
    }

    #[test]
    fn display_range_comes_from_the_wlroots_variables() {
        let display = display_from(Some("300"), Some("400"));
        assert_eq!(display.sdr_white_nits, 300.0);
        assert_eq!(display.peak_nits, 400.0);
        assert!((display.headroom() - 4.0 / 3.0).abs() < 1e-6);
        let unset = display_from(None, None);
        assert_eq!(unset.sdr_white_nits, 203.0);
        assert_eq!(unset.peak_nits, 1000.0);
        let invalid = display_from(Some("20"), Some("nits"));
        assert_eq!(invalid.sdr_white_nits, 203.0);
        assert_eq!(invalid.peak_nits, 1000.0);
        assert_eq!(display_from(Some("1001"), None).sdr_white_nits, 203.0);
        assert_eq!(display_from(Some(" 50 "), None).sdr_white_nits, 50.0);
        assert_eq!(display_from(Some("300"), Some("250")).headroom(), 1.0);
    }

    #[test]
    fn hdr_shaders_are_valid() -> Result<()> {
        let module = naga::front::wgsl::parse_str(SHADER)?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)?;
        Ok(())
    }

    /// The whole HDR chain on a real GPU, pixel by pixel:
    ///
    /// 0. a UI pixel at sRGB 1.0 is written to the scene target as is;
    /// 1. a viewport pixel of linear 2.0 is encoded to extended sRGB by the
    ///    shader function `fs_surface_linear` uses;
    /// 2. a viewport pixel far past the peak;
    /// 3. a UI pixel at sRGB 0.5.
    ///
    /// Then the encode pass writes a 10-bit PQ target, which is read back.
    #[test]
    fn ui_white_lands_at_sdr_white_and_viewport_highlights_above_it() -> Result<()> {
        let instance = wgpu::Instance::default();
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no GPU adapter; skipping");
            return Ok(());
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))?;
        let display = HdrOutput {
            sdr_white_nits: 300.0,
            peak_nits: 1000.0,
        };
        let swapchain_format = wgpu::TextureFormat::Rgb10a2Unorm;
        let mut hdr = HdrEncoder::new(&device, swapchain_format);
        let scene = hdr.scene_view(&device, [4, 1]).clone();

        // Stage 1: the linear viewport values, drawn through
        // `linear_to_srgb_extended` into the scene target, as a painted
        // `Rgba16Float` surface is.
        // Pixels 0 and 3 are UI pixels; the shader ignores their slots.
        let linear = [0.0_f32, 2.0, 50.0, 0.0];
        let surface_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("surface encode proof"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("srgb_extended.wgsl"),
                    r#"
@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}
@group(0) @binding(0) var<uniform> linear: vec4<f32>;
@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let value = linear[u32(position.x)];
    let ui = u32(position.x) == 0u || u32(position.x) == 3u;
    // UI pixels are already sRGB-encoded: 1.0 and 0.5.
    let encoded = select(linear_to_srgb_extended(vec3<f32>(value)), vec3<f32>(select(0.5, 1.0, u32(position.x) == 0u)), ui);
    return vec4<f32>(encoded, 1.0);
}
"#
                )
                .into(),
            ),
        });
        let uniform = wgpu::util::DeviceExt::create_buffer_init(
            &device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("surface values"),
                contents: bytemuck::cast_slice(&linear),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let surface_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("surface encode proof"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &surface_shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &surface_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(SCENE_FORMAT.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let surface_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &surface_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pq proof"),
            size: wgpu::Extent3d {
                width: 4,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: swapchain_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pq readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("surface encode proof"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scene,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&surface_pipeline);
            pass.set_bind_group(0, &surface_bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        hdr.encode(
            &device,
            &queue,
            &mut encoder,
            &output.create_view(&Default::default()),
            display,
            false,
        );
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            output.size(),
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).expect("readback receiver");
            });
        device.poll(wgpu::PollType::wait_indefinitely())?;
        receiver.recv()??;
        let bytes = readback.slice(..).get_mapped_range()?;
        let pixel = |index: usize| {
            let packed = u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
            [
                packed & 0x3ff,
                (packed >> 10) & 0x3ff,
                (packed >> 20) & 0x3ff,
                packed >> 30,
            ]
        };
        let code = |nits: f32| (pq_encode(nits) * 1023.0).round() as i64;
        let expect = |index: usize, nits: f32, what: &str| {
            let [r, g, b, a] = pixel(index);
            // A neutral grey is neutral in BT.2020 as well, so every channel
            // carries the same luminance.
            for channel in [r, g, b] {
                assert!(
                    (channel as i64 - code(nits)).abs() <= 1,
                    "{what}: PQ code {channel}, expected {} (PQ({nits} nits))",
                    code(nits)
                );
            }
            assert_eq!(a, 3, "{what}: opaque");
        };
        expect(0, display.sdr_white_nits, "UI white");
        expect(1, 2.0 * display.sdr_white_nits, "viewport at 2.0");
        expect(2, display.peak_nits, "viewport past the peak");
        expect(3, 0.214_041 * display.sdr_white_nits, "UI grey at sRGB 0.5");
        Ok(())
    }
}
