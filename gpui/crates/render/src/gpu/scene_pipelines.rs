//! The scene pass's pipelines, at the sample count a quality level asks
//! for: built at [`MSAA_SAMPLES`] up front and single-sampled on demand.

use super::descriptors::depth_state;
use super::{Gpu, MSAA_SAMPLES, SCENE_FORMAT};

/// The scene pass's multisampled pipelines. Below high quality it renders single
/// sampled, and the sample count is baked into a pipeline.
pub(super) struct ScenePipelines {
    pub(super) scene: wgpu::RenderPipeline,
    /// The ground under a floor material: `scene.wgsl`'s `fs_ground`.
    pub(super) ground: wgpu::RenderPipeline,
    pub(super) surface_depth: wgpu::RenderPipeline,
    pub(super) grid: wgpu::RenderPipeline,
    pub(super) compass: wgpu::RenderPipeline,
    pub(super) cables: wgpu::RenderPipeline,
    /// The reflection probes as small balls (`probe_debug.wgsl`).
    pub(super) probe_debug: wgpu::RenderPipeline,
}

/// Everything [`ScenePipelines`] is built from, kept so a second sample count
/// can be built when it is first asked for.
pub(super) struct ScenePipelineSource {
    pub(super) layout: wgpu::PipelineLayout,
    pub(super) scene: wgpu::ShaderModule,
    pub(super) grid: wgpu::ShaderModule,
    pub(super) cables: wgpu::ShaderModule,
}

const MESH_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 4] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 12,
        shader_location: 1,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x2,
        offset: 24,
        shader_location: 2,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x4,
        offset: 32,
        shader_location: 3,
    },
];

pub(super) fn mesh_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: 48,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MESH_VERTEX_ATTRIBUTES,
    }
}

impl ScenePipelineSource {
    pub(super) fn build(&self, device: &wgpu::Device, samples: u32) -> ScenePipelines {
        let lit = |label: &str, entry_point: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&self.layout),
                vertex: wgpu::VertexState {
                    module: &self.scene,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(mesh_vertex_layout())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.scene,
                    entry_point: Some(entry_point),
                    targets: &[Some(SCENE_FORMAT.into())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(depth_state(true)),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let scene = lit("scene", "fs_main");
        let ground = lit("ground", "fs_ground");

        let surface_depth = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("surface-depth"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.scene,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.scene,
                entry_point: Some("fs_surface_depth"),
                targets: &[Some(wgpu::TextureFormat::R16Float.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let probe_debug = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("probe-debug"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.scene,
                entry_point: Some("vs_probe_debug"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.scene,
                entry_point: Some("fs_probe_debug"),
                targets: &[Some(SCENE_FORMAT.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let grid = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.grid,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.grid,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            // `depthWrite: false` — the grid tests against the stage but never
            // occludes it.
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        // The grid's own state, with the grid's own module, down to a second
        // fragment entry point: the compass is the same quad, blended the same
        // way, and a divergent depth or blend choice here would be a second
        // answer to a settled question.
        let compass = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("compass"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.grid,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.grid,
                entry_point: Some("fs_compass"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        // The grid's own state, with the grid's shader swapped out: both are
        // transparent scene-space affordances that test depth and never write
        // it, and a second blend or depth choice here would be a second answer
        // to the same question.
        let cables = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cables"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.cables,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.cables,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        ScenePipelines {
            scene,
            ground,
            surface_depth,
            grid,
            compass,
            cables,
            probe_debug,
        }
    }
}

impl Gpu {
    /// The scene pipelines for `samples` per pixel: [`MSAA_SAMPLES`] or one.
    pub(super) fn scene_pipelines(&self, samples: u32) -> &ScenePipelines {
        if samples == MSAA_SAMPLES {
            &self.scene_pipelines
        } else {
            self.scene_pipelines_single
                .get_or_init(|| self.scene_source.build(&self.device, samples))
        }
    }
}
