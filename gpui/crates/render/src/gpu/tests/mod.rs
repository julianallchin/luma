//! Whole-frame tests of the pass chain, rendered on the shared device, and
//! the frames they draw.

mod haze;
mod quality;
mod resize;
mod shadows;

use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::colour::hex_srgb;
use crate::frame::{Camera, DirectionalLight, Draw, FixtureCone, Frame, MaterialTextures};
use crate::overlay::{Overlay, OverlayDepth};
use crate::scene_desc::DebugView;
use luma_scene::mesh::{Material, MeshData, Vertex};

use super::{Channels, Renderer};

#[test]
fn profiled_capture_preserves_live_pixels_through_shadow_updates() -> anyhow::Result<()> {
    let mut profiled = Renderer::new_profiled()?;
    let mut live = Renderer::new()?;
    let mut frame = fixture_surface_frame(1);
    frame.geometry_shadows = true;
    frame.haze_density = 0.3;
    frame.haze_steps = 8;
    frame.haze_resolution = 1.0;
    let mut pixels = Vec::new();
    for (step, size) in [[96, 72], [96, 72], [103, 77], [103, 77]]
        .into_iter()
        .enumerate()
    {
        if step > 0 {
            frame.fixture_cones[0].position.x += 0.01;
        }
        let timing = profiled.profile_live_into(&frame, size[0], size[1], 2, &mut pixels)?;
        let expected = live.render_next(&frame, size[0], size[1], 2)?;
        assert_eq!(pixels, expected, "profiled pixels differ at update {step}");
        assert_eq!(pixels.len(), (size[0] * size[1] * 4) as usize);
        assert!(timing.gpu_total_ms.is_finite() && timing.gpu_total_ms >= 0.0);
        let cpu = timing.cpu;
        assert_eq!(
            cpu.prepare + cpu.clusters + cpu.upload + cpu.targets + cpu.encode,
            cpu.total,
        );
        let shadows = cpu.fixture_shadows;
        assert!(shadows.globals <= cpu.upload);
        assert!(
            shadows.cull
                + shadows.buckets
                + shadows.resources
                + shadows.maps
                + shadows.hierarchy
                + cpu.submission.staging
                + cpu.submission.finish
                + cpu.submission.submit
                <= cpu.encode,
        );
        assert!(profiled.shadow_stats().redrawn_maps > 0);
        assert_eq!(profiled.shadow_stats(), live.shadow_stats());
    }
    Ok(())
}

#[test]
fn surface_profiling_keeps_live_targets_and_reports_every_frame() -> anyhow::Result<()> {
    let mut renderer = Renderer::new_profiled()?;
    let mut frame = fixture_surface_frame(1);
    frame.geometry_shadows = true;
    frame.fixture_shadows = true;
    frame.haze_density = 0.3;
    frame.haze_resolution = 1.0;
    for size in [[96, 72], [96, 72], [103, 77], [103, 77]] {
        let timing = renderer.profile_live_surface(&frame, size[0], size[1], 2)?;
        assert!(timing.gpu_total_ms.is_finite() && timing.gpu_total_ms > 0.0);
        let targets = renderer.targets.as_ref().expect("rendered targets");
        assert!(targets.destination == super::Destination::Compositor(super::DisplayRange::Sdr));
        assert_eq!([targets.width, targets.height], size);
        #[cfg(target_os = "macos")]
        assert!(matches!(
            targets.presentation(0),
            super::targets::PresentationTarget::Shared(_)
        ));
    }
    Ok(())
}

#[test]
fn post_agx_overlay_keeps_authored_srgb_across_scene_lighting_and_output_formats(
) -> anyhow::Result<()> {
    const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
    let mut renderer = Renderer::new()?;
    let dark = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::ZERO);
    let lit = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::splat(40.0));

    let mut rgba_dark = Vec::new();
    renderer.render_into(&dark, 96, 96, 1, Channels::Rgba, &mut rgba_dark)?;
    let mut rgba_lit = Vec::new();
    renderer.render_into(&lit, 96, 96, 1, Channels::Rgba, &mut rgba_lit)?;
    let mut bgra_lit = Vec::new();
    renderer.render_into(&lit, 96, 96, 1, Channels::Bgra, &mut bgra_lit)?;

    assert_eq!(center_rgb(&rgba_dark, Channels::Rgba), AUTHORED);
    assert_eq!(center_rgb(&rgba_lit, Channels::Rgba), AUTHORED);
    assert_eq!(center_rgb(&bgra_lit, Channels::Bgra), AUTHORED);
    let bgra_offset = center_offset(96, 96);
    assert_eq!(
        &bgra_lit[bgra_offset..bgra_offset + 4],
        &[AUTHORED[2], AUTHORED[1], AUTHORED[0], 255]
    );
    Ok(())
}

#[test]
fn a_ten_bit_frame_is_the_eight_bit_picture_with_finer_steps() -> anyhow::Result<()> {
    const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
    // Red in the low ten bits, then green, blue and two bits of alpha.
    let unpack = |pixel: &[u8]| {
        let word = u32::from_le_bytes(pixel.try_into().unwrap());
        [
            word & 1023,
            (word >> 10) & 1023,
            (word >> 20) & 1023,
            word >> 30,
        ]
    };
    let mut renderer = Renderer::new()?;
    // The composite pass writes the neutral look; the post chain the stage one.
    for look in [
        crate::scene_desc::Look::NEUTRAL,
        crate::scene_desc::Look::STAGE,
    ] {
        let mut frame = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::splat(0.02));
        frame.look = look;
        let mut rgba = Vec::new();
        renderer.render_into(&frame, 96, 96, 1, Channels::Rgba, &mut rgba)?;
        let mut rgb10 = Vec::new();
        renderer.render_into(&frame, 96, 96, 1, Channels::Rgb10, &mut rgb10)?;
        assert_eq!(rgb10.len(), rgba.len());

        let mut off_grid = 0;
        let mut worst = 0.0_f32;
        for (ten, eight) in rgb10.chunks_exact(4).zip(rgba.chunks_exact(4)) {
            let [r, g, b, a] = unpack(ten);
            assert_eq!(a, 3, "opaque");
            for (code, byte) in [r, g, b].into_iter().zip(eight) {
                let as_eight = code as f32 * 255.0 / 1023.0;
                worst = worst.max((as_eight - f32::from(*byte)).abs());
                off_grid += usize::from((as_eight - as_eight.round()).abs() > 0.1);
            }
        }
        let offset = center_offset(96, 96);
        let [r, g, b, _] = unpack(&rgb10[offset..offset + 4]);
        let center = [r, g, b].map(|code| (code as f32 * 255.0 / 1023.0).round() as u8);
        assert_eq!(center, AUTHORED, "the overlay's colour, sRGB-encoded");
        // Each frame within its own dither of the other: one 8-bit code
        // for the 8-bit frame, a quarter of one for the 10-bit frame.
        assert!(worst <= 2.0, "the 10-bit frame strays {worst} 8-bit codes");
        assert!(off_grid > 0, "every 10-bit value sits on the 8-bit grid");
    }
    Ok(())
}

#[test]
fn post_agx_overlays_load_reverse_z_depth_while_free_gizmos_ignore_it() -> anyhow::Result<()> {
    const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
    let mut renderer = Renderer::new()?;
    let occluder_only = overlay_test_frame(true, OverlayDepth::Tested, 1.0, Vec3::ZERO);
    let mut without_overlay = overlay_test_frame(true, OverlayDepth::Tested, 1.0, Vec3::ZERO);
    without_overlay.overlays.clear();

    let mut background = Vec::new();
    renderer.render_into(&without_overlay, 96, 96, 1, Channels::Rgba, &mut background)?;
    let mut tested_behind = Vec::new();
    renderer.render_into(
        &occluder_only,
        96,
        96,
        1,
        Channels::Rgba,
        &mut tested_behind,
    )?;
    assert_eq!(
        center_rgb(&tested_behind, Channels::Rgba),
        center_rgb(&background, Channels::Rgba),
        "a tested cage behind opaque geometry must remain occluded"
    );

    let tested_front = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::ZERO);
    let mut front = Vec::new();
    renderer.render_into(&tested_front, 96, 96, 1, Channels::Rgba, &mut front)?;
    assert_eq!(center_rgb(&front, Channels::Rgba), AUTHORED);

    let free_behind = overlay_test_frame(true, OverlayDepth::Free, 1.0, Vec3::ZERO);
    let mut free = Vec::new();
    renderer.render_into(&free_behind, 96, 96, 1, Channels::Rgba, &mut free)?;
    assert_eq!(
        center_rgb(&free, Channels::Rgba),
        AUTHORED,
        "a free gizmo must use Always and paint over opaque geometry"
    );
    Ok(())
}

#[test]
fn clustered_fixture_light_is_local_and_surface_toggle_is_independent() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(1);
    frame.fixture_cones[0].color = Vec3::new(1.0, 0.08, 0.02);
    let lit = renderer.render(&frame, 160, 120, 1)?;
    frame.fixture_surface_lighting = false;
    let dark = renderer.render(&frame, 160, 120, 1)?;

    let central = region_mean(&lit, 160, 52..108, 38..94);
    let central_dark = region_mean(&dark, 160, 52..108, 38..94);
    let corner = region_mean(&lit, 160, 0..24, 88..120);
    assert!(
        central > central_dark + 2.0,
        "fixture surface light did not contribute locally: {central:.2} vs {central_dark:.2}"
    );
    assert!(
        central > corner + 1.0,
        "finite cone illuminated the whole surface: center {central:.2}, corner {corner:.2}"
    );
    Ok(())
}

/// The case the relative assertions above cannot see: a stock rig's
/// throw, at magnitude. A real moving head sits ~6 m over a dark floor
/// with its dimmer at full — `intensity` ≈ 1, not the hot 3.0 the
/// local-contribution case uses — and its pool must be plainly *visible*,
/// not merely nonzero. This is the shared-radiance-scale contract: the
/// surface pass and the haze march read the same cones, and a surface
/// term that loses the beam gain reads as "bright shaft over a black
/// pool" while every relative assertion still passes.
#[test]
fn fixture_pool_is_visible_at_a_real_rig_throw() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(1);
    frame.fixture_cones[0].position = Vec3::new(0.0, 0.0, 6.0);
    frame.fixture_cones[0].range = 10.0;
    frame.fixture_cones[0].intensity = 1.0;
    frame.draws[0].material.base_color = Vec3::splat(0.2);
    let lit = renderer.render(&frame, 160, 120, 1)?;
    frame.fixture_surface_lighting = false;
    let dark = renderer.render(&frame, 160, 120, 1)?;
    let central = region_mean(&lit, 160, 52..108, 38..94);
    let central_dark = region_mean(&dark, 160, 52..108, 38..94);
    assert!(
        central > central_dark + 25.0,
        "a full-dimmer pool at a 6 m throw must be plainly visible: \
         {central:.2} vs {central_dark:.2}"
    );
    Ok(())
}

#[test]
fn clustered_surface_uses_shared_cone_and_gobo_photometry() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let open = fixture_surface_frame(1);
    let open_pixels = renderer.render(&open, 160, 120, 1)?;
    let mut spokes = fixture_surface_frame(1);
    spokes.fixture_cones[0].gobo = 1;
    spokes.fixture_cones[0].gobo_rotation = 0.31;
    let spokes_pixels = renderer.render(&spokes, 160, 120, 1)?;
    let mut outside = fixture_surface_frame(1);
    outside.fixture_cones[0].direction = Vec3::new(0.0, 1.0, -0.08).normalize();
    let outside_pixels = renderer.render(&outside, 160, 120, 1)?;

    let open_energy = region_mean(&open_pixels, 160, 42..118, 32..104);
    let spokes_energy = region_mean(&spokes_pixels, 160, 42..118, 32..104);
    let outside_energy = region_mean(&outside_pixels, 160, 42..118, 32..104);
    assert!(
        open_energy > spokes_energy + 0.5,
        "shared gobo did not remove surface energy: {open_energy:.2} vs {spokes_energy:.2}"
    );
    assert!(
        open_energy > outside_energy + 1.0,
        "shared cone cutoff did not reject an off-axis surface: {open_energy:.2} vs {outside_energy:.2}"
    );
    Ok(())
}

/// The light index's fixed-width mask has no growth path, so the three cone
/// counts must all render.
#[test]
fn clustered_surface_renders_at_32_128_and_512_cones() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    for count in [32, 128, 512] {
        let frame = fixture_surface_frame(count);
        renderer.render(&frame, 320, 180, 1)?;
    }
    Ok(())
}

#[test]
fn cluster_occupancy_debug_is_topology_only() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(32);
    frame.cluster_debug = true;
    frame.fixture_surface_lighting = false;
    let first = renderer.render(&frame, 160, 120, 1)?;
    for light in &mut frame.fixture_cones {
        light.color = Vec3::splat(0.05);
        light.intensity = 0.01;
        light.gobo = 2;
    }
    let shading_only = renderer.render(&frame, 160, 120, 1)?;
    assert_eq!(first, shading_only);
    assert!(first.chunks_exact(4).any(|pixel| pixel[2] > 32));
    // A dark cone is not a light at all: it leaves the index.
    for light in &mut frame.fixture_cones {
        light.color = Vec3::ZERO;
    }
    let dark = renderer.render(&frame, 160, 120, 1)?;
    assert_ne!(dark, first);
    Ok(())
}

#[test]
fn fragment_counts_follow_unprofiled_frames_and_resizes() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    assert_eq!(renderer.fragment_stats()?, None);
    let frame = fixture_surface_frame(64);
    renderer.render(&frame, 127, 97, 1)?;
    let first = renderer.fragment_stats()?.expect("visible receiver");
    assert!(first.0 > 0 && first.0 <= 127 * 97 && first.1 > first.0);
    assert_eq!(
        renderer.fragment_stats()?,
        Some(first),
        "counter accumulated across requests"
    );
    renderer.render(&frame, 64, 48, 1)?;
    let resized = renderer.fragment_stats()?.expect("resized receiver");
    assert!(resized.0 > 0 && resized.0 <= 64 * 48 && resized.0 < first.0);
    assert!(resized.1 > resized.0 && resized.1 < first.1);
    Ok(())
}

#[test]
fn surface_depth_culling_preserves_subpixel_and_coplanar_receivers() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    renderer.render(&fixture_surface_frame(16), 127, 97, 1)?;
    assert!(renderer
        .targets
        .as_ref()
        .unwrap()
        .msaa_surface_depth
        .is_none());
    let mut frame = fixture_surface_frame(64);
    let receiver = || Draw {
        mesh: 0,
        model: Mat4::IDENTITY,
        material: Material::default(),
        textures: MaterialTextures::default(),
        editor_object: None,
    };
    for (width, x, tilt) in [(0.002, 0.41, 0.4), (0.0006, -0.27, -0.2)] {
        let mut strip = receiver();
        strip.model = Mat4::from_scale_rotation_translation(
            Vec3::new(width, 0.35, 1.0),
            glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)
                * glam::Quat::from_rotation_y(tilt),
            Vec3::new(x, -0.8, 1.2),
        );
        strip.material.base_color = Vec3::new(0.05, 0.45, 0.8);
        frame.draws.push(strip);
    }
    let mut panel = receiver();
    panel.model = Mat4::from_scale(Vec3::new(0.2, 0.2, 1.0));
    panel.material.base_color = Vec3::new(0.8, 0.1, 0.02);
    panel.material.metallic = 0.7;
    frame.draws.push(panel);
    // A cable over the ground: a thin strip halfway along the first
    // camera's view ray, so its tiles hold depths ~3.7 m and ~7.5 m and
    // the surface refinement splits them into two buckets.
    let mut cable = receiver();
    cable.model = Mat4::from_scale_rotation_translation(
        Vec3::new(0.004, 0.6, 1.0),
        glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2) * glam::Quat::from_rotation_y(0.3),
        Vec3::new(0.05, -3.0, 2.4),
    );
    cable.material.base_color = Vec3::new(0.9, 0.8, 0.2);
    frame.draws.push(cable);
    for (eye, target) in [
        (Vec3::new(0.0, -6.0, 4.5), Vec3::ZERO),
        (Vec3::new(4.0, -4.0, 0.12), Vec3::new(0.0, 0.0, 0.001)),
        (Vec3::new(-2.0, -1.1, 1.3), Vec3::new(0.0, 0.0, 1.0)),
    ] {
        for shift in [0.0, 0.013, 0.029] {
            frame.camera.eye = eye + Vec3::X * shift;
            frame.camera.target = target + Vec3::X * shift;
            renderer.surface_depth_cull = false;
            let reference = renderer.render(&frame, 127, 97, 1)?;
            renderer.surface_depth_cull = true;
            let culled = renderer.render(&frame, 127, 97, 1)?;
            assert_eq!(
                reference, culled,
                "changed subpixel/coplanar receiver at {eye:?}, shift {shift}"
            );
        }
    }
    for (width, height) in [(129, 99), (64, 48)] {
        renderer.surface_depth_cull = false;
        let reference = renderer.render(&frame, width, height, 1)?;
        renderer.surface_depth_cull = true;
        assert_eq!(
            reference,
            renderer.render(&frame, width, height, 1)?,
            "resize {width}x{height}"
        );
    }
    // Verify this exercised real pruning, not an empty or unused third plane.
    let masks = renderer.light_index.bindings().tile_masks;
    let readback = renderer.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("surface-mask-test"),
        size: masks.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = renderer
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(&masks, 0, &readback, 0, masks.size());
    renderer.gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()??;
    let mapped = readback.slice(..).get_mapped_range()?;
    let values: &[u32] = bytemuck::cast_slice(&mapped);
    // The planes of the last frame's grid, 64 by 48: the buffer keeps the
    // room of the larger frames before it.
    let tiles = |pixels: u32| pixels.div_ceil(crate::light_index::TILE_SIZE) as usize;
    let plane = tiles(64) * tiles(48) * crate::light_index::MASK_WORDS;
    let full = &values[..plane];
    let surface = &values[2 * plane..3 * plane];
    let surface_b = &values[3 * plane..];
    assert!(full.iter().zip(surface).all(|(a, b)| b & !a == 0));
    assert!(full.iter().zip(surface_b).all(|(a, b)| b & !a == 0));
    let original: u32 = full.iter().map(|b| b.count_ones()).sum();
    let refined: u32 = surface.iter().map(|b| b.count_ones()).sum();
    assert!(
        refined > 0 && refined < original,
        "surface plane did not prune: {original} -> {refined}"
    );
    // The straddling tiles (cable over ground) must have been split into
    // two buckets.
    let splits = renderer.light_index.bindings().surface_splits;
    let readback = renderer.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("surface-split-test"),
        size: splits.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = renderer
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(&splits, 0, &readback, 0, splits.size());
    renderer.gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()??;
    let split_mapped = readback.slice(..).get_mapped_range()?;
    let split_depths: &[f32] = bytemuck::cast_slice(&split_mapped);
    let split_tiles = split_depths.iter().filter(|s| **s < 1e29).count();
    let refined_b: u32 = surface_b.iter().map(|b| b.count_ones()).sum();
    assert!(
        split_tiles > 0 && refined_b > 0,
        "no straddling tile was split: {split_tiles} tiles, {refined_b} bucket-B bits"
    );
    assert!(
        refined_b < refined,
        "bucket B did not prune below bucket A: {refined} vs {refined_b}"
    );
    Ok(())
}

fn fixture_surface_frame(count: usize) -> Frame {
    let vertex = |position: [f32; 3]| Vertex {
        position,
        normal: [0.0, 0.0, 1.0],
        uv: [0.0; 2],
        tangent: [1.0, 0.0, 0.0, 1.0],
    };
    let mesh = MeshData {
        key: "::clustered-surface-floor".into(),
        vertices: vec![
            vertex([-5.0, -5.0, 0.0]),
            vertex([5.0, -5.0, 0.0]),
            vertex([5.0, 5.0, 0.0]),
            vertex([-5.0, 5.0, 0.0]),
        ]
        .into(),
        indices: Arc::from([0, 1, 2, 0, 2, 3]),
    };
    let fixture_cones = (0..count)
        .map(|index| {
            let column = (index % 32) as f32;
            let row = (index / 32) as f32;
            FixtureCone {
                position: Vec3::new((column - 15.5) * 0.08, (row - 7.5) * 0.08, 3.0),
                range: 5.0,
                direction: Vec3::NEG_Z,
                cos_beam: 0.96,
                color: Vec3::ONE,
                intensity: 3.0 / count.max(1) as f32,
                cos_field: 0.88,
                gobo: 0,
                gobo_rotation: 0.0,
                haze_gain: 1.0,
                lens: luma_scene::luminaire::Lens::POINT,
            }
        })
        .collect();
    Frame {
        meshes: vec![mesh],
        images: Vec::new(),
        draws: vec![Draw {
            mesh: 0,
            model: Mat4::IDENTITY,
            material: Material {
                base_color: Vec3::splat(0.65),
                roughness: 0.55,
                ..Material::default()
            },
            textures: MaterialTextures::default(),
            editor_object: None,
        }],
        transparent: Vec::new(),
        gizmo_pivot: None,
        overlays: Vec::new(),
        floor: None,
        fixture_cones,
        panels: Vec::new(),
        lasers: Vec::new(),
        fixture_shadow_capacity_hint: 0,
        fixture_surface_lighting: true,
        fixture_shadows: true,
        geometry_shadows: false,
        cluster_debug: false,
        probes: crate::scene_desc::ProbeView::default(),
        clear_color: Vec3::ZERO,
        room: None,
        ambient: Vec3::splat(0.002),
        environment: None,
        directional: None,
        sky: None,
        haze_density: 0.0,
        haze_appearance: Default::default(),
        haze_bounds: luma_scene::Aabb::new(Vec3::splat(-32.0), Vec3::splat(32.0)),
        haze_steps: 1,
        haze_resolution: 0.5,
        quality: Default::default(),
        time: 0.0,
        debug_view: DebugView::Pbr,
        look: crate::scene_desc::Look::NEUTRAL,
        camera: Camera {
            eye: Vec3::new(0.0, -6.0, 4.5),
            target: Vec3::ZERO,
            fov_y_deg: 48.0,
        },
    }
}

fn region_mean(
    pixels: &[u8],
    width: usize,
    xs: std::ops::Range<usize>,
    ys: std::ops::Range<usize>,
) -> f64 {
    let mut sum = 0_u64;
    let mut samples = 0_u64;
    for y in ys {
        for x in xs.clone() {
            let offset = (y * width + x) * 4;
            sum += u64::from(pixels[offset])
                + u64::from(pixels[offset + 1])
                + u64::from(pixels[offset + 2]);
            samples += 3;
        }
    }
    sum as f64 / samples as f64
}

fn overlay_test_frame(
    occluder: bool,
    depth: OverlayDepth,
    overlay_y: f32,
    scene_radiance: Vec3,
) -> Frame {
    let vertex = |position: [f32; 3]| Vertex {
        position,
        normal: [0.0, -1.0, 0.0],
        uv: [0.0; 2],
        tangent: [1.0, 0.0, 0.0, 1.0],
    };
    let mesh = MeshData {
        key: "::post-agx-overlay-test".into(),
        vertices: vec![
            vertex([-2.0, 0.0, -2.0]),
            vertex([2.0, 0.0, -2.0]),
            vertex([0.0, 0.0, 2.0]),
        ]
        .into(),
        indices: Arc::from([0, 1, 2]),
    };
    let draws = occluder
        .then(|| Draw {
            mesh: 0,
            model: Mat4::IDENTITY,
            material: Material {
                base_color: Vec3::splat(0.35),
                roughness: 1.0,
                ..Material::default()
            },
            textures: MaterialTextures::default(),
            editor_object: None,
        })
        .into_iter()
        .collect();
    Frame {
        meshes: vec![mesh],
        images: Vec::new(),
        draws,
        transparent: Vec::new(),
        gizmo_pivot: None,
        floor: None,
        overlays: vec![Overlay {
            mesh: 0,
            model: Mat4::from_translation(Vec3::Y * overlay_y),
            lines: false,
            color: hex_srgb(0x33_99_e6),
            opacity: 1.0,
            depth,
        }],
        fixture_cones: Vec::new(),
        panels: Vec::new(),
        lasers: Vec::new(),
        fixture_shadow_capacity_hint: 0,
        fixture_surface_lighting: false,
        fixture_shadows: true,
        geometry_shadows: false,
        cluster_debug: false,
        probes: crate::scene_desc::ProbeView::default(),
        environment: None,
        clear_color: scene_radiance,
        room: None,
        ambient: scene_radiance,
        directional: Some(DirectionalLight {
            direction: Vec3::new(0.0, -1.0, 1.0).normalize(),
            radiance: scene_radiance,
            shadow_eye: Vec3::new(0.0, -4.0, 5.0),
            shadows: false,
            shadow_softness: 1.0,
        }),
        sky: None,
        haze_density: 0.0,
        haze_appearance: Default::default(),
        haze_bounds: luma_scene::Aabb::new(Vec3::splat(-32.0), Vec3::splat(32.0)),
        haze_steps: 1,
        haze_resolution: 1.0,
        quality: Default::default(),
        time: 0.0,
        debug_view: DebugView::Pbr,
        look: crate::scene_desc::Look::NEUTRAL,
        camera: Camera {
            eye: Vec3::new(0.0, -5.0, 0.0),
            target: Vec3::ZERO,
            fov_y_deg: 48.0,
        },
    }
}

fn center_offset(width: usize, height: usize) -> usize {
    ((height / 2) * width + width / 2) * 4
}

fn center_rgb(pixels: &[u8], channels: Channels) -> [u8; 3] {
    let offset = center_offset(96, 96);
    match channels {
        Channels::Rgba => [pixels[offset], pixels[offset + 1], pixels[offset + 2]],
        Channels::Bgra => [pixels[offset + 2], pixels[offset + 1], pixels[offset]],
        Channels::Hdr | Channels::Rgb10 => unreachable!("read back only as RGBA8 or BGRA8"),
    }
}
