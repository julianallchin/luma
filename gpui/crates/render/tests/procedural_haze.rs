//! Behavioural checks and optional captures for the shared procedural medium.
use glam::Vec3;
use luma_render::{
    assets::Library,
    build_frame_with,
    frame::FixtureCone,
    scene_desc::{CameraPose, HazeAppearance, RenderSettings, Scene},
    Frame, Renderer,
};
use std::{collections::BTreeMap, path::PathBuf};

fn frame() -> Frame {
    let mut render = RenderSettings::dark_stage(48.0, 1.0);
    render.haze.density = 0.65;
    render.haze.appearance = HazeAppearance {
        cloudiness: 0.0,
        wind_speed: 0.0,
        turbulence: 0.0,
        ..Default::default()
    };
    render.show_grid = false;
    render.show_floor = true;
    render.show_gizmos = false;
    let scene = Scene {
        id: "procedural-haze".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [8.0, 5.0, 10.0],
            target: [0.0, 2.0, 0.0],
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
    build_frame_with(
        &scene,
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut Library::default(),
    )
    .unwrap()
}
fn lights(frame: &mut Frame, count: usize) {
    frame.fixture_shadows = false;
    frame.geometry_shadows = false;
    frame.fixture_cones = (0..count)
        .map(|i| FixtureCone {
            position: Vec3::new(
                (i % 4) as f32 * 2.5 - 3.75,
                (i / 4 % 4) as f32 * 2.5 - 3.75,
                6.0,
            ),
            direction: Vec3::NEG_Z,
            range: 18.0,
            cos_beam: 15_f32.to_radians().cos(),
            cos_field: 30_f32.to_radians().cos(),
            color: Vec3::new(0.15, 0.4, 1.0),
            intensity: 0.1 * (16.0 / count as f32).min(1.0),
            wash: 0.8,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
        })
        .collect();
}
fn brightness(p: &[u8]) -> u64 {
    p.chunks_exact(4)
        .map(|v| u64::from(v[0]) + u64::from(v[1]) + u64::from(v[2]))
        .sum()
}
fn capture(name: &str, pixels: &[u8], width: u32, height: u32) {
    if let Some(dir) = std::env::var_os("LUMA_HAZE_CAPTURE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        let path = PathBuf::from(dir).join(format!("{name}.png"));
        let mut png = png::Encoder::new(std::fs::File::create(path).unwrap(), width, height);
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .unwrap()
            .write_image_data(pixels)
            .unwrap();
    }
}
#[test]
fn unlit_haze_does_not_glow_and_density_attenuates_the_background() {
    let mut renderer = Renderer::new().unwrap();
    let mut f = frame();
    f.draws.clear();
    f.transparent.clear();
    let dark = renderer.render(&f, 160, 120, 1).unwrap();
    assert_eq!(brightness(&dark), 0, "the medium must not emit light");
    f.clear_color = Vec3::splat(0.2);
    f.haze_density = 0.0;
    let clear = renderer.render(&f, 160, 120, 1).unwrap();
    f.haze_density = 0.3;
    let thin = renderer.render(&f, 160, 120, 1).unwrap();
    f.haze_density = 1.2;
    let thick = renderer.render(&f, 160, 120, 1).unwrap();
    assert!(brightness(&clear) > brightness(&thin));
    assert!(brightness(&thin) > brightness(&thick));
    f.haze_appearance.cloudiness = 1.0;
    let cloudy = renderer.render(&f, 160, 120, 1).unwrap();
    assert_ne!(
        cloudy, thick,
        "clouds must affect background attenuation, not just beam brightness"
    );
}
#[test]
fn appearance_changes_are_shared_deterministic_and_wind_moves_the_field() {
    let mut renderer = Renderer::new().unwrap();
    let mut f = frame();
    lights(&mut f, 16);
    let uniform = renderer.render(&f, 640, 480, 4).unwrap();
    capture("uniform", &uniform, 640, 480);
    f.haze_appearance.cloudiness = 1.0;
    f.haze_appearance.cloud_size = 2.0;
    let cloudy = renderer.render(&f, 640, 480, 4).unwrap();
    capture("cloudy", &cloudy, 640, 480);
    assert_ne!(uniform, cloudy);
    assert_eq!(cloudy, renderer.render(&f, 640, 480, 4).unwrap());
    f.time = 3.0;
    assert_eq!(
        cloudy,
        renderer.render(&f, 640, 480, 4).unwrap(),
        "stationary medium changed with time"
    );
    f.haze_appearance.wind_speed = 1.0;
    let drift = renderer.render(&f, 640, 480, 4).unwrap();
    capture("wind", &drift, 640, 480);
    assert_ne!(cloudy, drift);
    f.haze_appearance.turbulence = 1.0;
    let turbulent = renderer.render(&f, 640, 480, 4).unwrap();
    capture("turbulent", &turbulent, 640, 480);
    assert_ne!(drift, turbulent);
    f.haze_appearance.cloudiness = 0.0;
    assert_eq!(
        uniform,
        renderer.render(&f, 640, 480, 4).unwrap(),
        "wind changed a uniform medium"
    );
}
#[test]
fn side_view_blinder_dissolves_at_the_medium_boundary() {
    let mut renderer = Renderer::new().unwrap();
    let mut f = frame();
    f.draws.clear();
    f.transparent.clear();
    lights(&mut f, 1);
    f.camera.eye = Vec3::new(16.0, -40.0, 5.0);
    f.camera.target = Vec3::new(16.0, 0.0, 5.0);
    f.haze_bounds = luma_scene::Aabb::new(Vec3::splat(-16.0), Vec3::splat(16.0));
    f.fixture_cones[0].position = Vec3::new(0.0, 0.0, 5.0);
    f.fixture_cones[0].direction = Vec3::X;
    f.fixture_cones[0].range = 48.0;
    f.fixture_cones[0].cos_beam = 35_f32.to_radians().cos();
    f.fixture_cones[0].cos_field = 60_f32.to_radians().cos();
    f.fixture_cones[0].intensity = 1.0;
    let pixels = renderer.render(&f, 640, 480, 8).unwrap();
    capture("blinder-side", &pixels, 640, 480);
    // Sight parallel to the volume boundary: a hard box creates a vertical wall.
    // Beyond the source, no single column may contain a wall-sized drop.
    // Average a central strip to avoid mistaking individual samples for edges.
    let columns: Vec<f32> = (260..620)
        .map(|x| {
            (180..300)
                .map(|y| pixels[(y * 640 + x) * 4 + 2] as f32)
                .sum::<f32>()
                / 120.0
        })
        .collect();
    let peak = columns.iter().copied().fold(0.0_f32, f32::max);
    assert!(peak > 10.0, "regression scene must contain visible haze");
    let drop = columns
        .windows(2)
        .map(|v| v[0] - v[1])
        .fold(0.0_f32, f32::max);
    assert!(
        drop < peak * 0.08,
        "abrupt side-view cutoff: {drop} / {peak}"
    );
}

#[test]
fn narrow_beams_retain_bright_origins_at_viewport_resolution() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut catalogue = luma_render::Catalogue::load(&root.join("goldens/scenes.json")).unwrap();
    let lens = catalogue
        .definitions
        .get_mut("golden/moving-head.qxf")
        .unwrap()
        .physical
        .as_mut()
        .unwrap()
        .lens
        .as_mut()
        .unwrap();
    lens.degrees_min = 8.0;
    lens.degrees_max = 8.0;
    let scene = catalogue
        .scenes
        .iter_mut()
        .find(|s| s.id == "mover-fan")
        .unwrap();
    scene.render.haze.density = 0.24;
    scene.render.haze.appearance = HazeAppearance {
        cloudiness: 0.7,
        cloud_size: 2.0,
        wind_speed: 0.0,
        ..Default::default()
    };
    scene.render.show_grid = false;
    scene.render.show_gizmos = false;
    for state in scene.state.values_mut() {
        state.color = [0.35, 0.8, 1.0];
        state.dimmer = 1.0;
        state.strobe = 0.0;
    }
    let mut library = Library::new(root.join("../../../resources/meshes"));
    let mut f = luma_render::build_frame(scene, &catalogue.definitions, 0.0, &mut library).unwrap();
    let mut renderer = Renderer::new().unwrap();
    for resolution in [0.5, 1.0] {
        f.haze_resolution = resolution;
        let pixels = renderer.render(&f, 960, 720, 8).unwrap();
        capture(
            if resolution < 1.0 {
                "narrow-half"
            } else {
                "narrow-full"
            },
            &pixels,
            960,
            720,
        );
        // This strip lies below the housings: fixture materials cannot satisfy
        // the highlight check in place of actual scattered light.
        let peak = (235..280)
            .flat_map(|y| (240..720).map(move |x| (y * 960 + x) * 4))
            .map(|i| pixels[i].min(pixels[i + 1]).min(pixels[i + 2]))
            .max()
            .unwrap();
        assert!(
            peak > 220,
            "narrow beam origins lost their bright broadband highlights: {peak}"
        );
    }
}

#[test]
fn production_haze_keeps_the_shared_grid_across_dimmer_counts() {
    let mut renderer = Renderer::new_profiled().unwrap();
    let mut f = frame();
    for count in [16, 128, 129] {
        lights(&mut f, count);
        f.geometry_shadows = true;
        f.fixture_shadows = true;
        let timing = renderer.profile_live_frame(&f, 320, 240, 1).unwrap();
        assert!(
            timing.gpu_fog_grid_ms > 0.0,
            "production haze stopped using the shared grid at {count} active emitters"
        );
    }
}

#[test]
fn haze_controls_round_trip_and_invalid_values_are_bounded() {
    let authored = HazeAppearance {
        cloudiness: 0.9,
        cloud_size: 8.0,
        turbulence: 0.6,
        wind_speed: 2.0,
        wind_direction: 270.0,
    };
    assert_eq!(
        authored,
        serde_json::from_str(&serde_json::to_string(&authored).unwrap()).unwrap()
    );
    let bad = HazeAppearance {
        cloudiness: f32::NAN,
        cloud_size: 0.0,
        turbulence: 2.0,
        wind_speed: -1.0,
        wind_direction: -90.0,
    }
    .sanitized();
    assert!(bad.cloudiness.is_finite());
    assert!(bad.cloud_size > 0.0);
    assert_eq!(bad.turbulence, 1.0);
    assert_eq!(bad.wind_speed, 0.0);
    assert_eq!(bad.wind_direction, 270.0);
    let old: luma_render::scene_desc::HazeSettings = serde_json::from_value(
        serde_json::json!({"enabled":true,"steps":8,"resolution":0.5,"density":0.5}),
    )
    .unwrap();
    assert_eq!(old.appearance, HazeAppearance::default());
}
#[test]
#[ignore = "GPU benchmark; run explicitly on an otherwise idle device"]
fn profile_cloudy_haze() {
    let mut renderer = Renderer::new_profiled().unwrap();
    let mut f = frame();
    for count in [16, 128, 512] {
        lights(&mut f, count);
        for cloudiness in [0.0, 1.0] {
            f.haze_appearance.cloudiness = cloudiness;
            f.haze_resolution = 0.5;
            let mut times = Vec::new();
            for step in 0..12 {
                f.time = step as f32 / 60.0;
                let timing = renderer.profile_live_frame(&f, 1280, 720, 1).unwrap();
                if step >= 4 {
                    times.push(timing.gpu_total_ms);
                    if step == 11 {
                        eprintln!(
                            "passes haze={:.3} composite={:.3} grid={:.3}",
                            timing.gpu_volumetric_ms,
                            timing.gpu_composite_ms,
                            timing.gpu_fog_grid_ms
                        );
                    }
                }
            }
            times.sort_by(f64::total_cmp);
            eprintln!(
                "lights={count} cloudiness={cloudiness} median_gpu_ms={:.3}",
                times[times.len() / 2]
            );
        }
    }
}
