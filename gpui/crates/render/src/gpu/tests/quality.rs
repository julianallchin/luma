//! What each quality level draws a beam with: Medium marches the haze, Low
//! draws it in closed form and marches nothing. And what Low leaves out:
//! fixture shadows.

use glam::{Mat4, Vec3};

use crate::frame::{Camera, Draw, Frame, MaterialTextures};
use crate::gpu::Renderer;
use crate::scene_desc::Quality;
use luma_scene::mesh::Material;

use super::{fixture_surface_frame, region_mean};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;

/// One warm beam falling from above the floor through haze, seen from the
/// side. The floor takes no fixture light, so the only light between the
/// beam's columns and the floor behind them is the haze's.
fn beam_frame(quality: Quality, haze: f32) -> Frame {
    let mut frame = fixture_surface_frame(1);
    frame.fixture_surface_lighting = false;
    frame.geometry_shadows = true;
    frame.haze_density = haze;
    (frame.haze_steps, frame.haze_resolution) = quality.limit_haze(8, crate::LIVE_HAZE_RESOLUTION);
    frame.quality = quality;
    let light = &mut frame.fixture_cones[0];
    light.position = Vec3::new(0.0, 0.0, 3.0);
    light.intensity = 3.0;
    light.color = Vec3::new(1.0, 0.35, 0.08);
    frame
}

/// The mean of each channel over `xs` × `ys`.
fn channel_means(
    pixels: &[u8],
    xs: std::ops::Range<usize>,
    ys: std::ops::Range<usize>,
) -> [f64; 3] {
    let mut sum = [0_u64; 3];
    let mut count = 0_u64;
    for y in ys {
        for x in xs.clone() {
            let offset = (y * WIDTH as usize + x) * 4;
            for (channel, total) in sum.iter_mut().enumerate() {
                *total += u64::from(pixels[offset + channel]);
            }
            count += 1;
        }
    }
    sum.map(|total| total as f64 / count as f64)
}

/// The beam's middle: the centre columns between the lamp, near the top of
/// the frame, and the floor at its centre.
fn on_beam() -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    (w / 2 - 3..w / 2 + 3, h / 5..h * 2 / 5)
}

/// The same rows, well outside the cone.
fn off_beam() -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    (w / 16..w / 16 + 6, h / 5..h * 2 / 5)
}

fn brightness(pixels: &[u8], (xs, ys): (std::ops::Range<usize>, std::ops::Range<usize>)) -> f64 {
    region_mean(pixels, WIDTH as usize, xs, ys)
}

#[test]
fn low_quality_draws_beams_in_haze_without_marching() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;

    let marched = renderer.render(&beam_frame(Quality::Medium, 0.6), WIDTH, HEIGHT, 1)?;
    assert!(
        renderer.fog_block_stats()?.is_some(),
        "Medium marches the haze through the far-field grid"
    );
    assert!(renderer.haze_glow.on, "Medium fills the haze glow");

    let analytic = renderer.render(&beam_frame(Quality::Low, 0.6), WIDTH, HEIGHT, 1)?;
    assert!(
        renderer.fog_block_stats()?.is_none(),
        "Low ran the far-field fog grid"
    );
    assert!(!renderer.haze_glow.on, "Low filled the haze glow");

    let clear = renderer.render(&beam_frame(Quality::Low, 0.0), WIDTH, HEIGHT, 1)?;

    // The beam is light in the haze and nothing without it.
    let lit = brightness(&analytic, on_beam());
    assert!(
        lit > brightness(&analytic, off_beam()) + 10.0,
        "no beam: {lit} on it, {} beside it",
        brightness(&analytic, off_beam())
    );
    assert!(
        lit > brightness(&clear, on_beam()) + 10.0,
        "the beam is not the haze's"
    );

    // Roughly the marched beam: within a factor of two of its brightness,
    // and the same hue, its channels in the lamp's order.
    let reference = brightness(&marched, on_beam());
    assert!(
        (0.5..2.0).contains(&(lit / reference)),
        "analytic beam {lit} against the marched {reference}"
    );
    for pixels in [&marched, &analytic] {
        let [r, g, b] = channel_means(pixels, on_beam().0, on_beam().1);
        assert!(r > g && g > b, "a warm beam reads {r} {g} {b}");
    }
    Ok(())
}

#[test]
fn analytic_beams_fade_from_the_source() -> anyhow::Result<()> {
    // Nearer the lamp the beam is narrower and brighter: compare the beam's
    // own core high up against low down, at Low and at Medium.
    let mut renderer = Renderer::new()?;
    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let core = |pixels: &[u8], ys: std::ops::Range<usize>| {
        region_mean(pixels, w, w / 2 - 2..w / 2 + 2, ys)
    };
    for quality in [Quality::Medium, Quality::Low] {
        let pixels = renderer.render(&beam_frame(quality, 0.6), WIDTH, HEIGHT, 1)?;
        let near = core(&pixels, h / 10..h / 5);
        let far = core(&pixels, h * 3 / 10..h * 2 / 5);
        assert!(
            near > far,
            "{quality:?}: {near} by the lamp, {far} further down"
        );
    }
    Ok(())
}

/// A lamp over the floor with a dark plate between them, seen from above
/// and to the side, with no haze and no fill: every lit pixel is the lamp's.
fn shadowed_floor_frame(quality: Quality) -> Frame {
    let mut frame = fixture_surface_frame(1);
    frame.fixture_cones[0].position = Vec3::new(0.0, 0.0, 3.0);
    frame.draws.push(Draw {
        mesh: 0,
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
            * Mat4::from_scale(Vec3::splat(0.09)),
        material: Material {
            base_color: Vec3::splat(0.08),
            roughness: 0.8,
            ..Material::default()
        },
        textures: MaterialTextures::default(),
        editor_object: None,
    });
    frame.camera = Camera {
        eye: Vec3::new(3.8, -6.0, 3.2),
        target: Vec3::new(0.0, 0.0, 1.35),
        fov_y_deg: 45.0,
    };
    frame.ambient = Vec3::ZERO;
    frame.geometry_shadows = true;
    frame.quality = quality;
    frame
}

#[test]
fn low_quality_draws_no_fixture_shadows() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let energy = |pixels: &[u8]| {
        pixels
            .chunks_exact(4)
            .map(|pixel| u64::from(pixel[0]) + u64::from(pixel[1]) + u64::from(pixel[2]))
            .sum::<u64>()
    };

    let shadowed = renderer.render(&shadowed_floor_frame(Quality::Medium), WIDTH, HEIGHT, 1)?;
    assert!(
        renderer.shadowed_fixture_count() > 0 && renderer.shadow_stats().redrawn_maps > 0,
        "Medium drew no fixture shadow map"
    );

    // The same renderer, switched down: no map is assigned, drawn or
    // reduced, and the floor under the plate takes the lamp's light.
    let open = renderer.render(&shadowed_floor_frame(Quality::Low), WIDTH, HEIGHT, 1)?;
    let stats = renderer.shadow_stats();
    assert_eq!(
        renderer.shadowed_fixture_count(),
        0,
        "Low kept a fixture shadow map"
    );
    assert_eq!(
        (stats.redrawn_maps, stats.hierarchy_layers),
        (0, 0),
        "Low drew or reduced a fixture shadow map"
    );
    let changed = shadowed
        .chunks_exact(4)
        .zip(open.chunks_exact(4))
        .filter(|(medium, low)| medium[..3] != low[..3])
        .count();
    assert!(
        changed > 100,
        "the plate shadows the floor at Low too: {changed} pixels differ"
    );
    assert!(
        energy(&open) > energy(&shadowed),
        "lighting the floor unshadowed made it darker"
    );
    Ok(())
}
