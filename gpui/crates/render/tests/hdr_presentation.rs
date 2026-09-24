//! An HDR frame is the SDR frame with its highlights kept.
//!
//! ```sh
//! cargo test -p luma-render --test hdr_presentation
//! ```
//!
//! The same descriptor is drawn for an SDR compositor and for an HDR one on
//! the compositor's own (adopted) device. Below the composite pass's knee the
//! two must agree to the 8-bit step; above it the HDR frame may go past SDR
//! white, and never past the headroom.
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
use luma_render::scene_desc::{CameraPose, DebugView, Environment, Piece, RenderSettings, Scene};
use luma_render::{
    build_frame_with, AsyncPresentation, AsyncViewport, DisplayRange, Frame, Presented,
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;
const HEADROOM: f32 = 4.0;
/// `HDR_KNEE` in `tone.wgsl`: up to here HDR is SDR exactly.
const KNEE: f32 = 0.6;

/// A dark stage with one bright hazy beam, so the frame has both a range the
/// two transforms share and highlights AgX compresses into white.
fn frame() -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut render = RenderSettings::dark_stage(48.0, 0.5);
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.haze.enabled = true;
    render.haze.steps = 8;
    render.haze.density = 0.65;
    render.debug_view = DebugView::Pbr;
    let scene = Scene {
        id: "hdr-presentation".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: Vec::<Piece>::new(),
        state: BTreeMap::new(),
    };
    let mut frame =
        build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap();
    frame.fixture_cones = vec![FixtureCone {
        position: Vec3::new(0.0, 0.0, 0.15),
        range: 8.0,
        direction: Vec3::Z,
        cos_beam: 0.975,
        color: Vec3::new(1.0, 0.9, 0.8),
        intensity: 4.0,
        cos_field: 0.93,
        wash: 0.0,
        gobo: 0,
        gobo_rotation: 0.31,
        haze_gain: 1.0,
    }];
    frame.haze_density = 0.65;
    frame
}

/// Play the window: a device like the one gpui's compositor makes, offered
/// to the renderer so its frames can be shared.
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

/// Draw the first frame of a fresh viewport, which has no temporal history
/// behind it, so two viewports draw comparable frames.
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

fn srgb_to_linear(byte: u8) -> f32 {
    let v = f32::from(byte) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb_byte(v: f32) -> i32 {
    let v = v.clamp(0.0, 1.0);
    let encoded = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as i32
}

#[test]
fn an_hdr_frame_is_the_sdr_frame_with_its_highlights_kept() {
    if !adopt_a_compositor_device() {
        eprintln!("no GPU adapter; skipping");
        return;
    }
    let sdr = draw_one(DisplayRange::Sdr);
    let hdr = draw_one(DisplayRange::Hdr { headroom: HEADROOM });
    assert!(
        matches!(sdr.image, Presented::Shared(_)) && matches!(hdr.image, Presented::Shared(_)),
        "an adopted device shares its frames"
    );

    let sdr = sdr.image.to_bytes();
    let hdr: Vec<f32> = hdr
        .image
        .to_bytes()
        .chunks_exact(2)
        .map(|bytes| half::f16::from_le_bytes([bytes[0], bytes[1]]).to_f32())
        .collect();
    let texels = (WIDTH * HEIGHT) as usize;
    assert_eq!(sdr.len(), texels * 4, "SDR is BGRA8");
    assert_eq!(hdr.len(), texels * 4, "HDR is RGBA half-float");

    let mut below_knee = 0;
    let mut above_white = 0;
    let mut brightest = 0.0_f32;
    for (bgra, rgba) in sdr.chunks_exact(4).zip(hdr.chunks_exact(4)) {
        let sdr_rgb = [bgra[2], bgra[1], bgra[0]];
        let hdr_rgb = [rgba[0], rgba[1], rgba[2]];
        let peak = hdr_rgb.iter().copied().fold(0.0, f32::max);
        brightest = brightest.max(peak);
        assert!(
            hdr_rgb.iter().all(|v| v.is_finite() && *v >= 0.0),
            "HDR values are finite and non-negative: {hdr_rgb:?}"
        );
        assert!(
            peak <= HEADROOM * 1.001,
            "HDR goes to the headroom and no further: {peak}"
        );
        assert_eq!(rgba[3], 1.0, "the viewport is opaque");
        if peak > 1.0 {
            above_white += 1;
        }
        // A little under the knee, so a pixel the 8-bit SDR frame rounded
        // across it does not count.
        if peak < KNEE * 0.95 {
            below_knee += 1;
            for (s, h) in sdr_rgb.iter().zip(hdr_rgb) {
                assert!(
                    (linear_to_srgb_byte(h) - i32::from(*s)).abs() <= 1,
                    "below the knee HDR is SDR: {h} (sRGB {}) vs {s} ({})",
                    linear_to_srgb_byte(h),
                    srgb_to_linear(*s)
                );
            }
        }
    }
    eprintln!(
        "{below_knee} texels below the knee, {above_white} above SDR white, brightest {brightest}"
    );
    assert!(
        below_knee > texels / 2,
        "most of a dark stage is below the knee"
    );
    assert!(above_white > 0, "the beam's core goes above SDR white");
}
