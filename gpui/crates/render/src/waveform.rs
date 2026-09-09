//! Compact GPU peak hierarchy and a shared renderer for waveform strips.
use crate::{
    device::DeviceContext,
    share::{Shared, Surface},
};
use std::sync::Arc;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    origin: u32,
    count: u32,
    atlas_width: u32,
    width: u32,
    fraction: f32,
    samples_per_pixel: f32,
    height: f32,
    padding: f32,
    gains: [f32; 4],
    ceilings: [f32; 4],
    colors: [[f32; 4]; 4],
    rows: [[u32; 4]; 32],
}

/// One strip's view, in physical pixels and audio seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Time at the left edge.
    pub start: f64,
    /// Audio duration represented by one physical pixel.
    pub seconds_per_pixel: f64,
    /// Physical target width.
    pub width: u32,
    /// Physical target height.
    pub height: u32,
    /// Combined top/bottom padding in physical pixels.
    pub padding: f32,
    /// Encoded sRGB: background, low, mid, high.
    pub colors: [[f32; 4]; 4],
}

/// Immutable track data and pipelines shared by timeline and minimap.
pub struct Waveform {
    context: Arc<DeviceContext>,
    atlas: wgpu::Texture,
    rows: [[u32; 4]; 32],
    count: u32,
    rate: f64,
    gains: [f32; 4],
    ceilings: [f32; 4],
    sample_pipeline: wgpu::ComputePipeline,
    draw_pipeline: wgpu::RenderPipeline,
}

/// A completed render target. It is immutable once published to the compositor.
pub struct Frame {
    shared: Option<Shared>,
    texture: Option<wgpu::Texture>,
    context: Arc<DeviceContext>,
    width: u32,
    height: u32,
}

impl Frame {
    /// Zero-copy presentation on supported compositors.
    pub fn surface(&self) -> Option<Surface> {
        self.shared.as_ref().map(Shared::surface)
    }

    /// Pixel capture for tests and platforms without surface sharing; not used
    /// by native Linux/macOS presentation. Returns BGRA sRGB bytes.
    pub fn pixels(&self) -> anyhow::Result<Vec<u8>> {
        if let Some(shared) = &self.shared {
            return Ok(shared.surface().to_bytes());
        }
        let texture = self.texture.as_ref().expect("frame owns a target");
        let stride = (self.width * 4).div_ceil(256) * 256;
        let buffer = self.context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("waveform-capture"),
            size: u64::from(stride) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        let submission = self.context.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.context.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })?;
        rx.recv()??;
        let mapped = buffer.slice(..).get_mapped_range()?;
        Ok(mapped
            .chunks(stride as usize)
            .flat_map(|row| row[..self.width as usize * 4].iter().copied())
            .collect())
    }
}

impl Waveform {
    /// Pack fixed dense peak bins into half floats, then build pairwise maxima on the GPU.
    /// Bins cover at most 1/6000 second, independent of view or zoom.
    /// Run off the UI thread: pipeline construction and upload are one-time work.
    pub fn new(
        context: Arc<DeviceContext>,
        bands: &[Vec<f32>; 3],
        gains: [f32; 3],
        ceilings: [f32; 3],
        rate: u32,
    ) -> anyhow::Result<Self> {
        let count = u32::try_from(bands[0].len())?;
        anyhow::ensure!(
            count > 0 && rate > 0 && bands.iter().all(|b| b.len() == count as usize),
            "invalid waveform signal"
        );
        anyhow::ensure!(
            gains.iter().all(|v| v.is_finite() && *v >= 0.),
            "invalid band gains"
        );
        // One-time packing also normalizes before half conversion, avoiding overflow
        // and preserving precision for quiet tracks. Navigation never touches PCM.
        let bin_size = (rate as usize / 6000).max(1);
        let count = count.div_ceil(bin_size as u32);
        let rate = f64::from(rate) / bin_size as f64;
        let device = &context.device;
        let max_dimension = device.limits().max_texture_dimension_2d;
        anyhow::ensure!(count < (1 << 31), "waveform has too many samples");
        let minimum_width =
            (u64::from(count) * 2).div_ceil(u64::from(max_dimension.saturating_sub(32).max(1)));
        let width = 4096
            .max(u32::try_from(minimum_width)?.next_power_of_two())
            .min(max_dimension)
            .min(count.next_power_of_two());
        let mut rows = [[0u32; 4]; 32];
        let mut height = 0;
        let mut length = count;
        let mut levels = 0;
        loop {
            rows[levels][0] = height;
            height += length.div_ceil(width);
            levels += 1;
            if length == 1 {
                break;
            }
            length = length.div_ceil(2);
        }
        anyhow::ensure!(
            height <= max_dimension,
            "track exceeds GPU waveform texture capacity ({height} rows, limit {max_dimension})"
        );
        let atlas = texture(
            device,
            "waveform-hierarchy",
            width,
            height,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        // Upload in rows instead of duplicating the entire track in an interleaved staging Vec.
        let mut packed = vec![[0u16; 4]; width as usize * 32];
        for start in (0..count as usize).step_by(packed.len()) {
            let n = packed.len().min(count as usize - start);
            packed.fill([0; 4]);
            for (i, value) in packed[..n].iter_mut().enumerate() {
                let first = (start + i) * bin_size;
                let last = (first + bin_size).min(bands[0].len());
                for band in 0..3 {
                    let peak = bands[band][first..last]
                        .iter()
                        .fold(0f32, |p, s| p.max(s.abs()));
                    let normalized = if gains[band] > 0. {
                        (peak / gains[band]).clamp(0., 1.)
                    } else {
                        0.
                    };
                    value[band] = half::f16::from_f32(normalized).to_bits();
                }
            }
            context.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: start as u32 / width,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&packed),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 8),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width,
                    height: (n as u32).div_ceil(width),
                    depth_or_array_layers: 1,
                },
            );
        }
        let reduce_shader =
            device.create_shader_module(wgpu::include_wgsl!("shaders/waveform_reduce.wgsl"));
        let reduce_layout = pipeline_layout(device, true);
        let reduce = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("waveform-reduce"),
            layout: Some(&reduce_layout),
            module: &reduce_shader,
            entry_point: Some("reduce"),
            compilation_options: Default::default(),
            cache: None,
        });
        let scratch = texture(
            device,
            "waveform-reduce-scratch",
            width,
            count.div_ceil(2).div_ceil(width),
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let source = atlas.create_view(&Default::default());
        let destination = scratch.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        length = count;
        for level in 1..levels {
            let output_count = length.div_ceil(2);
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("waveform-reduce-params"),
                contents: bytemuck::cast_slice(&[rows[level - 1][0], length, width, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &reduce.get_bind_group_layout(0),
                entries: &[
                    entry(0, wgpu::BindingResource::TextureView(&source)),
                    entry(1, wgpu::BindingResource::TextureView(&destination)),
                    entry(2, uniform.as_entire_binding()),
                ],
            });
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&reduce);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups(
                    width.div_ceil(16),
                    output_count.div_ceil(width).div_ceil(16),
                    1,
                );
            }
            encoder.copy_texture_to_texture(
                scratch.as_image_copy(),
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: rows[level][0],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width,
                    height: output_count.div_ceil(width),
                    depth_or_array_layers: 1,
                },
            );
            length = output_count;
        }
        context.queue.submit([encoder.finish()]);
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/waveform.wgsl"));
        let sample_layout = pipeline_layout(device, false);
        let sample_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("waveform-columns"),
            layout: Some(&sample_layout),
            module: &shader,
            entry_point: Some("sample_columns"),
            compilation_options: Default::default(),
            cache: None,
        });
        let draw_shader =
            device.create_shader_module(wgpu::include_wgsl!("shaders/waveform_draw.wgsl"));
        let draw_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("waveform-draw"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &draw_shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &draw_shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
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
        Ok(Self {
            context,
            atlas,
            rows,
            count,
            rate,
            gains: [1.; 4],
            ceilings: [ceilings[0], ceilings[1], ceilings[2], 0.],
            sample_pipeline,
            draw_pipeline,
        })
    }

    /// Resident hierarchy bytes, including row padding (excludes temporary build scratch).
    pub fn hierarchy_bytes(&self) -> u64 {
        u64::from(self.atlas.width()) * u64::from(self.atlas.height()) * 8
    }

    /// True after device loss; the caller should reload the track on a new context.
    pub fn is_lost(&self) -> bool {
        self.context.is_lost()
    }

    /// Render one physical-pixel strip. Queue completion is awaited here, so
    /// cross-device Metal surfaces are safe when published. Call on a worker.
    pub fn render(&self, view: View) -> anyhow::Result<Frame> {
        anyhow::ensure!(!self.is_lost(), "waveform GPU device lost");
        let device = &self.context.device;
        anyhow::ensure!(
            view.width > 0
                && view.height > 0
                && view.width <= device.limits().max_texture_dimension_2d
                && view.height <= device.limits().max_texture_dimension_2d,
            "invalid waveform dimensions"
        );
        anyhow::ensure!(
            view.start.is_finite()
                && view.start >= 0.
                && view.seconds_per_pixel.is_finite()
                && view.seconds_per_pixel > 0.,
            "invalid waveform range"
        );
        let start = (view.start * f64::from(self.rate)).min(f64::from(self.count));
        let params = Params {
            origin: start.floor() as u32,
            count: self.count,
            atlas_width: self.atlas.width(),
            width: view.width,
            fraction: start.fract() as f32,
            samples_per_pixel: (view.seconds_per_pixel * f64::from(self.rate)) as f32,
            height: view.height as f32,
            padding: view.padding,
            gains: self.gains,
            ceilings: self.ceilings,
            colors: view.colors,
            rows: self.rows,
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("waveform-view"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let heights = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("waveform-heights"),
            size: u64::from(view.width) * 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let atlas_view = self.atlas.create_view(&Default::default());
        let sample_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.sample_pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, wgpu::BindingResource::TextureView(&atlas_view)),
                entry(1, uniform.as_entire_binding()),
                entry(2, heights.as_entire_binding()),
            ],
        });
        let draw_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.draw_pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, uniform.as_entire_binding()),
                entry(1, heights.as_entire_binding()),
            ],
        });
        let shared = Shared::on(
            device,
            &self.context.queue,
            self.context.adopted,
            view.width,
            view.height,
        );
        let texture = shared.is_none().then(|| {
            texture(
                device,
                "waveform-output",
                view.width,
                view.height,
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            )
        });
        let fallback_view = texture.as_ref().map(|t| t.create_view(&Default::default()));
        let target = shared
            .as_ref()
            .map(Shared::view)
            .or(fallback_view.as_ref())
            .unwrap();
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.sample_pipeline);
            pass.set_bind_group(0, &sample_bind, &[]);
            pass.dispatch_workgroups(view.width.div_ceil(64), 1, 1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("waveform"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.draw_pipeline);
            pass.set_bind_group(0, &draw_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        let submission = self.context.queue.submit([encoder.finish()]);
        device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })?;
        Ok(Frame {
            shared,
            texture,
            context: self.context.clone(),
            width: view.width,
            height: view.height,
        })
    }
}

fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}
fn texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn pipeline_layout(device: &wgpu::Device, reduce: bool) -> wgpu::PipelineLayout {
    let uniform = wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("waveform-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: if reduce {
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    }
                } else {
                    uniform
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: if reduce {
                    uniform
                } else {
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    }
                },
                count: None,
            },
        ],
    });
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    })
}
