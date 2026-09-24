//! The cloud layer on the GPU: the noise it is made of, the per-frame trace
//! from the camera, the shadow map, and the probe's panorama.
//!
//! See `clouds.rs` for what the layer is and `atmosphere_cloud_common.wgsl`
//! for how it is marched.

use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use super::clouds::{self, Budget, Layer, BLUE_NOISE_SIZE, SHADOW_BANDS, SHADOW_SPAN_KM};
use super::{
    binding, buffer, compute, layout, prelude, sampled_2d, sampler_entry, storage_2d,
    uniform_entry, SkyFrame, FORMAT,
};
use crate::scene_desc::{CloudCover, Quality};

const SHAPE_SIZE: u32 = 128;
const DETAIL_SIZE: u32 = 32;
/// Weight of a new trace against a pixel's reprojected history. Each pixel
/// is traced every fourth frame, so this is about a dozen frames of memory.
const HISTORY_BLEND: f32 = 0.35;

pub(crate) struct Pipelines {
    panorama_layout: wgpu::BindGroupLayout,
    panorama: wgpu::ComputePipeline,
    view_layout: wgpu::BindGroupLayout,
    view: wgpu::ComputePipeline,
    shadow_layout: wgpu::BindGroupLayout,
    shadow: wgpu::ComputePipeline,
    shape: wgpu::TextureView,
    detail: wgpu::TextureView,
    blue_noise: wgpu::TextureView,
    wrap_sampler: wgpu::Sampler,
}

fn sampled_3d(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
        },
        ..sampled_2d(binding)
    }
}

fn storage(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        ..storage_2d(binding)
    }
}

/// Bindings 0 to 8, which every cloud pass shares
/// (`atmosphere_cloud_common.wgsl`).
fn common_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        sampled_2d(0),
        sampler_entry(1),
        sampled_2d(2),
        sampled_2d(3),
        sampler_entry(4),
        sampled_3d(5),
        sampled_3d(6),
        uniform_entry(7, wgpu::ShaderStages::COMPUTE),
        uniform_entry(8, wgpu::ShaderStages::COMPUTE),
        sampled_2d(15),
    ]
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let source = |entry: &str| {
            format!(
                "{}{}{}{entry}",
                prelude(),
                include_str!("../shaders/atmosphere_common.wgsl"),
                include_str!("../shaders/atmosphere_cloud_common.wgsl"),
            )
        };
        let with = |extra: Vec<wgpu::BindGroupLayoutEntry>| {
            let mut entries = common_entries();
            entries.extend(extra);
            entries
        };

        let panorama_layout = device.create_bind_group_layout(&layout(
            "atmosphere-clouds-panorama",
            &with(vec![storage(9, FORMAT)]),
        ));
        let panorama = compute(
            device,
            "atmosphere-clouds-panorama",
            &panorama_layout,
            &source(include_str!("../shaders/atmosphere_clouds.wgsl")),
        );
        let unfiltered = wgpu::BindGroupLayoutEntry {
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            ..sampled_2d(11)
        };
        let view_layout = device.create_bind_group_layout(&layout(
            "atmosphere-clouds-view",
            &with(vec![
                uniform_entry(9, wgpu::ShaderStages::COMPUTE),
                sampled_2d(10),
                unfiltered,
                storage(12, FORMAT),
                storage(13, wgpu::TextureFormat::R32Float),
                sampled_2d(14),
                sampled_2d(16),
            ]),
        ));
        let view = compute(
            device,
            "atmosphere-clouds-view",
            &view_layout,
            &source(&format!(
                "const AERIAL_MAX_KM: f32 = {:?};\n{}{}",
                super::MAX_DISTANCE_M * 0.001,
                include_str!("../shaders/cloud_shadow.wgsl"),
                include_str!("../shaders/atmosphere_cloud_view.wgsl")
            )),
        );
        let shadow_layout = device.create_bind_group_layout(&layout(
            "atmosphere-clouds-shadow",
            &with(vec![
                storage(9, FORMAT),
                uniform_entry(10, wgpu::ShaderStages::COMPUTE),
            ]),
        ));
        let shadow = compute(
            device,
            "atmosphere-clouds-shadow",
            &shadow_layout,
            &source(include_str!("../shaders/atmosphere_cloud_shadow.wgsl")),
        );

        let wrap_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atmosphere-clouds-wrap"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let (shape, detail) = bake_noise(device, queue);
        let blue_noise = device
            .create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("atmosphere-clouds-blue-noise"),
                    size: wgpu::Extent3d {
                        width: BLUE_NOISE_SIZE,
                        height: BLUE_NOISE_SIZE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                clouds::blue_noise(),
            )
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            panorama_layout,
            panorama,
            view_layout,
            view,
            shadow_layout,
            shadow,
            shape,
            detail,
            blue_noise,
            wrap_sampler,
        }
    }
}

/// Bake the shape and detail volumes (`atmosphere_cloud_noise.wgsl`).
fn bake_noise(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::TextureView, wgpu::TextureView) {
    let layout = device.create_bind_group_layout(&layout(
        "atmosphere-clouds-noise",
        &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D3,
                },
                count: None,
            },
            uniform_entry(1, wgpu::ShaderStages::COMPUTE),
        ],
    ));
    let pipeline = compute(
        device,
        "atmosphere-clouds-noise",
        &layout,
        include_str!("../shaders/atmosphere_cloud_noise.wgsl"),
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("atmosphere-clouds-noise"),
    });
    let mut volume = |kind: u32, size: u32, label: &str| {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: size,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &[
                binding(0, wgpu::BindingResource::TextureView(&view)),
                binding(
                    1,
                    buffer(device, [kind, 0x9e37_79b9_u32 + kind, 0, 0], label).as_entire_binding(),
                ),
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        let groups = size.div_ceil(4);
        pass.dispatch_workgroups(groups, groups, groups);
        drop(pass);
        view
    };
    let shape = volume(0, SHAPE_SIZE, "atmosphere-clouds-shape");
    let detail = volume(1, DETAIL_SIZE, "atmosphere-clouds-detail");
    queue.submit(Some(encoder.finish()));
    (shape, detail)
}

/// `CloudLayer` in `atmosphere_cloud_common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    shape: [f32; 4],
    detail: [f32; 4],
    march: [f32; 4],
    wind: [f32; 4],
    light: [f32; 4],
    cirrus: [f32; 4],
    cirrus_shape: [f32; 4],
}

impl LayerUniform {
    fn of(layer: &Layer, budget: Budget, sun: Vec3, time: f32) -> Self {
        // Kilometres the wind has carried the layer. The shape and detail
        // volumes drift a little faster than the weather, so clouds change
        // shape as they go rather than sliding past as stamps.
        let drift = layer.wind_mps * 0.001 * time.max(0.0);
        Self {
            shape: [
                layer.base_km,
                layer.thickness_km,
                layer.extinction,
                layer.tile_km,
            ],
            detail: [
                layer.erosion,
                layer.shape_km,
                layer.detail_km,
                layer.clear_fraction(),
            ],
            march: [
                budget.steps as f32,
                budget.light_steps as f32,
                layer.light_km(sun.z),
                layer.ambient,
            ],
            wind: [-drift, 0.0, -drift * 0.25, -drift * 0.6],
            light: [layer.powder, layer.sharpness, 0.0, 0.0],
            cirrus: [
                layer.cirrus.altitude_km,
                layer.cirrus.optical_depth,
                layer.cirrus.coverage,
                layer.cirrus.streak_km,
            ],
            // The wind high up is about twice the wind at the cumulus.
            cirrus_shape: [layer.cirrus.width_km, -2.0 * drift, 0.0, 0.0],
        }
    }
}

/// `CloudView` in `atmosphere_cloud_view.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewUniform {
    inv_view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    camera: [f32; 4],
    frame: [f32; 4],
}

/// The camera and clock a frame's clouds are traced for.
pub(crate) struct FrameInput {
    /// World from clip, unjittered, metres.
    pub view_proj: Mat4,
    /// World position of the camera, metres.
    pub eye: Vec3,
    /// Output size, pixels.
    pub width: u32,
    pub height: u32,
    /// Seconds, for the wind.
    pub time: f32,
    /// Whether this is a live frame whose successor may reuse its history.
    pub temporal: bool,
    pub quality: Quality,
}

/// What a frame's clouds left for the passes after them.
pub(crate) struct Traced {
    /// The layer as the camera sees it, over the whole output.
    pub view: wgpu::TextureView,
    /// Sun transmittance through the layer over the ground round the camera.
    pub shadow: wgpu::TextureView,
    /// `SkyUniform::shadow`: the map's centre and side, km, and the sun it
    /// lets through on average, for past its edge.
    pub shadow_params: [f32; 4],
}

struct Targets {
    size: (u32, u32),
    color: wgpu::Texture,
    depth: wgpu::Texture,
    history_color: wgpu::Texture,
    history_depth: wgpu::Texture,
    bind: wgpu::BindGroup,
    /// What the bind group was made for: the sky table and the shadow map
    /// it reads.
    skyview: wgpu::TextureView,
    shadow: wgpu::TextureView,
}

struct ShadowMap {
    size: u32,
    view: wgpu::TextureView,
    /// Centre, km, the map was last drawn round.
    centre: [f32; 2],
    bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
}

/// The history key: a change in any of these makes last frame's clouds a
/// different sky.
#[derive(Clone, Copy, PartialEq)]
struct Key {
    sun: [u32; 3],
    cover: CloudCover,
    quality: Quality,
    size: (u32, u32),
}

#[derive(Default)]
pub(crate) struct Cache {
    weather: Option<(CloudCover, wgpu::TextureView)>,
    layer_uniform: Option<wgpu::Buffer>,
    view_uniform: Option<wgpu::Buffer>,
    targets: Option<Targets>,
    shadow: Option<ShadowMap>,
    frame: u32,
    /// Last frame's camera and key, when it left a history.
    previous: Option<(Mat4, Key)>,
}

impl Cache {
    fn weather(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cover: CloudCover,
    ) -> Option<wgpu::TextureView> {
        let map = clouds::weather(cover)?;
        if let Some((resident, view)) = &self.weather {
            if *resident == cover {
                return Some(view.clone());
            }
        }
        let size = clouds::WEATHER_SIZE;
        let view = device
            .create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("atmosphere-weather"),
                    size: wgpu::Extent3d {
                        width: size,
                        height: size,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &map.texels,
            )
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.weather = Some((cover, view.clone()));
        Some(view)
    }

    fn layer_uniform(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        value: LayerUniform,
    ) -> wgpu::Buffer {
        let buffer = self.layer_uniform.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("atmosphere-cloud-layer"),
                size: std::mem::size_of::<LayerUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        queue.write_buffer(buffer, 0, bytemuck::bytes_of(&value));
        buffer.clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn common<'a>(
        pipelines: &'a Pipelines,
        tables: &'a super::Tables<'a>,
        weather: &'a wgpu::TextureView,
        sky: &'a wgpu::Buffer,
        layer: &'a wgpu::Buffer,
    ) -> Vec<wgpu::BindGroupEntry<'a>> {
        vec![
            binding(0, wgpu::BindingResource::TextureView(tables.transmittance)),
            binding(1, wgpu::BindingResource::Sampler(tables.sampler)),
            binding(2, wgpu::BindingResource::TextureView(tables.skyview)),
            binding(3, wgpu::BindingResource::TextureView(weather)),
            binding(4, wgpu::BindingResource::Sampler(&pipelines.wrap_sampler)),
            binding(5, wgpu::BindingResource::TextureView(&pipelines.shape)),
            binding(6, wgpu::BindingResource::TextureView(&pipelines.detail)),
            binding(7, sky.as_entire_binding()),
            binding(8, layer.as_entire_binding()),
            binding(15, wgpu::BindingResource::TextureView(tables.multiscatter)),
        ]
    }

    /// March the layer into the probe's panorama, from the venue, at time
    /// zero's wind: the probe is low frequency and follows the sun and the
    /// weather, not the drift. `None` for a clear sky.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn panorama(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        sky: &SkyFrame,
        tables: &super::Tables<'_>,
        sky_uniform: &wgpu::Buffer,
        quality: Quality,
    ) -> Option<wgpu::TextureView> {
        let layer = Layer::of(sky.clouds)?;
        let weather = self.weather(device, queue, sky.clouds)?;
        let budget = Budget::of(quality);
        let uniform = buffer(
            device,
            LayerUniform::of(&layer, budget, sky.sun_direction, 0.0),
            "atmosphere-cloud-panorama-layer",
        );
        let (width, height) = budget.panorama;
        let panorama =
            super::storage_texture(device, width, height, 1, "atmosphere-clouds-panorama")
                .create_view(&wgpu::TextureViewDescriptor::default());
        let mut entries = Self::common(pipelines, tables, &weather, sky_uniform, &uniform);
        entries.push(binding(9, wgpu::BindingResource::TextureView(&panorama)));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere-clouds-panorama"),
            layout: &pipelines.panorama_layout,
            entries: &entries,
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("atmosphere-clouds-panorama"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipelines.panorama);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        Some(panorama)
    }

    /// Trace this frame's clouds and bring the shadow map up to date.
    /// `None` for a clear sky, which forgets any history.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn trace(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        sky: &SkyFrame,
        tables: &super::Tables<'_>,
        sky_uniform: &wgpu::Buffer,
        input: &FrameInput,
        profile: &mut crate::pass_profile::PassQueries<'_>,
    ) -> Option<Traced> {
        let Some(layer) = Layer::of(sky.clouds) else {
            self.previous = None;
            return None;
        };
        let weather = self.weather(device, queue, sky.clouds)?;
        let budget = Budget::of(input.quality);
        let layer_buffer = self.layer_uniform(
            device,
            queue,
            LayerUniform::of(&layer, budget, sky.sun_direction, input.time),
        );
        let eye_km = input.eye * 0.001;

        // --- shadow map --------------------------------------------------
        // Centred on the camera, snapped to a kilometre so a walk round the
        // venue does not redraw it every frame.
        let centre = [eye_km.x.round(), eye_km.y.round()];
        let refresh_all = self
            .shadow
            .as_ref()
            .is_none_or(|s| s.size != budget.shadow || s.centre != centre)
            || self.previous.is_none_or(|(_, key)| {
                key.sun != sky.sun_direction.to_array().map(f32::to_bits) || key.cover != sky.clouds
            })
            || !input.temporal;
        if self.shadow.as_ref().is_none_or(|s| s.size != budget.shadow) {
            let view = super::storage_texture(
                device,
                budget.shadow,
                budget.shadow,
                1,
                "atmosphere-cloud-shadow",
            )
            .create_view(&wgpu::TextureViewDescriptor::default());
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("atmosphere-cloud-shadow"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut entries = Self::common(pipelines, tables, &weather, sky_uniform, &layer_buffer);
            entries.push(binding(9, wgpu::BindingResource::TextureView(&view)));
            entries.push(binding(10, uniform.as_entire_binding()));
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("atmosphere-cloud-shadow"),
                layout: &pipelines.shadow_layout,
                entries: &entries,
            });
            self.shadow = Some(ShadowMap {
                size: budget.shadow,
                view,
                centre,
                bind,
                uniform,
            });
        }
        let shadow = self.shadow.as_mut().expect("shadow map allocated");
        // The bind group holds the weather and tables it was made with.
        if refresh_all {
            let mut entries = Self::common(pipelines, tables, &weather, sky_uniform, &layer_buffer);
            entries.push(binding(9, wgpu::BindingResource::TextureView(&shadow.view)));
            entries.push(binding(10, shadow.uniform.as_entire_binding()));
            shadow.bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("atmosphere-cloud-shadow"),
                layout: &pipelines.shadow_layout,
                entries: &entries,
            });
        }
        shadow.centre = centre;
        let (first, stride) = if refresh_all {
            (0, 1)
        } else {
            (self.frame % SHADOW_BANDS, SHADOW_BANDS)
        };
        let map = [centre[0], centre[1], SHADOW_SPAN_KM, 0.0];
        let rows = [first, stride, 0, 0];
        let mut bytes = bytemuck::bytes_of(&map).to_vec();
        bytes.extend_from_slice(bytemuck::bytes_of(&rows));
        queue.write_buffer(&shadow.uniform, 0, &bytes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("atmosphere-cloud-shadow"),
                timestamp_writes: profile.compute("atmosphere-cloud-shadow", None),
            });
            pass.set_pipeline(&pipelines.shadow);
            pass.set_bind_group(0, &shadow.bind, &[]);
            pass.dispatch_workgroups(
                budget.shadow.div_ceil(8),
                budget.shadow.div_ceil(stride).div_ceil(8),
                1,
            );
        }
        let shadow_view = shadow.view.clone();

        // --- view trace --------------------------------------------------
        let size = (
            input.width.div_ceil(budget.divisor).max(1),
            input.height.div_ceil(budget.divisor).max(1),
        );
        let key = Key {
            sun: sky.sun_direction.to_array().map(f32::to_bits),
            cover: sky.clouds,
            quality: input.quality,
            size,
        };
        let view_uniform = self
            .view_uniform
            .get_or_insert_with(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("atmosphere-cloud-view"),
                    size: std::mem::size_of::<ViewUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .clone();
        let rebuild = self.targets.as_ref().is_none_or(|t| {
            t.size != size || t.skyview != *tables.skyview || t.shadow != shadow_view
        });
        if rebuild {
            let texture = |format, label| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };
            let color = texture(FORMAT, "atmosphere-cloud-view");
            let depth = texture(wgpu::TextureFormat::R32Float, "atmosphere-cloud-view-depth");
            let history_color = texture(FORMAT, "atmosphere-cloud-history");
            let history_depth = texture(
                wgpu::TextureFormat::R32Float,
                "atmosphere-cloud-history-depth",
            );
            let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
            let (color_view, depth_view) = (view(&color), view(&depth));
            let (history_color_view, history_depth_view) =
                (view(&history_color), view(&history_depth));
            let mut entries = Self::common(pipelines, tables, &weather, sky_uniform, &layer_buffer);
            entries.extend([
                binding(9, view_uniform.as_entire_binding()),
                binding(10, wgpu::BindingResource::TextureView(&history_color_view)),
                binding(11, wgpu::BindingResource::TextureView(&history_depth_view)),
                binding(12, wgpu::BindingResource::TextureView(&color_view)),
                binding(13, wgpu::BindingResource::TextureView(&depth_view)),
                binding(
                    14,
                    wgpu::BindingResource::TextureView(&pipelines.blue_noise),
                ),
                binding(16, wgpu::BindingResource::TextureView(&shadow_view)),
            ]);
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("atmosphere-cloud-view"),
                layout: &pipelines.view_layout,
                entries: &entries,
            });
            self.targets = Some(Targets {
                size,
                color,
                depth,
                history_color,
                history_depth,
                bind,
                shadow: shadow_view.clone(),
                skyview: tables.skyview.clone(),
            });
            self.previous = None;
        }
        // A new preset is a new sky table (`AtmosphereCache::prepare` keys on
        // it), so the rebuild above has already rebound the new weather.
        let history = input.temporal && self.previous.is_some_and(|(_, previous)| previous == key);
        let prev_view_proj = self
            .previous
            .map_or(input.view_proj, |(view_proj, _)| view_proj);
        queue.write_buffer(
            &view_uniform,
            0,
            bytemuck::bytes_of(&ViewUniform {
                inv_view_proj: input.view_proj.inverse().to_cols_array_2d(),
                prev_view_proj: prev_view_proj.to_cols_array_2d(),
                camera: eye_km.extend(0.0).to_array(),
                frame: [
                    (self.frame % 1024) as f32,
                    f32::from(u8::from(!history)),
                    f32::from(u8::from(history)),
                    HISTORY_BLEND,
                ],
            }),
        );
        let targets = self.targets.as_ref().expect("targets allocated");
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("atmosphere-cloud-view"),
                timestamp_writes: profile.compute("atmosphere-cloud-view", None),
            });
            pass.set_pipeline(&pipelines.view);
            pass.set_bind_group(0, &targets.bind, &[]);
            pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        }
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        encoder.copy_texture_to_texture(
            targets.color.as_image_copy(),
            targets.history_color.as_image_copy(),
            extent,
        );
        encoder.copy_texture_to_texture(
            targets.depth.as_image_copy(),
            targets.history_depth.as_image_copy(),
            extent,
        );
        self.previous = input.temporal.then_some((input.view_proj, key));
        self.frame = self.frame.wrapping_add(1);
        Some(Traced {
            view: targets
                .color
                .create_view(&wgpu::TextureViewDescriptor::default()),
            shadow: shadow_view,
            shadow_params: [centre[0], centre[1], SHADOW_SPAN_KM, layer.clear_fraction()],
        })
    }
}
