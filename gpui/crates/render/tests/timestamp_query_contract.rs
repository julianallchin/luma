//! Every sample index of a timestamp query set records, high indices
//! included: the layout in `gpu.rs::QUERY_COUNT` is free to use as many as it
//! declares. Pinned against a standalone device so a wgpu or driver change
//! that breaks it is attributed here rather than in the renderer.
//!
//! The other property the profiler depends on — that the set must be resolved
//! only once the frame has completed, because `resolve_query_set` runs
//! alongside the render stream and otherwise reads it before it has settled —
//! is not pinned here: a synthetic workload reproduces it only intermittently.
//! `timestamp_lie.rs` covers it instead, at a viewport size where the renderer
//! drops tail samples deterministically without it.
//!
//! Run: cargo test -p luma-render --release --test timestamp_query_contract

use std::sync::mpsc;

const PASSES: u32 = 4;
const SAMPLES: u32 = PASSES * 2;

const SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let x = f32(i32(i % 2u) * 4 - 1);
    let y = f32(i32(i / 2u) * 4 - 1);
    return vec4f(x, y, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) p: vec4f) -> @location(0) vec4f {
    var v = p.x * 1e-4 + p.y * 1e-5 + 0.5;
    for (var i = 0u; i < 4000u; i = i + 1u) {
        v = v + sin(v) * 1e-6 + 1e-7;
    }
    return vec4f(fract(v), 0.0, 0.0, 1.0);
}
"#;

struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    view: wgpu::TextureView,
    pipeline: wgpu::RenderPipeline,
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl Harness {
    fn new() -> Self {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .expect("adapter");
        assert!(
            adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            "adapter lacks TIMESTAMP_QUERY; the contract is unobservable here"
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("timestamp-contract"),
            required_features: wgpu::Features::TIMESTAMP_QUERY,
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .expect("device");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor::default());
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 2048,
                height: 2048,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let bytes = u64::from(SAMPLES) * 8;
        Self {
            view: target.create_view(&Default::default()),
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: None,
                ty: wgpu::QueryType::Timestamp,
                count: SAMPLES,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            pipeline,
            device,
            queue,
        }
    }

    /// Encode `PASSES` sampled passes, each writing its own (begin, end) pair.
    fn encode_passes(&self, encoder: &mut wgpu::CommandEncoder) {
        for pass_index in 0..PASSES {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                    query_set: &self.queries,
                    beginning_of_pass_write_index: Some(pass_index * 2),
                    end_of_pass_write_index: Some(pass_index * 2 + 1),
                }),
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.draw(0..3, 0..1);
        }
    }

    fn encode_resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.queries, 0..SAMPLES, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, u64::from(SAMPLES) * 8);
    }

    fn wait(&self) {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
    }

    fn drain(&self) -> Vec<u64> {
        let (tx, rx) = mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.wait();
        rx.recv().expect("map callback").expect("map ok");
        let samples = {
            let data = self.readback.slice(..).get_mapped_range().expect("mapped");
            data.chunks_exact(8)
                .map(|bytes| u64::from_ne_bytes(bytes.try_into().unwrap()))
                .collect()
        };
        self.readback.unmap();
        samples
    }
}

/// Every declared sample index records, high indices included: nothing about
/// the renderer's layout has to avoid the upper half of its query set.
#[test]
fn every_sample_index_of_a_query_set_records() {
    let harness = Harness::new();
    let mut encoder = harness.device.create_command_encoder(&Default::default());
    harness.encode_passes(&mut encoder);
    harness.queue.submit([encoder.finish()]);
    // Resolve only once the passes have finished — see the sibling test for
    // what an in-frame resolve does instead.
    harness.wait();
    let mut encoder = harness.device.create_command_encoder(&Default::default());
    harness.encode_resolve(&mut encoder);
    harness.queue.submit([encoder.finish()]);
    harness.wait();

    let samples = harness.drain();
    let unrecorded: Vec<_> = samples
        .iter()
        .enumerate()
        .filter(|(_, value)| **value == 0)
        .map(|(index, _)| index)
        .collect();
    assert!(
        unrecorded.is_empty(),
        "sample indices {unrecorded:?} never recorded: {samples:?}"
    );
}
