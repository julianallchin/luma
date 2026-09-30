//! An HDR frame carries the SDR frame's dither.
//!
//! ```sh
//! cargo test -p luma-render --test hdr_dither
//! ```
//!
//! The HDR target is half-float, but what the display gets is not: the
//! compositor encodes it into a 10-bit PQ swapchain, and a capture of it is
//! 8-bit. Without noise, the dim soft edge of a beam over a dusk sky crosses
//! those steps as a staircase of flat runs. The HDR frame below the knee must
//! be the SDR frame noise included, so its 8-bit encoding matches the SDR
//! frame's bytes.
//!
//! One test to a binary: adopting a device is process-wide.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::frame::FixtureCone;
use luma_render::scene_desc::{CameraPose, RenderSettings, Scene, VenueEnvironment, VenueHaze};
use luma_render::{build_frame_with, AsyncPresentation, AsyncViewport, DisplayRange, Frame};

const WIDTH: u32 = 240;
const HEIGHT: u32 = 160;

/// A dusk sky, just after sunset, with one hazy beam grazing across it.
fn frame() -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(-1.8),
        VenueHaze::default(),
        50.0,
        1.0,
    );
    render.show_grid = false;
    render.show_floor = false;
    let scene = Scene {
        id: "hdr-dither".into(),
        times: vec![0.0],
        // Three-space: (x, up, -y). Looking level at the horizon.
        camera: CameraPose {
            position: [0.0, 2.0, 30.0],
            target: [0.0, 6.0, 0.0],
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: Vec::new(),
        state: BTreeMap::new(),
    };
    let mut frame =
        build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap();
    frame.fixture_cones = vec![FixtureCone {
        strobe: luma_render::strobe::Rows::STEADY,
        position: Vec3::new(4.0, 0.0, 7.0),
        range: 40.0,
        direction: Vec3::new(-1.0, 0.0, 0.25).normalize(),
        cos_beam: 0.995,
        color: Vec3::new(0.3, 0.8, 1.0),
        intensity: 4.0,
        cos_field: 0.98,
        wash: 0.0,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: luma_render::luminaire::Lens::POINT,
    }];
    frame
}

/// A device like the one gpui's compositor makes, offered to the renderer.
fn adopt_a_compositor_device() -> bool {
    let instance = wgpu::Instance::default();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return false;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features()
            & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::SUBGROUP),
        required_limits: wgpu::Limits {
            max_storage_buffer_binding_size: adapter.limits().max_storage_buffer_binding_size,
            max_buffer_size: adapter.limits().max_buffer_size,
            max_storage_buffers_per_shader_stage:
                adapter.limits().max_storage_buffers_per_shader_stage,
            max_sampled_textures_per_shader_stage: adapter
                .limits()
                .max_sampled_textures_per_shader_stage,
            ..wgpu::Limits::default().using_resolution(adapter.limits())
        },
        ..Default::default()
    })) else {
        return false;
    };
    luma_render::device::DeviceContext::adopt(
        Arc::new(device),
        Arc::new(queue),
        adapter,
        Arc::default(),
    );
    true
}

/// The first frame of a fresh viewport, so the two ranges draw comparable
/// frames.
fn draw_one(range: DisplayRange) -> AsyncPresentation {
    let mut viewport = AsyncViewport::new();
    viewport.set_subframes(1);
    viewport.set_display_range(range);
    viewport.submit(frame(), WIDTH, HEIGHT);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(result) = viewport.take_latest() {
            return result.expect("the frame rendered");
        }
        assert!(
            Instant::now() < deadline,
            "the renderer worker never completed a frame"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn an_hdr_frame_keeps_the_sdr_frames_dither() {
    if !adopt_a_compositor_device() {
        eprintln!("no GPU adapter; skipping");
        return;
    }
    let sdr = draw_one(DisplayRange::Sdr).image.to_bytes();
    let hdr = draw_one(DisplayRange::Hdr { headroom: 4.0 })
        .image
        .to_bytes();
    let hdr = luma_render::image_out::rgba8_from_presented(&hdr, WIDTH, HEIGHT).unwrap();

    let (mut compared, mut equal) = (0, 0);
    let (mut sdr_flat, mut hdr_flat) = (0, 0);
    let rgb = |bytes: &[u8], i: usize, bgra: bool| {
        let t = &bytes[i * 4..i * 4 + 3];
        if bgra {
            [t[2], t[1], t[0]]
        } else {
            [t[0], t[1], t[2]]
        }
    };
    for i in 0..(WIDTH * HEIGHT) as usize {
        let (s, h) = (rgb(&sdr, i, true), rgb(&hdr, i, false));
        // Below the knee (`HDR_KNEE` 0.6 is sRGB ~203); dark enough that an
        // 8-bit step is visible.
        if s.iter().any(|&v| v > 160) || s.iter().all(|&v| v < 4) {
            continue;
        }
        compared += 1;
        equal += usize::from(s == h);
        if i % WIDTH as usize > 0 {
            sdr_flat += usize::from(s == rgb(&sdr, i - 1, true));
            hdr_flat += usize::from(h == rgb(&hdr, i - 1, false));
        }
    }
    eprintln!(
        "{compared} texels compared, {equal} equal; flat neighbours SDR {sdr_flat}, HDR {hdr_flat}"
    );
    assert!(
        compared > (WIDTH * HEIGHT / 2) as usize,
        "most of the frame is sky"
    );
    // The GPU's sRGB encode and the capture's round a value on a step either
    // way, so some texels differ by one. Without the noise, most would.
    assert!(
        equal * 4 >= compared * 3,
        "below the knee the HDR frame is the SDR frame, noise included: {equal} of {compared}"
    );
    assert!(
        hdr_flat * 10 <= sdr_flat * 12,
        "the HDR frame has the SDR frame's flat runs, no more: {hdr_flat} vs {sdr_flat}"
    );
}
