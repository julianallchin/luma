//! Sun shafts in outdoor stage haze (`shaders/sun_shafts.wgsl`).
//!
//! Outdoors the stage haze is lit by the sun in closed form, as if nothing
//! stood between the air and the sun. That is right for open air and wrong
//! under a roof or a cloud: the air in the roof's shadow glows as brightly
//! as the air beside it, and under overcast the haze still glows toward a
//! sun nobody can see. This pass marches each view ray at low resolution,
//! reads the stage's sun cascades and the cloud shadow map where the ray's
//! haze scatters, and leaves the fraction of that haze the sun reaches. The
//! composite scales the closed form's sun term by it (`composite.wgsl`).
//!
//! Cost: one texel per 2x2 pixels on High and per 4x4 on Low, a fixed
//! number of samples each, one shadow tap and one cloud-map tap per sample.

use glam::Mat4;

use crate::scene_desc::Quality;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    inv_view_proj: [[f32; 4]; 4],
    params: [f32; 4],
}

/// Pixels per shaft texel along each axis, and samples per ray.
fn budget(quality: Quality) -> (u32, u32) {
    match quality {
        Quality::Low => (4, 12),
        _ => (2, 24),
    }
}

pub(crate) struct Pipelines {
    scene: wgpu::BindGroupLayout,
    target: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    /// A 1x1 texel of full sun, bound when the pass does not run.
    pub(crate) off: wgpu::TextureView,
}

impl Pipelines {
    /// `source` is the scene shader's shared prelude followed by
    /// `sun_shafts.wgsl`: the pass reads the scene's own globals, cascades,
    /// haze field and cloud shadow, through the same declarations.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, source: &str) -> Self {
        let compute = wgpu::ShaderStages::COMPUTE;
        let mut scene_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: compute,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: compute,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: compute,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 10,
                visibility: compute,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 11,
                visibility: compute,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        scene_entries.extend(
            crate::atmosphere::AerialTextures::layout_entries().map(|entry| {
                wgpu::BindGroupLayoutEntry {
                    visibility: compute,
                    ..entry
                }
            }),
        );
        let scene = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sun-shafts-scene"),
            entries: &scene_entries,
        });
        let target = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sun-shafts-target"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: compute,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: compute,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: compute,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sun-shafts"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sun-shafts"),
            bind_group_layouts: &[Some(&scene), Some(&target)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sun-shafts"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let off = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sun-shafts-off"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let one = half::f16::from_f32(1.0).to_bits();
        let far = half::f16::from_f32(6e4).to_bits();
        queue.write_texture(
            off.as_image_copy(),
            bytemuck::cast_slice(&[one, far, 0, one]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        Self {
            scene,
            target,
            pipeline,
            off: off.create_view(&Default::default()),
        }
    }
}

/// What one frame's pass needs from the scene.
pub(crate) struct FrameInput<'a> {
    pub globals: &'a wgpu::Buffer,
    pub shadow_map: &'a wgpu::TextureView,
    pub shadow_sampler: &'a wgpu::Sampler,
    pub haze_field: (&'a wgpu::TextureView, &'a wgpu::Sampler),
    pub aerial: &'a crate::atmosphere::AerialTextures,
    pub depth: &'a wgpu::TextureView,
    pub inv_view_proj: Mat4,
    pub width: u32,
    pub height: u32,
    pub quality: Quality,
}

/// The shaft target, kept across frames at the size the frame needs.
#[derive(Default)]
pub(crate) struct Cache {
    target: Option<((u32, u32), wgpu::TextureView)>,
}

impl Cache {
    /// Encode the pass and return the texture the composite reads, with its
    /// size.
    pub(crate) fn encode(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        profile: &mut crate::pass_profile::PassQueries,
    ) -> (wgpu::TextureView, (u32, u32)) {
        let (divisor, samples) = budget(input.quality);
        let size = (
            input.width.div_ceil(divisor).max(1),
            input.height.div_ceil(divisor).max(1),
        );
        if self.target.as_ref().is_none_or(|(at, _)| *at != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("sun-shafts"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.target = Some((size, texture.create_view(&Default::default())));
        }
        let view = self.target.as_ref().expect("target allocated").1.clone();
        let uniform = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("sun-shafts"),
                contents: bytemuck::bytes_of(&Uniform {
                    inv_view_proj: input.inv_view_proj.to_cols_array_2d(),
                    params: [samples as f32, 0.0, 0.0, 0.0],
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input.globals.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(input.shadow_map),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(input.shadow_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 10,
                resource: wgpu::BindingResource::TextureView(input.haze_field.0),
            },
            wgpu::BindGroupEntry {
                binding: 11,
                resource: wgpu::BindingResource::Sampler(input.haze_field.1),
            },
        ];
        entries.extend(input.aerial.entries());
        let scene = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sun-shafts-scene"),
            layout: &pipelines.scene,
            entries: &entries,
        });
        let target = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sun-shafts-target"),
            layout: &pipelines.target,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input.depth),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("sun-shafts"),
            timestamp_writes: profile.compute("sun-shafts", None),
        });
        pass.set_pipeline(&pipelines.pipeline);
        pass.set_bind_group(0, &scene, &[]);
        pass.set_bind_group(1, &target, &[]);
        pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        drop(pass);
        (view, size)
    }
}
