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
    let values = measure_rays(uniform, &rays);
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

fn measure_rays(uniform: Uniform, rays: &[Ray]) -> Vec<[f32; 4]> {
    run(
        uniform,
        rays,
        r"
            let total = medium_optical_depth(cfg, ray.origin.xyz, ray.direction.xyz, ray.direction.w);
            let prefix = medium_ray(cfg, ray.origin.xyz, ray.direction.xyz, ray.direction.w);
            results[id.x] = vec4<f32>(total, prefix.optical[32],
                medium_density(cfg, ray.origin.xyz),
                medium_density(cfg, ray.origin.xyz + vec3<f32>(100000.0, 100000.0, 0.0)));
        ",
    )
}

/// Runs `body` once per ray, with `ray`, `cfg` and `results` in scope.
fn run(uniform: Uniform, rays: &[Ray], body: &str) -> Vec<[f32; 4]> {
    let context = DeviceContext::shared().unwrap();
    let device = &context.device;
    let queue = &context.queue;
    let field = HazeField::bake(device, queue);
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
            BODY
        }
        "
        .replace("BODY", body)
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
        contents: bytemuck::cast_slice(rays),
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
    values.to_vec()
}

#[test]
fn scalar_cloud_depth_matches_complete_prefix() {
    // Exercise both room boundaries and the global outdoor field, including
    // ground crossings, near-horizontal paths and moving cloud deformation.
    let mut rays = Vec::new();
    for height in [-4.0, 0.0, 1.7, 27.0, 60.0] {
        for up in [-1.0_f32, -0.37, -0.00001, 0.0, 0.00001, 0.37, 1.0] {
            for distance in [0.0, 0.1, 3.0, 25.0, 130.0, 199.0] {
                rays.push(Ray {
                    origin: [-2.0, 3.0, height, 0.0],
                    direction: [(1.0 - up * up).sqrt(), 0.0, up, distance],
                });
            }
        }
    }
    for scale_height in [0.0, OUTDOOR_SCALE_HEIGHT_M] {
        for time in [0.0, 3.0, 17.3] {
            let uniform = Uniform {
                shape: [0.35, 4.0, 0.3, 16.0],
                wind: [0.13, 0.075, 0.0, time],
                min: [-20.0, -30.0, 0.0, 0.0144],
                max: [20.0, 30.0, 30.0, scale_height],
            };
            for (i, value) in measure_rays(uniform, &rays).iter().enumerate() {
                assert!(
                    value[0].is_finite() && value[0] >= 0.0,
                    "ray {i}: {value:?}"
                );
                assert_eq!(
                    value[0].to_bits(),
                    value[1].to_bits(),
                    "scalar/prefix mismatch: ray {i}, scale height {scale_height}, time {time}"
                );
            }
        }
    }
}

/// Points scattered over a 200 m cube, `footprint` metres across.
fn scattered(count: usize, footprint: f32) -> Vec<Ray> {
    let mut seed = 0x9e37_79b9_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 200.0 - 100.0
    };
    (0..count)
        .map(|_| Ray {
            origin: [next(), next(), next(), footprint],
            direction: [0.0; 4],
        })
        .collect()
}

/// Indoor haze, clouds `largest` metres across, stirred by `turbulence`,
/// at `time`, with no wind.
fn stirred(cloudiness: f32, largest: f32, turbulence: f32, time: f32) -> Uniform {
    Uniform {
        shape: [cloudiness, largest, turbulence, 16.0],
        wind: [0.0, 0.0, 0.0, time],
        min: [-1000.0, -1000.0, -1000.0, 0.0144],
        max: [1000.0, 1000.0, 1000.0, 0.0],
    }
}

/// The clouds and wisps at each point: x the clouds, y the wisps.
fn haze_at(uniform: Uniform, points: &[Ray]) -> Vec<[f32; 2]> {
    run(
        uniform,
        points,
        "results[id.x] = vec4<f32>(medium_cloud(cfg, ray.origin.xyz), \
            medium_wisps(cfg, ray.origin.xyz, ray.origin.w), 0.0, 0.0);",
    )
    .into_iter()
    .map(|v| [v[0], v[1]])
    .collect()
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let mean = |v: &[f32]| v.iter().map(|&x| f64::from(x)).sum::<f64>() / v.len() as f64;
    let (ma, mb) = (mean(a), mean(b));
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b) {
        let (x, y) = (f64::from(x) - ma, f64::from(y) - mb);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    ab / (aa * bb).sqrt()
}

fn spread(v: &[f32]) -> (f64, f64) {
    let mean = v.iter().map(|&x| f64::from(x)).sum::<f64>() / v.len() as f64;
    let variance = v
        .iter()
        .map(|&x| (f64::from(x) - mean).powi(2))
        .sum::<f64>()
        / v.len() as f64;
    (mean, variance.sqrt())
}

/// An eddy r across, in air stirred at ε = 0.1·turbulence³ m²/s³, turns over
/// in r / (ε·r)^⅓ seconds (Kolmogorov). The clouds, the largest eddies, are
/// much as they were a tenth of a turn later and nothing like it a turn
/// later, whatever their size and stirring, and never jump between; in
/// still air they never change.
#[test]
fn clouds_turn_over_as_fast_as_their_size_and_stirring_say() {
    let points = scattered(4096, 0.0);
    let clouds =
        |uniform: Uniform| -> Vec<f32> { haze_at(uniform, &points).iter().map(|v| v[0]).collect() };
    for (largest, turbulence) in [(4.0_f32, 0.3_f32), (16.0, 0.6), (1.0, 1.0)] {
        let epsilon = 0.1 * turbulence.powi(3);
        let turn = largest / (epsilon * largest).cbrt();
        // Away from a turn's ends, so neither copy is mid-jump.
        let start = 0.3 * turn;
        let then = clouds(stirred(0.35, largest, turbulence, start));
        let soon = correlation(
            &then,
            &clouds(stirred(0.35, largest, turbulence, start + 0.1 * turn)),
        );
        let later = correlation(
            &then,
            &clouds(stirred(0.35, largest, turbulence, start + turn)),
        );
        assert!(
            soon > 0.8,
            "{largest} m at {turbulence}: {soon} a tenth of a turn on"
        );
        assert!(
            later.abs() < 0.15,
            "{largest} m at {turbulence}: {later} a turn on"
        );
    }
    // Each half turn the clouds step on through the field; the step itself
    // shows nowhere.
    let turn = 4.0 / (0.1 * 0.3_f32.powi(3) * 4.0).cbrt();
    for step in [0.5, 1.0, 7.5] {
        let before = clouds(stirred(0.35, 4.0, 0.3, (step - 1e-4) * turn));
        let after = clouds(stirred(0.35, 4.0, 0.3, (step + 1e-4) * turn));
        let jump = before
            .iter()
            .zip(&after)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(jump < 0.01, "clouds jumped {jump} at {step} turns");
    }
    assert_eq!(
        clouds(stirred(0.35, 4.0, 0.0, 0.0)),
        clouds(stirred(0.35, 4.0, 0.0, 1000.0)),
        "still air changed"
    );
}

/// The haze's contrast at a size r is the clouds' times (r / L)^⅓, L the
/// largest eddy (Obukhov–Corrsin): the wisps are fainter in haze of bigger
/// clouds, gone in haze with none, and only move the haze about, never add
/// to it. A pixel as wide as a wisp averages it away.
#[test]
fn wisps_are_as_faint_as_their_size_says() {
    let points = scattered(16384, 0.0);
    let wisps = |cloudiness: f32, largest: f32| -> Vec<f32> {
        haze_at(stirred(cloudiness, largest, 0.4, 5.0), &points)
            .iter()
            .map(|v| v[1])
            .collect()
    };
    let (small_mean, small) = spread(&wisps(0.35, 4.0));
    let (large_mean, large) = spread(&wisps(0.35, 16.0));
    let law = (4.0_f64 / 16.0).cbrt();
    assert!(
        (large / small - law).abs() < 0.05,
        "{large} / {small} vs {law}"
    );
    for mean in [small_mean, large_mean] {
        assert!((mean - 1.0).abs() < 0.01, "mean {mean}");
    }
    assert!(
        wisps(0.0, 4.0).iter().all(|&w| w == 1.0),
        "wisps without clouds"
    );
    let wide = haze_at(stirred(0.35, 4.0, 0.4, 5.0), &scattered(256, 0.5));
    assert!(
        wide.iter().all(|v| v[1] == 1.0),
        "a wide pixel showed wisps"
    );
}
