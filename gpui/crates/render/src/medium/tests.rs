use super::*;
use crate::{device::DeviceContext, haze_field::HazeField};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Ray {
    origin: [f32; 4],
    direction: [f32; 4],
}

#[test]
fn global_height_fog_matches_closed_form_paths() {
    let context = DeviceContext::shared().unwrap();
    let device = &context.device;
    let queue = &context.queue;
    let field = HazeField::bake(device, queue);
    let ray = |height: f32, up: f32, distance: f32| Ray {
        origin: [2500.0, -5000.0, height, 0.0],
        direction: [(1.0_f32 - up * up).sqrt(), 0.0, up, distance],
    };
    let rays = [
        ray(0.0, 0.0, 10.0),
        ray(50.0, 0.0, 100.0),
        ray(0.0, 1.0, 100_000.0),
        ray(1000.0, -1.0, 2000.0),
        ray(20_000.0, -1.0, 30_000.0),
        ray(1000.0, -0.5, 3000.0),
        ray(-10.0, 1.0, 10_000.0),
        ray(0.0, 0.0, 100_000.0),
        ray(20.0, 0.00001, 30.0),
        ray(20.0, -0.00001, 30.0),
    ];
    let uniform = Uniform {
        shape: [0.0, 4.0, 0.0, 16.0],
        wind: [0.0; 4],
        // Deliberately nowhere near the rays: lighting bounds are not density bounds.
        min: [-1.0, -1.0, -1.0, 0.0144],
        max: [1.0, 1.0, 1.0, OUTDOOR_SCALE_HEIGHT_M],
    };
    let source = format!(
        "{}{}{}",
        crate::haze_field::prelude(),
        include_str!("../shaders/medium.wgsl"),
        r"
        struct Ray { origin: vec4<f32>, direction: vec4<f32> };
        @group(0) @binding(0) var haze_noise_field: texture_3d<f32>;
        @group(0) @binding(1) var haze_noise_sampler: sampler;
        @group(0) @binding(2) var<uniform> cfg: ProceduralMedium;
        @group(0) @binding(3) var<storage, read> rays: array<Ray>;
        @group(0) @binding(4) var<storage, read_write> results: array<vec4<f32>>;
        @compute @workgroup_size(1)
        fn main(@builtin(global_invocation_id) id: vec3<u32>) {
            let ray = rays[id.x];
            let total = medium_optical_depth(cfg, ray.origin.xyz, ray.direction.xyz, ray.direction.w);
            let prefix = medium_ray(cfg, ray.origin.xyz, ray.direction.xyz, ray.direction.w);
            results[id.x] = vec4<f32>(total, prefix.optical[32],
                medium_density(cfg, ray.origin.xyz),
                medium_density(cfg, ray.origin.xyz + vec3<f32>(100000.0, 100000.0, 0.0)));
        }
        "
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("global-height-fog-test"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("global-height-fog-test"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&rays),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let bytes = (rays.len() * 16) as u64;
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&field.view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&field.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: input.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(rays.len() as u32, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, bytes);
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let data = readback.slice(..).get_mapped_range().unwrap();
    let values: &[[f32; 4]] = bytemuck::cast_slice(&data);
    let near_horizontal = 0.0144 * 30.0 * (-20.0_f32 / 50.0).exp();
    let expected = [
        0.144,
        1.44 / std::f32::consts::E,
        0.72,
        0.72,
        0.72,
        1.44,
        0.72,
        16.0,
        near_horizontal,
        near_horizontal,
    ];
    for (i, (&want, value)) in expected.iter().zip(values).enumerate() {
        assert!(value.iter().all(|v| v.is_finite()), "ray {i}: {value:?}");
        assert!(
            (value[0] - want).abs() < 0.0002,
            "ray {i}: {} vs {want}",
            value[0]
        );
        assert!(
            (value[1] - want).abs() < 0.0002,
            "prefix ray {i}: {} vs {want}",
            value[1]
        );
        assert_eq!(
            value[2].to_bits(),
            value[3].to_bits(),
            "density changed outside the lighting volume"
        );
    }
}
