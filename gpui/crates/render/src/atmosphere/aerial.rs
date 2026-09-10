//! Finite atmospheric paths, cached by sun and camera altitude.
//!
//! The atmosphere is horizontally uniform. An angular volume relative to the
//! sun therefore survives camera translation and rotation; only altitude and
//! the sun invalidate it. Its third axis is metric ray distance, not view Z.

use half::f16;

use super::{
    binding, buffer, compute, layout, prelude, sampled_2d, sampler_entry, storage_2d,
    uniform_entry, AtmospherePipelines, SkyFrame, SkyUniform, FORMAT, VIEW_HEIGHT_KM,
};

/// Outdoor clipping distance, also the last atmospheric integration slice.
pub(crate) const MAX_DISTANCE_M: f32 = 100_000.0;
const DISTANCE_SCALE_M: f32 = 100.0;
const SIZE: [u32; 3] = [64, 64, 64];

fn mapping_prelude() -> String {
    format!(
        "const AERIAL_MAX_M: f32 = {MAX_DISTANCE_M:?};\n\
         const AERIAL_SCALE_M: f32 = {DISTANCE_SCALE_M:?};\n{}",
        include_str!("../shaders/atmosphere_aerial_mapping.wgsl"),
    )
}

pub(crate) fn surface_prelude() -> String {
    format!(
        "{}{}",
        mapping_prelude(),
        include_str!("../shaders/atmosphere_aerial_sample.wgsl"),
    )
}

#[derive(Clone)]
pub(crate) struct Textures {
    pub radiance: wgpu::TextureView,
    pub transmittance: wgpu::TextureView,
    sky: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

impl Textures {
    pub(crate) fn layout_entries() -> [wgpu::BindGroupLayoutEntry; 4] {
        let sampled = |binding| wgpu::BindGroupLayoutEntry {
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            ..sampled_2d(binding)
        };
        [
            sampled(6),
            sampled(7),
            sampler_entry(8),
            uniform_entry(9, wgpu::ShaderStages::FRAGMENT),
        ]
    }

    pub(crate) fn entries(&self) -> [wgpu::BindGroupEntry<'_>; 4] {
        [
            binding(6, wgpu::BindingResource::TextureView(&self.radiance)),
            binding(7, wgpu::BindingResource::TextureView(&self.transmittance)),
            binding(8, wgpu::BindingResource::Sampler(&self.sampler)),
            binding(9, self.sky.as_entire_binding()),
        ]
    }
}

pub(crate) struct Pipelines {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    off: Textures,
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, sampler: &wgpu::Sampler) -> Self {
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: FORMAT,
                view_dimension: wgpu::TextureViewDimension::D3,
            },
            ..storage_2d(binding)
        };
        let layout = device.create_bind_group_layout(&layout(
            "atmosphere-aerial",
            &[
                sampled_2d(0),
                sampler_entry(1),
                sampled_2d(2),
                storage(3),
                storage(4),
                uniform_entry(5, wgpu::ShaderStages::COMPUTE),
            ],
        ));
        let pipeline = compute(
            device,
            "atmosphere-aerial",
            &layout,
            &format!(
                "{}{}{}{}",
                prelude(),
                include_str!("../shaders/atmosphere_common.wgsl"),
                mapping_prelude(),
                include_str!("../shaders/atmosphere_aerial.wgsl"),
            ),
        );
        let placeholder = |label, value: [f16; 4]| {
            let texture = texture(device, [1; 3], label);
            queue.write_texture(
                texture.as_image_copy(),
                bytemuck::cast_slice(&value.map(f16::to_bits)),
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
        };
        let off = Textures {
            radiance: placeholder("aerial-off-radiance", [f16::ZERO; 4]),
            transmittance: placeholder("aerial-off-transmittance", [f16::ONE; 4]),
            sky: buffer(device, SkyUniform::of(None), "aerial-off-sky"),
            sampler: sampler.clone(),
        };
        Self {
            layout,
            pipeline,
            off,
        }
    }
}

#[derive(Default)]
pub(crate) struct Cache {
    resident: Option<(SkyUniform, Textures)>,
}

impl Cache {
    pub(crate) fn prepare(
        &mut self,
        pipelines: &AtmospherePipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        sky: Option<&SkyFrame>,
        height_m: f32,
    ) -> Textures {
        let Some(sky) = sky else {
            return pipelines.aerial.off.clone();
        };
        let mut cfg = SkyUniform::of(Some(sky));
        // Clamp underground orbit views to the surface atmosphere. Do not
        // quantize altitude: that would make slow vertical motion step.
        cfg.params[2] = if height_m.is_finite() {
            (height_m * 0.001).max(0.001)
        } else {
            VIEW_HEIGHT_KM
        };
        if let Some((previous, textures)) = &self.resident {
            if *previous == cfg {
                return textures.clone();
            }
        }
        // Refill the resident volumes when the camera rises or the sun moves;
        // an orbit must not allocate another four megabytes every frame.
        let (radiance, transmittance) = self.resident.as_ref().map_or_else(
            || {
                (
                    texture(device, SIZE, "aerial-radiance")
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                    texture(device, SIZE, "aerial-transmittance")
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                )
            },
            |(_, textures)| (textures.radiance.clone(), textures.transmittance.clone()),
        );
        let sky = buffer(device, cfg, "aerial-sky");
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere-aerial"),
            layout: &pipelines.aerial.layout,
            entries: &[
                binding(
                    0,
                    wgpu::BindingResource::TextureView(&pipelines.transmittance),
                ),
                binding(1, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                binding(
                    2,
                    wgpu::BindingResource::TextureView(&pipelines.multiscatter),
                ),
                binding(3, wgpu::BindingResource::TextureView(&radiance)),
                binding(4, wgpu::BindingResource::TextureView(&transmittance)),
                binding(5, sky.as_entire_binding()),
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("atmosphere-aerial"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipelines.aerial.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(SIZE[0].div_ceil(8), SIZE[1].div_ceil(8), 1);
        }
        let textures = Textures {
            radiance,
            transmittance,
            sky,
            sampler: pipelines.sampler.clone(),
        };
        self.resident = Some((cfg, textures.clone()));
        textures
    }
}

fn texture(device: &wgpu::Device, size: [u32; 3], label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: size[2],
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests;
