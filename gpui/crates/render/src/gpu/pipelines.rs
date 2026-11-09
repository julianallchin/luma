//! Every bind-group layout and pipeline the pass chain uses, compiled once
//! per device in [`Gpu::on`].

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use crate::atmosphere::AtmospherePipelines;
use crate::environment::EnvironmentPipelines;
use crate::haze_field::HazeField;
use crate::light_index::LightIndexPipelines;
use crate::overlay::{Overlay, OverlayDepth};
use luma_scene::mesh::Image;

use super::descriptors::{
    depth_array_entry, depth_state, rw_storage_entry, storage_entry, texture_entry, uniform_entry,
};
use super::materials::{
    material_bind_group, upload_texture_view, MaterialDefaults, TextureEncoding,
};
use super::scene_pipelines::{mesh_vertex_layout, ScenePipelineSource};
use super::stats::RendererProfile;
use super::targets::shadow_texture_array;
use super::wgsl::{beam_transport_wgsl, composite_wgsl, scene_bindings_wgsl, scene_wgsl, shader};
use super::{Channels, Gpu, MSAA_SAMPLES, SCENE_FORMAT};

impl Gpu {
    /// Every pipeline, on a device somebody has already made.
    pub(super) fn on(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_profile: RendererProfile,
        lost: Arc<AtomicBool>,
        adopted: bool,
        started: Instant,
    ) -> Self {
        let environment = EnvironmentPipelines::new(&device, &queue);
        let atmosphere = AtmospherePipelines::new(&device, &queue);
        let mut scene_entries = vec![
            uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
            storage_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ];
        scene_entries.extend(crate::atmosphere::AerialTextures::layout_entries());
        scene_entries.extend([
            wgpu::BindGroupLayoutEntry {
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                ..texture_entry(10)
            },
            wgpu::BindGroupLayoutEntry {
                binding: 11,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ]);
        // The probe relight (`probes.rs`) lights its texels with the scene
        // pass's own functions, in compute, so groups 0 and 3 are visible there.
        for entry in &mut scene_entries {
            entry.visibility |= wgpu::ShaderStages::COMPUTE;
        }
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &scene_entries,
        });
        // Bindings 2 and 3 held the CSR cluster lists before the unified
        // light index (8–10) replaced them; the numbers stay reserved so the
        // surviving slots keep their shader-side ids. (A profiler counter
        // briefly lived at 2 as a read-write binding — bound across the hot
        // passes it serialised them on Metal, ~20× wall per frame at high
        // draw counts. Profiler accumulation is a separate compute pass now;
        // never bind a read-write buffer here.)
        let mut cluster_entries = vec![
            storage_entry(0, wgpu::ShaderStages::FRAGMENT),
            storage_entry(1, wgpu::ShaderStages::FRAGMENT),
            uniform_entry(4, wgpu::ShaderStages::FRAGMENT),
            storage_entry(5, wgpu::ShaderStages::FRAGMENT),
            depth_array_entry(6, wgpu::ShaderStages::FRAGMENT),
            wgpu::BindGroupLayoutEntry {
                binding: 7,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            uniform_entry(8, wgpu::ShaderStages::FRAGMENT),
            storage_entry(9, wgpu::ShaderStages::FRAGMENT),
            storage_entry(10, wgpu::ShaderStages::FRAGMENT),
            storage_entry(11, wgpu::ShaderStages::FRAGMENT),
            depth_array_entry(12, wgpu::ShaderStages::FRAGMENT),
            // Per-tile surface depth split of the light index (binding 14
            // is declared alongside 13 in `scene_bindings.wgsl`).
            // The far-field fog prefix, for grid-sourced surface transmittance.
            wgpu::BindGroupLayoutEntry {
                binding: 13,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            storage_entry(14, wgpu::ShaderStages::FRAGMENT),
            // Per-pixel ambient visibility (`sky_visibility.rs`).
            wgpu::BindGroupLayoutEntry {
                binding: 15,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            // The sun-shaft fraction (`sun_shafts.rs`).
            wgpu::BindGroupLayoutEntry {
                binding: 16,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ];
        // The haze's glow (`haze_glow.rs`): 17 to 19.
        cluster_entries.extend(crate::haze_glow::layout_entries(
            17,
            wgpu::ShaderStages::FRAGMENT,
        ));
        for entry in &mut cluster_entries {
            entry.visibility |= wgpu::ShaderStages::COMPUTE;
        }
        let cluster_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("surface-clusters"),
            entries: &cluster_entries,
        });

        // Group 1 is the per-draw material texture. glTF's `baseColorTexture`
        // is sRGB-encoded, so the view format decodes on sample and the shader
        // multiplies in linear space, as three does.
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let haze_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze"),
            entries: &[
                uniform_entry(
                    0,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                storage_entry(
                    1,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                storage_entry(
                    2,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // 4 and 5 held the per-pass haze tile list before the unified
                // light index (group 1) replaced it; they now carry the baked
                // density field (`haze_field.rs`), which is what the volumetric
                // integrand's `haze_noise` reads.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage_entry(
                    6,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                depth_array_entry(
                    7,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                storage_entry(
                    8,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                depth_array_entry(
                    9,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 12,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let haze_field = HazeField::bake(&device, &queue);

        // The unified light index: build pipelines plus the consumer layout
        // the haze pipeline binds as group 1.
        let light_index_pipelines = LightIndexPipelines::new(&device);

        let glow_entries = crate::haze_glow::layout_entries(8, wgpu::ShaderStages::FRAGMENT);
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // The sun-shaft fraction (`sun_shafts.rs`).
                texture_entry(7),
                // The haze's glow (`haze_glow.rs`).
                glow_entries[0],
                glow_entries[1],
                glow_entries[2],
            ],
        });

        // Both scene-geometry shaders open with the shared bind-group
        // declarations; see `scene_bindings.wgsl`.
        let bindings = scene_bindings_wgsl();
        let sun_shafts = crate::sun_shafts::Pipelines::new(
            &device,
            &queue,
            &format!("{bindings}{}", include_str!("../shaders/sun_shafts.wgsl")),
        );
        let laser = crate::laser::Pipelines::new(&device, SCENE_FORMAT);
        let haze_glow = crate::haze_glow::Pipelines::new(&device);
        let fixture_light = include_str!("../shaders/fixture_light.wgsl");
        let visibility = include_str!("../shaders/visibility.wgsl");
        let haze_visibility = visibility.replace("@group(3) @binding(11)", "@group(0) @binding(8)");
        // The light-index prelude is authored against group 1 (the haze
        // pass's slot); the surface pass carries the same bindings inside its
        // group 3, so its copy is rebound by this one documented replace.
        let light_index_prelude = crate::light_index::prelude();
        let scene_wgsl = scene_wgsl();
        let scene_module = shader(&device, "scene", &scene_wgsl);
        // The transport (ray reconstruction + per-light integral + group-0
        // layout) is one file both volumetric passes prepend, so they cannot
        // draw two different beams.
        let beam_transport = beam_transport_wgsl();
        // The density field's dimensions are compile-time properties of
        // `haze_field`, so they arrive as injected constants rather than as
        // uniform members nobody could see drift.
        let haze_field_prelude = crate::haze_field::prelude();
        let haze_base_text = format!(
            "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}{}",
            include_str!("../shaders/haze.wgsl"),
            include_str!("../shaders/haze_cache.wgsl"),
        );
        let haze_module = shader(&device, "haze", &haze_base_text);
        let medium_cache_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("medium-cache"),
                entries: &[
                    uniform_entry(0, wgpu::ShaderStages::COMPUTE),
                    storage_entry(1, wgpu::ShaderStages::COMPUTE),
                    storage_entry(2, wgpu::ShaderStages::COMPUTE),
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D3,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba16Float,
                            view_dimension: wgpu::TextureViewDimension::D3,
                        },
                        count: None,
                    },
                ],
            });
        let medium_cache_module = shader(
            &device,
            "medium-cache",
            &format!(
                "{}{}{}",
                crate::haze_field::prelude(),
                include_str!("../shaders/medium.wgsl"),
                include_str!("../shaders/medium_cache.wgsl")
            ),
        );
        let medium_cache_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("medium-cache"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("medium-cache"),
                        bind_group_layouts: &[Some(&medium_cache_layout)],
                        immediate_size: 0,
                    }),
                ),
                module: &medium_cache_module,
                entry_point: Some("build_cache"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let composite_module = shader(&device, "composite", &composite_wgsl());
        let grid_module = shader(
            &device,
            "grid",
            &format!("{bindings}{}", include_str!("../shaders/grid.wgsl")),
        );
        let cable_module = shader(
            &device,
            "cables",
            &format!("{bindings}{}", include_str!("../shaders/cables.wgsl")),
        );

        let scene_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("scene"),
                bind_group_layouts: &[
                    Some(&scene_layout),
                    Some(&material_layout),
                    Some(environment.scene_layout()),
                    Some(&cluster_layout),
                ],
                immediate_size: 0,
            });

        let vertex_layout = mesh_vertex_layout();

        let probes = crate::probes::Pipelines::new(
            &device,
            &crate::probes::PipelineSources {
                material_layout: &material_layout,
                scene_layout: &scene_layout,
                environment_layout: environment.scene_layout(),
                cluster_layout: &cluster_layout,
                vertex_layout: mesh_vertex_layout(),
                scene_source: &scene_wgsl,
            },
        );

        let scene_source = ScenePipelineSource {
            layout: scene_pipeline_layout.clone(),
            scene: scene_module.clone(),
            grid: grid_module.clone(),
            cables: cable_module.clone(),
        };
        let scene_pipelines = scene_source.build(&device, MSAA_SAMPLES);

        let depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth-prepass"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_main"),
                buffers: &[Some(vertex_layout.clone())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // The fixture shadow pass binds only what its vertex stage reads:
        // per-map globals, the instance table, and the mesh-bucketed caster
        // index list. Borrowing the full scene layout meant one material
        // bind per caster the pass never sampled.
        let fixture_shadow_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fixture-shadow"),
                entries: &[
                    uniform_entry(0, wgpu::ShaderStages::VERTEX),
                    storage_entry(1, wgpu::ShaderStages::VERTEX),
                    storage_entry(5, wgpu::ShaderStages::VERTEX),
                ],
            });
        let fixture_shadow_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fixture-shadow"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("fixture-shadow"),
                        bind_group_layouts: &[Some(&fixture_shadow_layout)],
                        immediate_size: 0,
                    }),
                ),
                vertex: wgpu::VertexState {
                    module: &scene_module,
                    entry_point: Some("vs_fixture_shadow"),
                    buffers: &[Some(vertex_layout.clone())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: None,
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(depth_state(true)),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_depth"),
                buffers: &[Some(vertex_layout)],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let fog_column_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let fog_prepare_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-prepare"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                }],
            });
        let fog_prepare_module = shader(
            &device,
            "fog-prepare",
            &format!(
                "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}",
                include_str!("../shaders/haze_prepare.wgsl")
            ),
        );
        let fog_prepare_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fog-prepare"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("fog-prepare"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&fog_prepare_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &fog_prepare_module,
                entry_point: Some("prepare_columns"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let fog_classify_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-classify"),
                entries: &[
                    fog_column_entry(0),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let fog_classify_module = shader(
            &device,
            "fog-classify",
            &format!(
                "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}",
                include_str!("../shaders/haze_classify.wgsl")
            ),
        );
        let fog_classify_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fog-classify"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("fog-classify"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&fog_classify_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &fog_classify_module,
                entry_point: Some("classify_blocks"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let fog_grid_write_entries = [
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: SCENE_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D3,
                },
                count: None,
            },
            fog_column_entry(1),
            storage_entry(2, wgpu::ShaderStages::COMPUTE),
        ];
        let fog_grid_write_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-grid-write"),
                entries: &fog_grid_write_entries,
            });
        let fog_integrate_entries = |tau: Option<wgpu::BindingType>| {
            let mut entries = vec![
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: SCENE_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D3,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                fog_column_entry(2),
            ];
            entries.extend(tau.map(|ty| wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty,
                count: None,
            }));
            entries
        };
        let fog_integrate_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-integrate"),
                entries: &fog_integrate_entries(None),
            });
        // `fog-transmittance` records per-slice τ; the reusing `fog-integrate`
        // reads it back (see `FOG_TAU_MODE` in `haze_integrate.wgsl`).
        let fog_tau_write_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-transmittance"),
                entries: &fog_integrate_entries(Some(wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::R32Float,
                    view_dimension: wgpu::TextureViewDimension::D3,
                })),
            });
        let fog_tau_read_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-integrate-reuse"),
                entries: &fog_integrate_entries(Some(wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                })),
            });
        let fog_integrate_module = |mode: u32| {
            let tau = match mode {
                0 => {
                    "fn fog_tau_read(column: vec2<u32>, slice: u32) -> f32 { return 0.0; }\n\
                      fn fog_tau_write(column: vec2<u32>, slice: u32, value: f32) {}\n"
                }
                1 => {
                    "@group(2) @binding(3) var fog_tau: texture_storage_3d<r32float, write>;\n\
                      fn fog_tau_read(column: vec2<u32>, slice: u32) -> f32 { return 0.0; }\n\
                      fn fog_tau_write(column: vec2<u32>, slice: u32, value: f32) {\n\
                          textureStore(fog_tau, vec3<i32>(vec3<u32>(column, slice)), vec4<f32>(value, 0.0, 0.0, 0.0));\n\
                      }\n"
                }
                _ => {
                    "@group(2) @binding(3) var fog_tau: texture_3d<f32>;\n\
                      fn fog_tau_read(column: vec2<u32>, slice: u32) -> f32 {\n\
                          return textureLoad(fog_tau, vec3<i32>(vec3<u32>(column, slice)), 0).r;\n\
                      }\n\
                      fn fog_tau_write(column: vec2<u32>, slice: u32, value: f32) {}\n"
                }
            };
            shader(
                &device,
                "fog-integrate",
                &format!(
                    "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}const FOG_TAU_MODE: u32 = {mode}u;\n{tau}{}",
                    include_str!("../shaders/haze_integrate.wgsl")
                ),
            )
        };
        let fog_integrate_pipeline_with =
            |label: &str,
             module: &wgpu::ShaderModule,
             layout: &wgpu::BindGroupLayout,
             transmittance_only: bool| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: Some(
                        &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: Some(label),
                            bind_group_layouts: &[
                                Some(&haze_layout),
                                Some(light_index_pipelines.layout()),
                                Some(layout),
                            ],
                            immediate_size: 0,
                        }),
                    ),
                    module,
                    entry_point: Some("integrate_grid"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: if transmittance_only {
                            &[("TRANSMITTANCE_ONLY", 1.0)]
                        } else {
                            &[]
                        },
                        ..Default::default()
                    },
                    cache: None,
                })
            };
        let fog_integrate_pipeline = fog_integrate_pipeline_with(
            "fog-integrate",
            &fog_integrate_module(0),
            &fog_integrate_layout,
            false,
        );
        let fog_transmittance_pipeline = fog_integrate_pipeline_with(
            "fog-transmittance",
            &fog_integrate_module(1),
            &fog_tau_write_layout,
            true,
        );
        let fog_integrate_reuse_pipeline = fog_integrate_pipeline_with(
            "fog-integrate-reuse",
            &fog_integrate_module(2),
            &fog_tau_read_layout,
            false,
        );
        let fog_grid_read_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-grid-read"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
        let fog_grid_module = shader(
            &device,
            "fog-grid",
            &format!(
                "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}",
                include_str!("../shaders/haze_grid.wgsl")
            ),
        );
        let fog_grid_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fog-grid"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("fog-grid"),
                    bind_group_layouts: &[
                        Some(&haze_layout),
                        Some(light_index_pipelines.layout()),
                        Some(&fog_grid_write_layout),
                    ],
                    immediate_size: 0,
                }),
            ),
            module: &fog_grid_module,
            entry_point: Some("light_grid"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let haze_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("haze"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("haze"),
                    // The transport declares the compute kernels' grid
                    // groups too; the reference reads neither.
                    bind_group_layouts: &[
                        Some(&haze_layout),
                        Some(light_index_pipelines.layout()),
                        Some(&fog_grid_read_layout),
                        Some(&fog_grid_read_layout),
                    ],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &haze_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &haze_module,
                entry_point: Some("fs_main"),
                // Subframes accumulate additively; each carries weight 1/K.
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: ADD,
                        alpha: ADD,
                    }),
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

        let haze_output_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: SCENE_FORMAT,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        // Lit-interval cache: slot table, header words, payload claims and
        // payload entries. Writable bindings, but this layout belongs to the
        // compute kernels alone, so no other pass is serialized against them.
        let haze_compute_entries = [
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            haze_output_entry(1),
            uniform_entry(4, wgpu::ShaderStages::COMPUTE),
            rw_storage_entry(5, wgpu::ShaderStages::COMPUTE),
            rw_storage_entry(6, wgpu::ShaderStages::COMPUTE),
            rw_storage_entry(7, wgpu::ShaderStages::COMPUTE),
        ];
        let haze_compute_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("haze-compute"),
                entries: &haze_compute_entries,
            });
        let haze_constants = [("GRID_FOG", 1.0)];
        let haze_compact_pipelines = crate::haze_compact::Pipelines::new(
            &device,
            [
                &haze_layout,
                light_index_pipelines.layout(),
                &fog_grid_read_layout,
            ],
            &haze_compute_entries,
            &format!(
                "{haze_base_text}{}",
                include_str!("../shaders/haze_compact.wgsl")
            ),
            &haze_constants,
        );
        let haze_compute_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("haze-compute"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("haze-compute"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&haze_compute_layout),
                            Some(&fog_grid_read_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &haze_module,
                entry_point: Some("compute_haze"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &haze_constants,
                    ..Default::default()
                },
                cache: None,
            });

        // Low quality's closed-form beams (`haze_analytic.wgsl`): the
        // transport's ray and span, none of its march.
        let haze_analytic_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("haze-analytic"),
                entries: &[haze_output_entry(0)],
            });
        let haze_analytic_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("haze-analytic"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("haze-analytic"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&haze_analytic_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &shader(
                    &device,
                    "haze-analytic",
                    &format!(
                        "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}",
                        include_str!("../shaders/haze_analytic.wgsl")
                    ),
                ),
                entry_point: Some("analytic_haze"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });

        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("composite"),
                bind_group_layouts: &[
                    Some(&composite_layout),
                    Some(environment.scene_layout()),
                    Some(atmosphere.composite_layout()),
                ],
                immediate_size: 0,
            });
        let composite_pipelines = Channels::ALL.map(|channels| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("composite"),
                layout: Some(&composite_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &composite_module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(channels.format().into())],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: channels.display_constants(),
                        ..Default::default()
                    },
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        let composite_linear_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("composite-linear"),
                layout: Some(&composite_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &composite_module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(SCENE_FORMAT.into())],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &[("LINEAR_OUTPUT", 1.0)],
                        ..Default::default()
                    },
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX),
                storage_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT),
            ],
        });
        let overlay_module = shader(
            &device,
            "overlay",
            &format!(
                "{}{}",
                include_str!("../shaders/tone.wgsl"),
                include_str!("../shaders/overlay.wgsl")
            ),
        );
        let overlay_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay"),
                bind_group_layouts: &[Some(&overlay_layout)],
                immediate_size: 0,
            });
        let overlay_position_layout = wgpu::VertexBufferLayout {
            array_stride: 48,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            }],
        };
        let overlay_pipelines = std::array::from_fn(|i| {
            let channels = Channels::ALL[i / 4];
            let variant = i % 4;
            let lines = variant & 1 == 1;
            let free = variant & 2 == 2;
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("overlay"),
                layout: Some(&overlay_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &overlay_module,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(overlay_position_layout.clone())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &overlay_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: channels.format(),
                        blend: free.then_some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: if channels == Channels::Rgb10 {
                            &[("TEN_BIT_OUTPUT", 1.0)]
                        } else {
                            &[]
                        },
                        ..Default::default()
                    },
                }),
                primitive: wgpu::PrimitiveState {
                    topology: if lines {
                        wgpu::PrimitiveTopology::LineList
                    } else {
                        wgpu::PrimitiveTopology::TriangleList
                    },
                    // three's `MeshBasicMaterial` is `FrontSide`; the gizmo's
                    // plane quads are one-sided and the hide rules assume it.
                    cull_mode: (!lines).then_some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(if free {
                    wgpu::DepthStencilState {
                        depth_compare: Some(wgpu::CompareFunction::Always),
                        ..depth_state(false)
                    }
                } else {
                    depth_state(true)
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        let (dummy_shadow, _) = shadow_texture_array(&device, 1, 1, 1, "shadow-placeholder");
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::GreaterEqual),
            ..Default::default()
        });
        let hard_shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hard-shadow"),
            compare: Some(wgpu::CompareFunction::GreaterEqual),
            ..Default::default()
        });
        // glTF sampler `wrapS/wrapT = REPEAT`, trilinear — three's default for
        // an imported texture, and the mip chain is what keeps the deck's wood
        // grain from aliasing into a bright fizz at grazing angles.
        let texture_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let floor_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("floor"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let one_pixel = |rgba: [u8; 4], encoding| {
            upload_texture_view(
                &device,
                &queue,
                &Image {
                    width: 1,
                    height: 1,
                    rgba: std::sync::Arc::from(rgba),
                },
                encoding,
            )
        };
        let material_defaults = MaterialDefaults {
            base_color: one_pixel([255; 4], TextureEncoding::Srgb),
            normal: one_pixel([128, 128, 255, 255], TextureEncoding::Linear),
            metallic_roughness: one_pixel([255; 4], TextureEncoding::Linear),
            occlusion: one_pixel([255; 4], TextureEncoding::Linear),
            // The glTF identity for an absent emissive texture is white. The
            // factor, not a synthetic black map, is what disables emission.
            // This also preserves procedural emissive materials, which never
            // carry a glTF image.
            emissive: one_pixel([255; 4], TextureEncoding::Srgb),
        };
        let white_material = material_bind_group(
            &device,
            &material_layout,
            &texture_sampler,
            &material_defaults.base_color,
            &material_defaults.normal,
            &material_defaults.metallic_roughness,
            &material_defaults.occlusion,
            &material_defaults.emissive,
        );

        let shadow_hierarchy_pipelines = crate::shadow_hierarchy::Pipelines::new(&device);
        let sky_visibility = crate::sky_visibility::Pipelines::new(&device, &queue);
        let post = crate::post::Pipelines::new(
            &device,
            &queue,
            &Channels::ALL.map(|channels| (channels.format(), channels.display_constants())),
        );
        let interval_cache_table_bits = crate::interval_cache::table_bits(&device.limits());
        Self {
            sky_visibility,
            post,
            composite_linear_pipeline,
            device,
            queue,
            adapter_profile,
            environment,
            atmosphere,
            sun_shafts,
            laser,
            haze_glow,
            probes,
            scene_layout,
            material_layout,
            cluster_layout,
            haze_layout,
            medium_cache_layout,
            medium_cache_pipeline,
            shadow_hierarchy_pipelines,
            haze_field,
            light_index_pipelines,
            composite_layout,
            overlay_layout,
            scene_pipelines,
            scene_source,
            scene_pipelines_single: std::sync::OnceLock::new(),
            depth_pipeline,
            shadow_pipeline,
            fixture_shadow_layout,
            fixture_shadow_pipeline,
            haze_pipeline,
            haze_compute_pipeline,
            haze_compute_layout,
            haze_analytic_pipeline,
            haze_analytic_layout,
            interval_cache_table_bits,
            haze_compact_pipelines,
            fog_grid_pipeline,
            fog_prepare_pipeline,
            fog_prepare_layout,
            fog_classify_layout,
            fog_classify_pipeline,
            fog_integrate_pipeline,
            fog_transmittance_pipeline,
            fog_integrate_reuse_pipeline,
            fog_integrate_layout,
            fog_tau_write_layout,
            fog_tau_read_layout,
            fog_grid_write_layout,
            fog_grid_read_layout,
            composite_pipelines,
            overlay_pipelines,
            hard_shadow_sampler,
            shadow_sampler,
            dummy_shadow,
            linear_sampler,
            texture_sampler,
            floor_sampler,
            white_material,
            material_defaults,
            lost,
            adopted,
            built_in: started.elapsed(),
        }
    }
}

const ADD: wgpu::BlendComponent = wgpu::BlendComponent {
    src_factor: wgpu::BlendFactor::One,
    dst_factor: wgpu::BlendFactor::One,
    operation: wgpu::BlendOperation::Add,
};

/// Slot of the overlay's pipeline in [`Renderer::overlay_pipelines`]: bit 0 is
/// line topology, bit 1 is [`OverlayDepth::Free`], bits 2 and up are the
/// output's [`Channels::index`].
pub(super) fn overlay_pipeline_index(overlay: &Overlay, channels: Channels) -> usize {
    usize::from(overlay.lines)
        | (usize::from(overlay.depth == OverlayDepth::Free) << 1)
        | (channels.index() << 2)
}
