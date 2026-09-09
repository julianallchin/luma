//! Within-window glass for the wgpu compositor. Ordinary frames still render
//! directly to the surface; only frames with backdrop operations need a
//! sampleable scene texture. Two reusable quarter-size targets carry the blur.
use bytemuck::{Pod, Zeroable};
use gpui::{BackdropBlur, PrimitiveBatch, Scene};
use wgpu::util::DeviceExt;

const IDLE_FRAMES: u32 = 60;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    bounds: [f32; 4],
    clip: [f32; 4],
    radii: [f32; 4],
    source: [f32; 4],
    kernel: [f32; 4],
}

impl Params {
    fn new(blur: &BackdropBlur, size: [u32; 2]) -> Option<Self> {
        let sigma = blur.blur_radius.0;
        if !sigma.is_finite() || sigma <= 0.0 || blur.opacity <= 0.0 {
            return None;
        }
        let visible = blur.bounds.intersect(&blur.content_mask.bounds);
        let clip = [
            visible.origin.x.0.max(0.0),
            visible.origin.y.0.max(0.0),
            visible.right().0.min(size[0] as f32),
            visible.bottom().0.min(size[1] as f32),
        ];
        if clip[2] <= clip[0] || clip[3] <= clip[1] {
            return None;
        }
        let scale = if sigma >= 8.0 { 4.0 } else { 1.0 };
        let padding = (sigma * 3.0).ceil() + scale;
        let x = (clip[0] - padding).floor().max(0.0);
        let y = (clip[1] - padding).floor().max(0.0);
        let width = (clip[2] + padding).ceil().min(size[0] as f32) - x;
        let height = (clip[3] + padding).ceil().min(size[1] as f32) - y;
        Some(Self {
            bounds: [
                blur.bounds.origin.x.0,
                blur.bounds.origin.y.0,
                blur.bounds.size.width.0,
                blur.bounds.size.height.0,
            ],
            clip: [clip[0], clip[1], clip[2] - clip[0], clip[3] - clip[1]],
            radii: [
                blur.corner_radii.top_left.0,
                blur.corner_radii.top_right.0,
                blur.corner_radii.bottom_right.0,
                blur.corner_radii.bottom_left.0,
            ],
            source: [x, y, width, height],
            kernel: [
                (width / scale).ceil(),
                (height / scale).ceil(),
                scale,
                sigma,
            ],
        })
    }
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    fn new(device: &wgpu::Device, size: [u32; 2], format: wgpu::TextureFormat) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop target"),
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
        let view = texture.create_view(&Default::default());
        Self { texture, view }
    }
}

pub(crate) struct Backdrop {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    downsample: wgpu::RenderPipeline,
    horizontal: wgpu::RenderPipeline,
    vertical: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    present: wgpu::RenderPipeline,
    frame: Option<Target>,
    scratch: Option<[Target; 2]>,
    idle_frames: u32,
}

impl Backdrop {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
            label: Some("backdrop shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("backdrop.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("backdrop pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: (entry == "composite").then_some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::Constant,
                                dst_factor: wgpu::BlendFactor::OneMinusConstant,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::Constant,
                                dst_factor: wgpu::BlendFactor::OneMinusConstant,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            downsample: pipeline("downsample"),
            horizontal: pipeline("horizontal"),
            vertical: pipeline("vertical"),
            composite: pipeline("composite"),
            present: pipeline("present"),
            layout,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("backdrop sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            frame: None,
            scratch: None,
            idle_frames: 0,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        size: [u32; 2],
        format: wgpu::TextureFormat,
        blurs: &[BackdropBlur],
    ) {
        if blurs.is_empty() {
            self.idle_frames = (self.idle_frames + 1).min(IDLE_FRAMES);
            if self.idle_frames == IDLE_FRAMES {
                self.frame = None;
                self.scratch = None;
            }
            return;
        }
        self.idle_frames = 0;
        if self.frame.as_ref().is_none_or(|target| {
            target.texture.width() != size[0] || target.texture.height() != size[1]
        }) {
            self.frame = Some(Target::new(device, size, format));
            self.scratch = None;
        }
        let mut needed = [1, 1];
        for params in blurs.iter().filter_map(|blur| Params::new(blur, size)) {
            needed[0] = needed[0].max(params.kernel[0] as u32);
            needed[1] = needed[1].max(params.kernel[1] as u32);
        }
        if let Some(targets) = &self.scratch {
            needed[0] = needed[0].max(targets[0].texture.width());
            needed[1] = needed[1].max(targets[0].texture.height());
        }
        if self.scratch.as_ref().is_none_or(|targets| {
            targets[0].texture.width() < needed[0] || targets[0].texture.height() < needed[1]
        }) {
            // Quantization avoids reallocating on every tick of an expanding dialog.
            let extent = [
                needed[0].next_multiple_of(64).min(size[0]),
                needed[1].next_multiple_of(64).min(size[1]),
            ];
            self.scratch = Some([
                Target::new(device, extent, format),
                Target::new(device, extent, format),
            ]);
        }
    }

    pub fn frame_view(&self) -> Option<&wgpu::TextureView> {
        self.frame.as_ref().map(|target| &target.view)
    }

    fn bind(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        uniform: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }

    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        blur: &BackdropBlur,
    ) {
        let (Some(frame), Some(scratch)) = (&self.frame, &self.scratch) else {
            return;
        };
        let Some(params) = Params::new(blur, [frame.texture.width(), frame.texture.height()])
        else {
            return;
        };
        // Each operation owns its parameters: queue.write_buffer on one shared
        // uniform would make every blur in a submitted frame use the last one.
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("backdrop parameters"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_binding = self.bind(device, &frame.view, &uniform);
        let first = self.bind(device, &scratch[0].view, &uniform);
        let second = self.bind(device, &scratch[1].view, &uniform);
        let extent = Some([0.0, 0.0, params.kernel[0], params.kernel[1]]);
        draw(
            encoder,
            &scratch[0].view,
            &self.downsample,
            &frame_binding,
            extent,
            None,
        );
        draw(
            encoder,
            &scratch[1].view,
            &self.horizontal,
            &first,
            extent,
            None,
        );
        draw(
            encoder,
            &scratch[0].view,
            &self.vertical,
            &second,
            extent,
            None,
        );
        draw(
            encoder,
            &frame.view,
            &self.composite,
            &first,
            Some(params.clip),
            Some(blur.opacity),
        );
    }

    pub fn present(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
    ) {
        let Some(frame) = &self.frame else {
            return;
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("backdrop present parameters"),
            contents: bytemuck::bytes_of(&Params::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        draw(
            encoder,
            target,
            &self.present,
            &self.bind(device, &frame.view, &uniform),
            None,
            None,
        );
    }
}

fn draw(
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bindings: &wgpu::BindGroup,
    extent: Option<[f32; 4]>,
    opacity: Option<f32>,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("backdrop pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: if opacity.is_none() {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    if let Some([x, y, width, height]) = extent {
        pass.set_viewport(x, y, width, height, 0.0, 1.0);
    }
    if let Some(opacity) = opacity {
        let alpha = opacity.clamp(0.0, 1.0) as f64;
        pass.set_blend_constant(wgpu::Color {
            r: alpha,
            g: alpha,
            b: alpha,
            a: alpha,
        });
    }
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bindings, &[]);
    pass.draw(0..3, 0..1);
}

pub(crate) fn batch_first_order(scene: &Scene, batch: &PrimitiveBatch) -> u32 {
    match batch {
        PrimitiveBatch::Shadows(range) => scene.shadows[range.start].order,
        PrimitiveBatch::Quads(range) => scene.quads[range.start].order,
        PrimitiveBatch::Paths(range) => scene.paths[range.start].order,
        PrimitiveBatch::Underlines(range) => scene.underlines[range.start].order,
        PrimitiveBatch::MonochromeSprites { range, .. } => {
            scene.monochrome_sprites[range.start].order
        }
        PrimitiveBatch::SubpixelSprites { range, .. } => scene.subpixel_sprites[range.start].order,
        PrimitiveBatch::PolychromeSprites { range, .. } => {
            scene.polychrome_sprites[range.start].order
        }
        PrimitiveBatch::Surfaces(range) => scene.surfaces[range.start].order,
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use gpui::{Bounds, ContentMask, Corners, ScaledPixels, point, size};

    fn blur(rect: [f32; 4], sigma: f32) -> BackdropBlur {
        BackdropBlur {
            order: 0,
            opacity: 1.0,
            blur_radius: ScaledPixels(sigma),
            bounds: Bounds::new(
                point(ScaledPixels(rect[0]), ScaledPixels(rect[1])),
                size(ScaledPixels(rect[2]), ScaledPixels(rect[3])),
            ),
            content_mask: ContentMask {
                bounds: Bounds::new(
                    point(ScaledPixels(0.0), ScaledPixels(0.0)),
                    size(ScaledPixels(256.0), ScaledPixels(192.0)),
                ),
            },
            corner_radii: Corners::all(ScaledPixels(24.0)),
        }
    }

    #[test]
    fn backdrop_shader_is_valid() -> Result<()> {
        let module = naga::front::wgsl::parse_str(include_str!("backdrop.wgsl"))?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)?;
        Ok(())
    }

    #[test]
    fn backdrop_bounds_handle_offscreen_and_identity_operations() {
        assert!(Params::new(&blur([300.0, 20.0, 20.0, 20.0], 12.0), [256, 192]).is_none());
        assert!(Params::new(&blur([0.0, 0.0, 20.0, 20.0], 0.0), [256, 192]).is_none());
        assert!(Params::new(&blur([0.0, 0.0, 20.0, 20.0], f32::NAN), [256, 192]).is_none());
        let params = Params::new(&blur([-20.0, -10.0, 60.0, 40.0], 2.0), [256, 192])
            .expect("visible corner");
        assert_eq!(params.clip, [0.0, 0.0, 40.0, 30.0]);
        assert_eq!(params.source, [0.0, 0.0, 47.0, 37.0]);
        assert_eq!(params.kernel[2], 1.0);
    }

    #[test]
    fn backdrop_gpu_blurs_only_the_clipped_rounded_regions_and_reuses_targets() -> Result<()> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
        eprintln!("backdrop GPU: {:?}", adapter.get_info());
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))?;
        device.set_device_lost_callback(|reason, message| {
            eprintln!("backdrop test device lost: {reason:?}: {message}");
        });
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let mut backdrop = Backdrop::new(&device, format);
        let mut first = blur([16.0, 16.0, 224.0, 160.0], 12.0);
        first.content_mask.bounds.size.width = ScaledPixels(176.0);
        let second = blur([192.0, 48.0, 64.0, 96.0], 3.0);
        backdrop.prepare(&device, [256, 192], format, &[first, second]);
        let frame_id = backdrop.frame.as_ref().expect("frame").texture.clone();
        let scratch_id = backdrop.scratch.as_ref().expect("scratch")[0]
            .texture
            .clone();
        backdrop.prepare(&device, [256, 192], format, &[first, second]);
        assert_eq!(frame_id, backdrop.frame.as_ref().expect("frame").texture);
        assert_eq!(
            scratch_id,
            backdrop.scratch.as_ref().expect("scratch")[0].texture
        );

        // Add upload/readback usages only to the test framebuffer. Production
        // uses render/sample bindings and never copies the window to the CPU.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop proof"),
            size: wgpu::Extent3d {
                width: 256,
                height: 192,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        backdrop.frame = Some(Target { texture, view });
        let original: Vec<u8> = (0..192)
            .flat_map(|y| {
                (0..256).flat_map(move |x| {
                    let value = if (x / 8 + y / 8) % 2 == 0 { 0 } else { 255 };
                    [value, value, value, 255]
                })
            })
            .collect();
        let frame = &backdrop.frame.as_ref().expect("frame").texture;
        queue.write_texture(
            frame.as_image_copy(),
            &original,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(1024),
                rows_per_image: None,
            },
            frame.size(),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        backdrop.encode(&device, &mut encoder, &first);
        backdrop.encode(&device, &mut encoder, &second);
        // A zero-radius operation must preserve previous pixels exactly.
        backdrop.encode(&device, &mut encoder, &blur([0.0, 0.0, 256.0, 192.0], 0.0));
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop presentation proof"),
            size: frame.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        backdrop.present(
            &device,
            &mut encoder,
            &output.create_view(&Default::default()),
        );
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("backdrop readback"),
            size: original.len() as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: None,
                },
            },
            frame.size(),
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
        let pixels = readback.slice(..).get_mapped_range()?;
        let pixel = |x: usize, y: usize| pixels[(y * 256 + x) * 4];
        for y in 56..136 {
            for x in 56..136 {
                assert!(
                    (120..=136).contains(&pixel(x, y)),
                    "unfiltered center ({x}, {y}): {}",
                    pixel(x, y)
                );
            }
        }
        for (x, y) in [(0, 0), (17, 17), (182, 90), (255, 191)] {
            let offset = (y * 256 + x) * 4;
            assert_eq!(
                &pixels[offset..offset + 4],
                &original[offset..offset + 4],
                "blur escaped its clip at ({x}, {y})"
            );
        }
        assert!(
            (20..235).contains(&pixel(220, 96)),
            "second blur did not use its own parameters"
        );
        assert!(pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
        if let Ok(path) = std::env::var("LUMA_BACKDROP_CAPTURE") {
            use std::io::Write;
            let mut file = std::fs::File::create(&path).context("create backdrop capture")?;
            write!(file, "P6\n256 192\n255\n")?;
            for pixel in pixels.chunks_exact(4) {
                file.write_all(&pixel[..3])?;
            }
        }
        drop(pixels);
        readback.unmap();

        // Fading glass mixes the same blurred image with the sharp backdrop;
        // it must not substitute a smaller blur radius or linger at full opacity.
        for opacity in [0.0, 0.5, 1.0] {
            queue.write_texture(
                frame.as_image_copy(),
                &original,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: None,
                },
                frame.size(),
            );
            let mut faded = first;
            faded.opacity = opacity;
            let mut encoder = device.create_command_encoder(&Default::default());
            backdrop.encode(&device, &mut encoder, &faded);
            backdrop.present(
                &device,
                &mut encoder,
                &output.create_view(&Default::default()),
            );
            encoder.copy_texture_to_buffer(
                output.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(1024),
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
                    sender.send(result).expect("fade readback receiver");
                });
            device.poll(wgpu::PollType::wait_indefinitely())?;
            receiver.recv()??;
            let pixels = readback.slice(..).get_mapped_range()?;
            let center = pixels[(64 * 256 + 64) * 4] as f32;
            assert!(
                (center - 128.0 * opacity).abs() <= 2.0,
                "glass opacity {opacity}: expected {}, got {center}",
                128.0 * opacity
            );
            drop(pixels);
            readback.unmap();
        }

        // Exercise presentation at the user's 1440p viewport, including the
        // normal 44px card sigma. Set LUMA_BACKDROP_PROFILE=1 to time warm frames.
        let mut display_blur = blur([830.0, 370.0, 900.0, 700.0], 44.0);
        display_blur.content_mask.bounds.size = size(ScaledPixels(2560.0), ScaledPixels(1440.0));
        backdrop.prepare(&device, [2560, 1440], format, &[display_blur]);
        let display = Target::new(&device, [2560, 1440], format);
        let profile = std::env::var_os("LUMA_BACKDROP_PROFILE").is_some();
        let mut times = Vec::new();
        for index in 0..if profile { 35 } else { 1 } {
            let start = std::time::Instant::now();
            let mut encoder = device.create_command_encoder(&Default::default());
            backdrop.encode(&device, &mut encoder, &display_blur);
            backdrop.present(&device, &mut encoder, &display.view);
            queue.submit([encoder.finish()]);
            device.poll(wgpu::PollType::wait_indefinitely())?;
            if index >= 5 {
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        if profile {
            times.sort_by(f64::total_cmp);
            eprintln!(
                "1440p backdrop + presentation, CPU encode through GPU completion: median {:.2}ms, p95 {:.2}ms",
                times[15], times[28]
            );
        }
        backdrop.prepare(&device, [128, 96], format, &[first]);
        assert_eq!(
            backdrop
                .frame
                .as_ref()
                .expect("resized frame")
                .texture
                .width(),
            128
        );
        for _ in 0..IDLE_FRAMES {
            backdrop.prepare(&device, [128, 96], format, &[]);
        }
        assert!(
            backdrop.frame.is_none() && backdrop.scratch.is_none(),
            "idle glass retained scratch textures"
        );
        Ok(())
    }
}
