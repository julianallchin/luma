use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::build_frame_with;
use luma_render::scene_desc::{
    CameraPose, DirectionalLight, Environment, HazeSettings, Piece, RenderSettings, Scene,
};

fn scene(render: RenderSettings, pieces: Vec<Piece>) -> Scene {
    Scene {
        id: "material-lighting-proof".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        render,
        selected_fixture_ids: Vec::new(),
        fixtures: Vec::new(),
        pieces,
        state: BTreeMap::new(),
    }
}

fn settings() -> RenderSettings {
    let mut settings = RenderSettings::dark_stage(50.0, 0.5);
    settings.environment = Environment {
        background: [0.01, 0.02, 0.03],
        ambient_color: [0.25, 0.5, 1.0],
        ambient_intensity: 0.4,
    };
    settings.haze = HazeSettings {
        enabled: true,
        steps: 12,
        resolution: 0.5,
        density: 0.7,
    };
    settings.sun = Some(DirectionalLight {
        direction: [2.0, -3.0, 6.0],
        color: [1.0, 0.5, 0.25],
        intensity: 2.0,
        shadows: false,
    });
    settings
}

#[test]
fn environment_contract_round_trips_without_legacy_dark_stage() {
    let expected = settings();
    let value = serde_json::to_value(&expected).unwrap();

    assert!(value.get("darkStage").is_none());
    assert_eq!(value["haze"]["enabled"], true);
    assert_eq!(value["sun"]["shadows"], false);
    assert_eq!(
        serde_json::from_value::<RenderSettings>(value).unwrap(),
        expected
    );
}

fn probe_settings(direction: [f32; 3], intensity: f32, shadows: bool) -> RenderSettings {
    let mut settings = RenderSettings::dark_stage(45.0, 0.5);
    settings.environment.ambient_intensity = 0.03;
    settings.haze.enabled = false;
    settings.haze.steps = 1;
    settings.haze.density = 0.0;
    settings.sun = (intensity > 0.0).then_some(DirectionalLight {
        direction,
        color: [1.0; 3],
        intensity,
        shadows,
    });
    settings
}

fn mean_rgb(pixels: &[u8]) -> f64 {
    pixels
        .chunks_exact(4)
        .map(|pixel| f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2]))
        .sum::<f64>()
        / (pixels.len() / 4 * 3) as f64
}

fn shadow_centroid(shadowed: &[u8], direct: &[u8], width: usize) -> (f64, f64) {
    let mut weight = 0.0;
    let mut x_sum = 0.0;
    for (index, (shadow, lit)) in shadowed
        .chunks_exact(4)
        .zip(direct.chunks_exact(4))
        .enumerate()
    {
        let delta = (0..3)
            .map(|channel| f64::from(lit[channel].saturating_sub(shadow[channel])))
            .sum::<f64>();
        if delta > 2.0 {
            weight += delta;
            x_sum += (index % width) as f64 * delta;
        }
    }
    (x_sum / weight.max(1.0), weight)
}

#[test]
fn gpu_probes_sun_direction_intensity_and_shadow_toggle() {
    const WIDTH: u32 = 192;
    const HEIGHT: u32 = 144;
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let piece = || Piece {
        id: "shadow-caster".into(),
        mesh_path: "stage_lab/speaker_dbr15.glb".into(),
        pos: [0.0, 0.0, 0.0],
        rot: [0.0, 0.0, 0.0],
        scale: 1.0,
    };
    let definitions = BTreeMap::new();
    let mut library = Library::new(meshes);
    let mut renderer = luma_render::Renderer::new().unwrap();
    let mut render = |settings: RenderSettings| {
        let frame = build_frame_with(
            &scene(settings, vec![piece()]),
            &definitions,
            &|_, _| None,
            0.0,
            &mut library,
        )
        .unwrap();
        renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap()
    };

    let left = [-6.0, -4.0, 8.0];
    let right = [6.0, -4.0, 8.0];
    let left_shadow = render(probe_settings(left, 2.0, true));
    let left_direct = render(probe_settings(left, 2.0, false));
    let right_shadow = render(probe_settings(right, 2.0, true));
    let right_direct = render(probe_settings(right, 2.0, false));
    let (left_centroid, left_weight) = shadow_centroid(&left_shadow, &left_direct, WIDTH as usize);
    let (right_centroid, right_weight) =
        shadow_centroid(&right_shadow, &right_direct, WIDTH as usize);
    assert!(left_weight > 500.0 && right_weight > 500.0);
    assert!(
        (left_centroid - right_centroid).abs() > 3.0,
        "sun direction did not move the shadow centroid: {left_centroid:.2} vs {right_centroid:.2}"
    );

    let dim = render(probe_settings(left, 0.4, false));
    let bright = render(probe_settings(left, 2.0, false));
    assert!(
        mean_rgb(&bright) > mean_rgb(&dim) + 1.0,
        "sun intensity was not energy-monotonic: {:.3} -> {:.3}",
        mean_rgb(&dim),
        mean_rgb(&bright)
    );

    let ambient_only = render(probe_settings(left, 0.0, false));
    assert!(
        mean_rgb(&left_direct) > mean_rgb(&ambient_only) + 1.0,
        "disabling shadows also removed direct light"
    );
    assert!(
        mean_rgb(&left_direct) > mean_rgb(&left_shadow),
        "disabling shadows did not remove shadow darkening"
    );
}

#[test]
fn checked_in_legacy_catalogue_is_adapted_at_load_boundary() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens/scenes.json");
    let catalogue = luma_render::Catalogue::load(&path).unwrap();

    let lit = catalogue
        .scenes
        .iter()
        .find(|scene| scene.id == "venue-no-haze")
        .unwrap();
    assert_eq!(lit.render.environment, Environment::EDITOR);
    assert_eq!(lit.render.sun, Some(DirectionalLight::EDITOR));
    assert!(lit.render.show_grid);
    assert!(!lit.render.haze.enabled);
    let mut library = Library::default();
    let frame = build_frame_with(
        &scene(lit.render.clone(), Vec::new()),
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut library,
    )
    .unwrap();
    assert_eq!(
        frame.directional.unwrap().shadow_eye,
        Vec3::new(8.0, -6.0, 12.0),
        "the legacy adapter must retain the exact captured shadow anchor"
    );

    let dark = catalogue
        .scenes
        .iter()
        .find(|scene| scene.render.environment == Environment::DARK)
        .unwrap();
    assert!(dark.render.sun.is_none());
    assert!(!dark.render.show_grid);
    assert!(dark.render.haze.enabled);
}

#[test]
fn frame_resolves_sun_environment_and_haze_independently() {
    let mut library = Library::default();
    let definitions = BTreeMap::new();
    let render = settings();
    let lit = scene(render.clone(), Vec::new());
    let frame = build_frame_with(&lit, &definitions, &|_, _| None, 0.0, &mut library).unwrap();

    assert_eq!(frame.clear_color, Vec3::new(0.01, 0.02, 0.03));
    assert_eq!(frame.ambient, Vec3::new(0.1, 0.2, 0.4));
    let sun = frame.directional.unwrap();
    assert!(sun
        .direction
        .abs_diff_eq(Vec3::new(2.0, -3.0, 6.0).normalize(), 1e-6));
    assert_eq!(sun.radiance, Vec3::new(2.0, 1.0, 0.5));
    assert!(!sun.shadows);
    assert!((frame.haze_density - 0.21).abs() < 1e-6);
    assert_eq!(frame.haze_steps, 12);
    assert_eq!(frame.haze_resolution, 0.5);

    let mut sunless = render.clone();
    sunless.sun = None;
    let frame = build_frame_with(
        &scene(sunless, Vec::new()),
        &definitions,
        &|_, _| None,
        0.0,
        &mut library,
    )
    .unwrap();
    assert!(frame.directional.is_none());
    assert!((frame.haze_density - 0.21).abs() < 1e-6);

    let mut clear_air = render;
    clear_air.haze.enabled = false;
    let frame = build_frame_with(
        &scene(clear_air, Vec::new()),
        &definitions,
        &|_, _| None,
        0.0,
        &mut library,
    )
    .unwrap();
    assert!(frame.directional.is_some());
    assert_eq!(frame.haze_density, 0.0);
}

#[test]
fn textured_pbr_proof_scene_resolves_deterministically() {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let proof_piece = || Piece {
        id: "proof-cdj".into(),
        mesh_path: "stage_lab/cdj_3000x.glb".into(),
        pos: [0.0, 0.0, 0.0],
        rot: [0.0, 0.0, 0.0],
        scale: 1.0,
    };
    let proof = scene(settings(), vec![proof_piece()]);
    let mut library = Library::new(meshes);
    let definitions = BTreeMap::new();

    let first = build_frame_with(&proof, &definitions, &|_, _| None, 0.0, &mut library).unwrap();
    let second = build_frame_with(&proof, &definitions, &|_, _| None, 0.0, &mut library).unwrap();

    assert!(
        !first.images.is_empty(),
        "proof asset must exercise sRGB textures"
    );
    assert!(
        first.draws.iter().any(|draw| draw.image.is_some()),
        "proof asset must bind a base-colour texture"
    );
    assert!(
        first
            .draws
            .iter()
            .any(|draw| { draw.material.metallic > 0.0 || draw.material.roughness < 1.0 }),
        "proof asset must exercise non-default metallic/roughness"
    );

    let signature = |frame: &luma_render::Frame| {
        frame
            .draws
            .iter()
            .map(|draw| {
                (
                    draw.image,
                    draw.material.base_color.to_array(),
                    draw.material.metallic.to_bits(),
                    draw.material.roughness.to_bits(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(signature(&first), signature(&second));
    assert_eq!(
        first
            .images
            .iter()
            .map(|image| (&image.key, &image.image.rgba))
            .collect::<HashMap<_, _>>(),
        second
            .images
            .iter()
            .map(|image| (&image.key, &image.image.rgba))
            .collect::<HashMap<_, _>>()
    );
}
