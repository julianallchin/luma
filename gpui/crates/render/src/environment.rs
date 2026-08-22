//! Resident HDR environment preprocessing and scene bindings.

use bytemuck::{Pod, Zeroable};
use half::f16;
use wgpu::util::DeviceExt;

use crate::frame::EnvironmentImage;

const CUBE_SIZE: u32 = 128;
const IRRADIANCE_SIZE: u32 = 16;
const SPECULAR_MIPS: u32 = 8;
const BRDF_SIZE: u32 = 128;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FilterParams {
    size: u32,
    mode: u32,
    roughness: f32,
    source_size: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SizeParams {
    size: u32,
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneParams {
    intensity: f32,
    rotation: f32,
    enabled: f32,
    visible: f32,
}

struct ResidentEnvironment {
    key: String,
    irradiance: wgpu::TextureView,
    specular: wgpu::TextureView,
}

/// Owns immutable preprocessing pipelines and the currently resident probe.
pub(crate) struct EnvironmentSystem {
    scene_layout: wgpu::BindGroupLayout,
    equirect_layout: wgpu::BindGroupLayout,
    filter_layout: wgpu::BindGroupLayout,
    equirect_pipeline: wgpu::ComputePipeline,
    filter_pipeline: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    fallback_cube: wgpu::TextureView,
    fallback_lut: wgpu::TextureView,
    brdf: Option<wgpu::TextureView>,
    resident: Option<ResidentEnvironment>,
}

impl EnvironmentSystem {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("environment-scene"),
            entries: &[
                sampled_cube(0),
                sampled_cube(1),
                sampled_2d(2),
                sampler_entry(3),
                uniform_entry(4),
            ],
        });
        let equirect_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("environment-equirect"),
            entries: &[
                sampled_2d(0),
                sampler_entry(1),
                storage_array(2),
                uniform_entry(3),
            ],
        });
        let filter_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("environment-filter"),
            entries: &[
                sampled_cube(0),
                sampler_entry(1),
                storage_array(2),
                uniform_entry(3),
            ],
        });
        let equirect_pipeline = compute_pipeline(
            device,
            "environment-equirect",
            &equirect_layout,
            include_str!("shaders/environment_equirect.wgsl"),
        );
        let filter_pipeline = compute_pipeline(
            device,
            "environment-filter",
            &filter_layout,
            include_str!("shaders/environment_filter.wgsl"),
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("environment"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let fallback_cube = black_cube(device, queue);
        let fallback_lut = black_2d(device, queue);
        Self {
            scene_layout,
            equirect_layout,
            filter_layout,
            equirect_pipeline,
            filter_pipeline,
            sampler,
            fallback_cube,
            fallback_lut,
            brdf: None,
            resident: None,
        }
    }

    pub(crate) fn scene_layout(&self) -> &wgpu::BindGroupLayout {
        &self.scene_layout
    }

    /// Ensure the frame's immutable probe is resident. Returns true on upload.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        environment: Option<&EnvironmentImage>,
    ) -> bool {
        let Some(environment) = environment else {
            return false;
        };
        if self
            .resident
            .as_ref()
            .is_some_and(|resident| resident.key == environment.key)
        {
            return false;
        }
        if self.brdf.is_none() {
            self.brdf = Some(brdf_lut(device, queue));
        }

        let source = upload_hdr(device, queue, environment);
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let raw_cube = cube_texture(device, CUBE_SIZE, 1, "environment-raw-cube");
        let raw_sample = raw_cube.create_view(&wgpu::TextureViewDescriptor {
            label: Some("environment-raw-cube"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let raw_target = storage_mip(&raw_cube, 0, "environment-raw-target");
        let specular_texture =
            cube_texture(device, CUBE_SIZE, SPECULAR_MIPS, "environment-specular");
        let specular = specular_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("environment-specular"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let irradiance_texture = cube_texture(device, IRRADIANCE_SIZE, 1, "environment-irradiance");
        let irradiance = irradiance_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("environment-irradiance"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let irradiance_target =
            storage_mip(&irradiance_texture, 0, "environment-irradiance-target");

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("environment-preprocess"),
        });
        dispatch_equirect(
            device,
            &mut encoder,
            (&self.equirect_layout, &self.equirect_pipeline),
            &source_view,
            &self.sampler,
            &raw_target,
            CUBE_SIZE,
        );
        dispatch_filter(
            device,
            &mut encoder,
            (&self.filter_layout, &self.filter_pipeline),
            &raw_sample,
            &self.sampler,
            &irradiance_target,
            FilterParams {
                size: IRRADIANCE_SIZE,
                mode: 0,
                roughness: 1.0,
                source_size: CUBE_SIZE as f32,
            },
        );
        for mip in 0..SPECULAR_MIPS {
            let size = (CUBE_SIZE >> mip).max(1);
            let target = storage_mip(&specular_texture, mip, "environment-specular-mip");
            dispatch_filter(
                device,
                &mut encoder,
                (&self.filter_layout, &self.filter_pipeline),
                &raw_sample,
                &self.sampler,
                &target,
                FilterParams {
                    size,
                    mode: 1,
                    roughness: mip as f32 / (SPECULAR_MIPS - 1) as f32,
                    source_size: CUBE_SIZE as f32,
                },
            );
        }
        queue.submit(Some(encoder.finish()));
        self.resident = Some(ResidentEnvironment {
            key: environment.key.clone(),
            irradiance,
            specular,
        });
        true
    }

    pub(crate) fn bind_group(
        &self,
        device: &wgpu::Device,
        environment: Option<&EnvironmentImage>,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let enabled = environment.is_some() && self.resident.is_some();
        let params = environment.map_or(
            SceneParams {
                intensity: 0.0,
                rotation: 0.0,
                enabled: 0.0,
                visible: 0.0,
            },
            |environment| SceneParams {
                intensity: environment.intensity,
                rotation: environment.rotation,
                enabled: f32::from(u8::from(enabled)),
                visible: f32::from(u8::from(enabled && environment.visible)),
            },
        );
        let uniform = buffer(
            device,
            &[params],
            wgpu::BufferUsages::UNIFORM,
            "environment-scene",
        );
        let (irradiance, specular) = self
            .resident
            .as_ref()
            .map_or((&self.fallback_cube, &self.fallback_cube), |resident| {
                (&resident.irradiance, &resident.specular)
            });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("environment-scene"),
            layout: &self.scene_layout,
            entries: &[
                binding(0, wgpu::BindingResource::TextureView(irradiance)),
                binding(1, wgpu::BindingResource::TextureView(specular)),
                binding(
                    2,
                    wgpu::BindingResource::TextureView(
                        self.brdf.as_ref().unwrap_or(&self.fallback_lut),
                    ),
                ),
                binding(3, wgpu::BindingResource::Sampler(&self.sampler)),
                binding(4, uniform.as_entire_binding()),
            ],
        });
        (uniform, bind_group)
    }
}

fn dispatch_equirect(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: (&wgpu::BindGroupLayout, &wgpu::ComputePipeline),
    source: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    target: &wgpu::TextureView,
    size: u32,
) {
    let params = buffer(
        device,
        &[SizeParams { size, _pad: [0; 3] }],
        wgpu::BufferUsages::UNIFORM,
        "environment-size",
    );
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("environment-equirect"),
        layout: pipeline.0,
        entries: &[
            binding(0, wgpu::BindingResource::TextureView(source)),
            binding(1, wgpu::BindingResource::Sampler(sampler)),
            binding(2, wgpu::BindingResource::TextureView(target)),
            binding(3, params.as_entire_binding()),
        ],
    });
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
    pass.set_pipeline(pipeline.1);
    pass.set_bind_group(0, &bg, &[]);
    pass.dispatch_workgroups(size.div_ceil(8), size.div_ceil(8), 6);
}

fn dispatch_filter(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: (&wgpu::BindGroupLayout, &wgpu::ComputePipeline),
    source: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    target: &wgpu::TextureView,
    params_value: FilterParams,
) {
    let params = buffer(
        device,
        &[params_value],
        wgpu::BufferUsages::UNIFORM,
        "environment-filter",
    );
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("environment-filter"),
        layout: pipeline.0,
        entries: &[
            binding(0, wgpu::BindingResource::TextureView(source)),
            binding(1, wgpu::BindingResource::Sampler(sampler)),
            binding(2, wgpu::BindingResource::TextureView(target)),
            binding(3, params.as_entire_binding()),
        ],
    });
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
    pass.set_pipeline(pipeline.1);
    pass.set_bind_group(0, &bg, &[]);
    pass.dispatch_workgroups(
        params_value.size.div_ceil(8),
        params_value.size.div_ceil(8),
        6,
    );
}

fn brdf_lut(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("environment-brdf"),
        size: wgpu::Extent3d {
            width: BRDF_SIZE,
            height: BRDF_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("environment-brdf"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba16Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            uniform_entry(1),
        ],
    });
    let pipeline = compute_pipeline(
        device,
        "environment-brdf",
        &layout,
        include_str!("shaders/environment_brdf.wgsl"),
    );
    let params = buffer(
        device,
        &[SizeParams {
            size: BRDF_SIZE,
            _pad: [0; 3],
        }],
        wgpu::BufferUsages::UNIFORM,
        "environment-brdf",
    );
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("environment-brdf"),
        layout: &layout,
        entries: &[
            binding(0, wgpu::BindingResource::TextureView(&view)),
            binding(1, params.as_entire_binding()),
        ],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("environment-brdf"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(BRDF_SIZE.div_ceil(8), BRDF_SIZE.div_ceil(8), 1);
    }
    queue.submit(Some(encoder.finish()));
    view
}

fn upload_hdr(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    environment: &EnvironmentImage,
) -> wgpu::Texture {
    let image = &environment.image;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("environment-source"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let row = (image.width * 8).div_ceil(256) * 256;
    let mut pixels = vec![0_u8; (row * image.height) as usize];
    for y in 0..image.height as usize {
        for x in 0..image.width as usize {
            let src = (y * image.width as usize + x) * 3;
            let dst = y * row as usize + x * 8;
            let rgba = [
                f16::from_f32(image.rgb[src]),
                f16::from_f32(image.rgb[src + 1]),
                f16::from_f32(image.rgb[src + 2]),
                f16::ONE,
            ];
            pixels[dst..dst + 8].copy_from_slice(bytemuck::cast_slice(&rgba));
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row),
            rows_per_image: Some(image.height),
        },
        wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn cube_texture(device: &wgpu::Device, size: u32, mips: u32, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn storage_mip(texture: &wgpu::Texture, mip: u32, label: &str) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(label),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        base_mip_level: mip,
        mip_level_count: Some(1),
        base_array_layer: 0,
        array_layer_count: Some(6),
        ..Default::default()
    })
}

fn black_cube(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("environment-fallback"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 6,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0; 48],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 6,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

fn black_2d(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("environment-lut-fallback"),
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
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0; 8],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    source: &str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

fn buffer<T: Pod>(
    device: &wgpu::Device,
    values: &[T],
    usage: wgpu::BufferUsages,
    label: &str,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(values),
        usage,
    })
}

fn sampled_cube(binding: u32) -> wgpu::BindGroupLayoutEntry {
    sampled(binding, wgpu::TextureViewDimension::Cube)
}
fn sampled_2d(binding: u32) -> wgpu::BindGroupLayoutEntry {
    sampled(binding, wgpu::TextureViewDimension::D2)
}
fn sampled(binding: u32, view_dimension: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension,
            multisampled: false,
        },
        count: None,
    }
}
fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}
fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn storage_array(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: FORMAT,
            view_dimension: wgpu::TextureViewDimension::D2Array,
        },
        count: None,
    }
}
fn binding(index: u32, resource: wgpu::BindingResource) -> wgpu::BindGroupEntry {
    wgpu::BindGroupEntry {
        binding: index,
        resource,
    }
}
