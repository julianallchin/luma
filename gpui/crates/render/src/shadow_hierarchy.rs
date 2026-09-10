//! Conservative depth ranges retained alongside the fixture shadow maps.
use wgpu::util::DeviceExt;

pub(crate) struct Pipelines {
    layouts: [wgpu::BindGroupLayout; 2],
    pipelines: [wgpu::ComputePipeline; 2],
}
impl Pipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow-depth-ranges"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shadow_reduce.wgsl").into()),
        });
        let layouts = std::array::from_fn(|i| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shadow-depth-ranges"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: i as u32,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: if i == 0 {
                                wgpu::TextureSampleType::Depth
                            } else {
                                wgpu::TextureSampleType::Float { filterable: false }
                            },
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rg32Float,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            })
        });
        let pipelines = std::array::from_fn(|i| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("shadow-depth-ranges"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("shadow-depth-ranges"),
                        bind_group_layouts: &[Some(&layouts[i])],
                        immediate_size: 0,
                    }),
                ),
                module: &shader,
                entry_point: Some(if i == 0 { "from_depth" } else { "reduce" }),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        });
        Self { layouts, pipelines }
    }
}

pub(crate) struct Targets {
    pub views: [wgpu::TextureView; 2],
    levels: [Vec<wgpu::TextureView>; 2],
    size: u32,
}
impl Targets {
    pub fn new(device: &wgpu::Device, size: u32, capacity: usize) -> Self {
        let level_count = size.ilog2();
        let textures: [wgpu::Texture; 2] = std::array::from_fn(|bank| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fixture-shadow-depth-ranges"),
                size: wgpu::Extent3d {
                    width: size / 2,
                    height: size / 2,
                    depth_or_array_layers: if bank == 0 {
                        capacity.min(256) as u32
                    } else {
                        capacity.saturating_sub(256).max(1) as u32
                    },
                },
                mip_level_count: level_count,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rg32Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        });
        let views = std::array::from_fn(|bank| {
            textures[bank].create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            })
        });
        let levels = std::array::from_fn(|bank| {
            (0..level_count)
                .map(|mip| {
                    textures[bank].create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(wgpu::TextureViewDimension::D2Array),
                        base_mip_level: mip,
                        mip_level_count: Some(1),
                        ..Default::default()
                    })
                })
                .collect()
        });
        Self {
            views,
            levels,
            size,
        }
    }
    pub fn record(
        &self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        maps: [&wgpu::TextureView; 2],
        dirty: &[bool],
    ) {
        for (bank, map) in maps.iter().enumerate() {
            let layers: Vec<u32> = dirty
                .iter()
                .enumerate()
                .filter(|(i, d)| **d && *i / 256 == bank)
                .map(|(i, _)| (i % 256) as u32)
                .collect();
            if layers.is_empty() {
                continue;
            }
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dirty-shadow-depth-ranges"),
                contents: bytemuck::cast_slice(&layers),
                usage: wgpu::BufferUsages::STORAGE,
            });
            for (mip, output) in self.levels[bank].iter().enumerate() {
                let kind = usize::from(mip > 0);
                let input = if mip == 0 {
                    *map
                } else {
                    &self.levels[bank][mip - 1]
                };
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shadow-depth-ranges"),
                    layout: &pipelines.layouts[kind],
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: kind as u32,
                            resource: wgpu::BindingResource::TextureView(input),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(output),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: buffer.as_entire_binding(),
                        },
                    ],
                });
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("shadow-depth-ranges"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&pipelines.pipelines[kind]);
                pass.set_bind_group(0, &bind_group, &[]);
                let dimension = self.size >> (mip + 1);
                pass.dispatch_workgroups(
                    dimension.div_ceil(8),
                    dimension.div_ceil(8),
                    layers.len() as u32,
                );
            }
        }
    }
}
