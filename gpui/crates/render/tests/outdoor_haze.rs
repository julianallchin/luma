//! Outdoor venue haze must scatter the light it removes from the environment.
use std::{collections::BTreeMap, path::PathBuf};

use glam::{Mat4, Vec3};
use luma_render::{
    assets::{Library, Material, Vertex},
    build_frame_with,
    frame::{Draw, FixtureCone, MaterialTextures, MeshData},
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene, VenueEnvironment},
    Frame, Renderer,
};

const WIDTH: u32 = 400;
const HEIGHT: u32 = 300;

fn venue(elevation: f32) -> Frame {
    let mut render = RenderSettings::room(VenueEnvironment::outdoor(elevation), 45.0, 1.0);
    render.show_floor = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.haze.density = 0.65;
    render.haze.appearance.cloudiness = 0.7;
    render.haze.appearance.wind_speed = 0.0;
    let scene = Scene {
        id: "outdoor-unlit-haze".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, 30.0, 120.0],
            target: [0.0, 30.0, 0.0],
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
    let mut frame = build_frame_with(
        &scene,
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut Library::default(),
    )
    .unwrap();
    // Former venue bounds are retained to catch accidental outdoor clipping.
    // No fixture is on: the sun and sky alone illuminate the haze.
    frame.haze_bounds =
        luma_scene::Aabb::new(Vec3::new(-50.0, -50.0, 0.0), Vec3::new(50.0, 50.0, 70.0));
    frame
}

fn centre_brightness(pixels: &[u8]) -> u32 {
    let mut sum = 0;
    for y in 80..100 {
        for x in 190..210 {
            let offset = ((y * WIDTH + x) * 4) as usize;
            sum += pixels[offset..offset + 3]
                .iter()
                .map(|&c| u32::from(c))
                .sum::<u32>();
        }
    }
    sum / (20 * 20 * 3)
}

fn capture(name: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_OUTDOOR_HAZE_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
    }
}

#[test]
fn daylight_and_twilight_illuminate_haze_without_stage_lights() {
    let mut renderer = Renderer::new().unwrap();
    for (label, elevation) in [("day", 30.0), ("twilight", -4.0)] {
        let mut frame = venue(elevation);
        frame.haze_density = 0.0;
        let clear = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("{label}-clear"), &clear);
        frame.haze_density = 0.65;
        let hazy = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("{label}-haze"), &hazy);
        frame.debug_view = DebugView::VolumetricAccumulation;
        let scattering = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("{label}-scattering"), &scattering);
        assert!(
            centre_brightness(&scattering) > centre_brightness(&clear) / 8,
            "{label}: outdoor haze has no environment illumination"
        );
        assert!(
            centre_brightness(&hazy) * 2 > centre_brightness(&clear),
            "{label}: venue haze became black smoke: {} vs {}",
            centre_brightness(&hazy),
            centre_brightness(&clear)
        );
        assert_ne!(hazy, clear, "haze must still affect transport");
    }
}

#[test]
fn haze_has_no_light_when_the_environment_and_fixtures_are_off() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = venue(30.0);
    frame.sky = None;
    frame.environment = None;
    frame.clear_color = Vec3::ZERO;
    frame.debug_view = DebugView::VolumetricAccumulation;
    let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert!(pixels.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]));
}

#[test]
fn daylight_scattering_stops_at_opaque_geometry() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = venue(30.0);
    let mesh = frame.meshes.len();
    frame.meshes.push(MeshData {
        key: "::outdoor-haze-black-card".into(),
        vertices: [
            [-100.0, 0.0, -100.0],
            [100.0, 0.0, -100.0],
            [100.0, 0.0, 100.0],
            [-100.0, 0.0, 100.0],
        ]
        .map(|position| Vertex {
            position,
            normal: [0.0, -1.0, 0.0],
            uv: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
        })
        .into(),
        indices: [0, 1, 2, 0, 2, 3].into(),
    });
    frame.draws.push(Draw {
        mesh,
        model: Mat4::from_translation(Vec3::new(0.0, -119.8, 0.0)),
        material: Material {
            base_color: Vec3::ZERO,
            metallic: 1.0,
            ..Default::default()
        },
        textures: MaterialTextures::default(),
        editor_object: None,
    });
    let front = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    capture("near-black-surface", &front);
    assert!(
        centre_brightness(&front) < 15,
        "haze behind an opaque card leaked through it: {}",
        centre_brightness(&front)
    );
    frame.draws.last_mut().unwrap().model = Mat4::from_translation(Vec3::new(0.0, 30.0, 0.0));
    let behind = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert!(
        centre_brightness(&behind) > 30,
        "a black surface must receive light scattered in front of it"
    );
    capture("black-surface-through-daylight-haze", &behind);
}

#[test]
fn outdoor_haze_has_no_venue_box_and_is_uniform_across_the_horizon() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = venue(30.0);
    frame.haze_appearance.cloudiness = 0.0;
    let original = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    capture("global-haze", &original);
    frame.haze_bounds = luma_scene::Aabb::new(Vec3::splat(2000.0), Vec3::splat(2010.0));
    assert!(
        original == renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap(),
        "moving a venue boundary changed the outdoor atmosphere"
    );
    frame.camera.eye.x += 5000.0;
    frame.camera.target.x += 5000.0;
    let translated = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    let largest_delta = original
        .iter()
        .zip(&translated)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(
        largest_delta <= 2,
        "the atmosphere ended outside the venue: max channel delta {largest_delta}"
    );
    // Away from the solar disc, a level view has no cloud-shaped island or
    // horizontal density edge. Compare the two outer thirds of the same sky row.
    let row = 100;
    let luma = |x| {
        let offset = ((row * WIDTH + x) * 4) as usize;
        original[offset..offset + 3]
            .iter()
            .map(|&v| u32::from(v))
            .sum::<u32>()
    };
    assert!(
        luma(60).abs_diff(luma(WIDTH - 60)) < 70,
        "a localized patch survived in global haze"
    );
    frame.camera.eye.z += 500.0;
    frame.camera.target.z += 500.0;
    let high_hazy = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    frame.haze_density = 0.0;
    let high_clear = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert!(
        centre_brightness(&high_hazy).abs_diff(centre_brightness(&high_clear)) < 3,
        "outdoor haze should thin smoothly with altitude"
    );
}

#[test]
#[ignore = "GPU benchmark; run explicitly on an otherwise idle device"]
fn profile_global_haze() {
    let mut renderer = Renderer::new_profiled().unwrap();
    let mut frame = venue(30.0);
    frame.haze_density = 0.24;
    println!(
        "GPU: {}",
        luma_render::device::DeviceContext::shared()
            .unwrap()
            .adapter_info()
            .name
    );
    for (label, offset, density) in [
        ("clear", 0.0, 0.0),
        ("global", 0.0, 0.24),
        ("global-5km-away", 5000.0, 0.24),
    ] {
        frame.camera.eye.x = offset;
        frame.camera.target.x = offset;
        frame.haze_density = density;
        for _ in 0..8 {
            renderer.profile_live_frame(&frame, 1280, 720, 1).unwrap();
        }
        let mut samples: Vec<_> = (0..40)
            .map(|_| {
                renderer
                    .profile_live_frame(&frame, 1280, 720, 1)
                    .unwrap()
                    .gpu_composite_ms
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        println!(
            "{label}: 1280x720 composite median {:.3} ms",
            samples[samples.len() / 2]
        );
    }
}

#[test]
fn fixture_lighting_grid_includes_air_before_its_work_bounds() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = venue(30.0);
    frame.haze_density = 0.24;
    frame.haze_appearance.cloudiness = 0.0;
    frame.geometry_shadows = true;
    frame.fixture_shadows = true;
    frame.debug_view = DebugView::VolumetricAccumulation;
    let unlit = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    frame.fixture_cones.push(FixtureCone {
        position: Vec3::new(0.0, 0.0, 30.0),
        range: 24.0,
        direction: -Vec3::Y,
        cos_beam: 0.5,
        color: Vec3::new(1.0, 0.1, 0.7),
        intensity: 1.0,
        cos_field: 0.1,
        wash: 1.0,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
    });
    let live = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    let reference = renderer
        .render_reference(&frame, WIDTH, HEIGHT, 32)
        .unwrap();
    capture("global-fixture-haze", &live);
    let energy = |pixels: &[u8]| -> f64 {
        pixels
            .chunks_exact(4)
            .zip(unlit.chunks_exact(4))
            .map(|(a, b)| {
                (0..3)
                    .map(|c| f64::from(a[c].saturating_sub(b[c])))
                    .sum::<f64>()
            })
            .sum()
    };
    let a = energy(&live);
    let b = energy(&reference);
    assert!(b > 500.0, "the fixture must illuminate visible haze");
    assert!(
        (a - b).abs() / b < 0.2,
        "lighting grid ignored camera extinction before its bounds: live {a}, reference {b}"
    );
}
