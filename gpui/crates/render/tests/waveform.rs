use luma_render::{
    device::DeviceContext,
    waveform::{View, Waveform},
};

fn view(start: f64, seconds_per_pixel: f64, width: u32) -> View {
    View {
        start,
        seconds_per_pixel,
        width,
        height: 80,
        padding: 8.,
        colors: [
            [0., 0., 0., 1.],
            [0., 0., 1., 1.],
            [1., 0.5, 0., 1.],
            [1., 1., 1., 1.],
        ],
    }
}

#[test]
fn gpu_waveform_matches_half_precision_sample_intervals() {
    let context = context();
    let count = 10003;
    let bands: [Vec<f32>; 3] = std::array::from_fn(|band| {
        (0..count)
            .map(|i| {
                if i % (17 + band * 11) == 0 {
                    -0.8
                } else {
                    ((i * 37 + band * 13) % 97) as f32 / 500.
                }
            })
            .collect()
    });
    let gpu = Waveform::new(context, &bands, [1.; 3], [0.95, 0.8, 0.6], 1000).unwrap();
    for (start, per_pixel, width) in [
        (0., 0.001, 1200),
        (3.001, 0.0005, 1200),
        (0., 0.016, 700),
        (9.999, 0.001, 32),
    ] {
        let v = view(start, per_pixel, width);
        let frame = gpu.render(v).unwrap();
        let pixels = frame.pixels().unwrap();
        let origin = (start * 1000.).floor() as usize;
        let fraction = (start * 1000.).fract() as f32;
        let step = (per_pixel * 1000.) as f32;
        for x in 0..width as usize {
            let a = (origin + (fraction + x as f32 * step).floor() as usize).min(count);
            let b = (origin + (fraction + (x + 1) as f32 * step).ceil() as usize).min(count);
            let heights: [f32; 3] = std::array::from_fn(|band| {
                let peak = bands[band][a..b]
                    .iter()
                    .map(|v| half::f16::from_f32(v.abs()).to_f32())
                    .fold(0., f32::max);
                ((1. + 9. * peak).log10() * [0.95, 0.8, 0.6][band] * 36.).floor()
            });
            for y in 0..80 {
                let distance = (y as f32 + 0.5 - 40.).abs();
                let mut expected = [0, 0, 0, 255];
                for (band, color) in [[255, 0, 0, 255], [0, 128, 255, 255], [255, 255, 255, 255]]
                    .into_iter()
                    .enumerate()
                {
                    if distance < heights[band] {
                        expected = color;
                    }
                }
                let actual = &pixels[(y * width as usize + x) * 4..][..4];
                assert!(actual.iter().zip(expected).all(|(a,b)| (*a as i32-b as i32).abs() <= 1), "start={start} step={per_pixel} x={x} y={y} range={a}..{b} actual={actual:?} expected={expected:?}");
            }
        }
    }
}

#[test]
#[ignore = "GPU timing diagnostic"]
fn gpu_waveform_timings() {
    use std::time::Instant;
    let context = context();
    eprintln!("adapter: {:?}", context.adapter_info());
    let count = 206 * 48000;
    let bands: [Vec<f32>; 3] = std::array::from_fn(|band| {
        (0..count)
            .map(|i| ((i * (band + 1) % 1000) as f32 / 1000.) - 0.5)
            .collect()
    });
    let started = Instant::now();
    let gpu = Waveform::new(context, &bands, [1.; 3], [0.95, 0.8, 0.6], 48000).unwrap();
    eprintln!("resident hierarchy: {} bytes", gpu.hierarchy_bytes());
    assert!(gpu.hierarchy_bytes() < 21_000_000);
    // First render waits for the hierarchy as well as pipelines and uploads.
    gpu.render(view(0., 206. / 1200., 1200)).unwrap();
    eprintln!(
        "upload + hierarchy + pipelines + first render: {:.3}ms",
        started.elapsed().as_secs_f64() * 1000.
    );
    for (label, step, width) in [
        ("deep", 1. / 500., 1200),
        ("normal", 1. / 50., 1200),
        ("minimap", 206. / 1200., 1200),
        ("4k minimap", 206. / 4000., 4000),
    ] {
        let mut times = Vec::new();
        for i in 0..53 {
            let start = Instant::now();
            let frame = gpu.render(view(0., step, width)).unwrap();
            std::hint::black_box(&frame);
            let ms = start.elapsed().as_secs_f64() * 1000.;
            if i >= 3 {
                times.push(ms);
            }
        }
        times.sort_by(f64::total_cmp);
        eprintln!(
            "{label}: submit-through-completion median={:.3}ms p95={:.3}ms",
            times[25], times[47]
        );
    }
}

fn context() -> std::sync::Arc<DeviceContext> {
    static CONTEXT: std::sync::OnceLock<std::sync::Arc<DeviceContext>> = std::sync::OnceLock::new();
    CONTEXT
        .get_or_init(|| {
            let adapter = pollster::block_on(
                wgpu::Instance::default().request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .unwrap();
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                    ..Default::default()
                }))
                .unwrap();
            DeviceContext::adopt(
                std::sync::Arc::new(device),
                std::sync::Arc::new(queue),
                adapter,
                std::sync::Arc::default(),
            );
            DeviceContext::shared().unwrap()
        })
        .clone()
}

#[test]
fn capture_shared_gpu_waveform_strips() {
    let rate = 1000;
    let bands: [Vec<f32>; 3] = std::array::from_fn(|band| {
        (0..206 * rate)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let envelope = 0.08
                    + (t * 0.23 + band as f32).sin().abs() * 0.4
                    + (t * 3.).sin().abs().powi(12) * 0.45;
                envelope * (t * [97., 211., 397.][band]).sin()
            })
            .collect()
    });
    let gpu = Waveform::new(context(), &bands, [1.; 3], [0.95, 0.8, 0.6], rate as u32).unwrap();
    let mut joined = vec![0; 1200 * 128 * 4];
    for (y, mut v) in [
        (0, view(0., 206. / 1200., 1200)),
        (48, view(60., 1. / 500., 1200)),
    ] {
        if y == 0 {
            v.height = 40;
            v.padding = 4.;
        }
        v.colors = [
            [0.06, 0.06, 0.07, 1.],
            [0., 0.333, 0.886, 1.],
            [0.949, 0.667, 0.235, 1.],
            [1., 1., 1., 1.],
        ];
        let frame = gpu.render(v).unwrap();
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
        assert!(
            frame.surface().is_some(),
            "capture must exercise shared presentation"
        );
        let mut pixels = frame.pixels().unwrap();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        joined[y * 1200 * 4..y * 1200 * 4 + pixels.len()].copy_from_slice(&pixels);
    }
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/waveform-gpu.png");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    luma_render::image_out::write(&path, &joined, 1200, 128).unwrap();
    eprintln!("GPU waveform capture: {}", path.display());
}

#[test]
fn compact_peaks_preserve_impulses_with_only_bin_edge_expansion() {
    // Quiet signal and isolated single-sample impulses expose boundary smearing.
    for rate in [44100u32, 48000, 96000] {
        let count = rate as usize * 2 + 3;
        let mut signal = vec![0.02f32; count];
        for i in (7..count).step_by(997) {
            signal[i] = 0.9;
        }
        let bands = [signal, vec![0.; count], vec![0.; count]];
        let gpu = Waveform::new(context(), &bands, [1.; 3], [0.95, 0.8, 0.6], rate).unwrap();
        let bin = (rate as usize / 6000).max(1);
        for scale in [1., 2., 3., 10., 20., 30.] {
            let v = view(0.12347, 1. / (500. * scale), 600);
            let pixels = gpu.render(v).unwrap().pixels().unwrap();
            let leaf_rate = rate as f64 / bin as f64;
            let origin = (v.start * leaf_rate).floor() as usize;
            let fraction = (v.start * leaf_rate).fract() as f32;
            let step = (v.seconds_per_pixel * leaf_rate) as f32;
            for x in 0..v.width as usize {
                let a = ((origin + (fraction + x as f32 * step).floor() as usize) * bin).min(count);
                let b = ((origin + (fraction + (x + 1) as f32 * step).ceil() as usize) * bin)
                    .min(count);
                let exact_a =
                    ((v.start + x as f64 * v.seconds_per_pixel) * rate as f64).floor() as usize;
                let exact_b = ((v.start + (x + 1) as f64 * v.seconds_per_pixel) * rate as f64)
                    .ceil() as usize;
                assert!(a <= exact_a && b >= exact_b);
                assert!(exact_a - a < bin && b - exact_b < bin);
                let peak = bands[0][a..b].iter().copied().fold(0., f32::max);
                let peak = half::f16::from_f32(peak).to_f32();
                let expected = ((1. + 9. * peak).log10() * 0.95 * 36.).floor() as usize;
                let actual = (0..40).filter(|y| pixels[(y * 600 + x) * 4] > 200).count();
                assert_eq!(actual, expected, "rate={rate} scale={scale} x={x}");
            }
        }
    }
}

#[test]
fn concurrent_completions_preserve_each_view_and_retained_surface() {
    let bands = std::array::from_fn(|band| {
        (0..6000)
            .map(|i| ((i * (17 + band * 11) % 997) as f32 / 997.) * 0.9)
            .collect()
    });
    let gpu = std::sync::Arc::new(
        Waveform::new(context(), &bands, [1.; 3], [0.95, 0.8, 0.6], 1000).unwrap(),
    );
    let retained = gpu.render(view(0., 0.002, 640)).unwrap();
    let retained_pixels = retained.pixels().unwrap();
    let views = [
        view(0.123, 0.0005, 959),
        view(2.997, 0.003, 321),
        view(5.993, 0.001, 37),
    ];
    let concurrent = std::thread::scope(|scope| {
        let jobs: Vec<_> = views
            .into_iter()
            .map(|view| {
                let gpu = gpu.clone();
                scope.spawn(move || gpu.render(view).unwrap())
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    for (view, frame) in views.into_iter().zip(concurrent) {
        assert_eq!(
            frame.pixels().unwrap(),
            gpu.render(view).unwrap().pixels().unwrap(),
            "concurrent completion published another view or unfinished pixels"
        );
    }
    assert_eq!(
        retained.pixels().unwrap(),
        retained_pixels,
        "later submissions changed a surface retained by the compositor"
    );
}
