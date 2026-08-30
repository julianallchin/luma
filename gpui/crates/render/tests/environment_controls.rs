use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use glam::{Mat4, Vec3};
use luma_render::assets::Library;
use luma_render::build_frame_with;
use luma_render::scene_desc::{
    CameraPose, DebugView, DirectionalLight, Environment, EnvironmentProbe, Geometry, HazeSettings,
    Piece, RenderSettings, Scene,
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
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
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
        probe: None,
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
        shadow_softness: 1.0,
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
        shadow_softness: 1.0,
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
        geometry: Geometry::mesh("stage_lab/speaker_dbr15.glb"),
        kind: "speaker".into(),
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
    let mut black = probe_settings(left, 0.0, false);
    black.environment.ambient_intensity = 0.0;
    let dark = render(black);
    let mut ambient_settings = probe_settings(left, 0.0, false);
    ambient_settings.environment.ambient_intensity = 0.5;
    let ambient_visible = render(ambient_settings);
    assert!(
        ambient_visible
            .chunks_exact(4)
            .zip(dark.chunks_exact(4))
            .any(|(ambient, dark)| (0..3).any(|channel| ambient[channel] > dark[channel])),
        "ambient did not remain independently controllable with the sun off: {:.4} vs {:.4}",
        mean_rgb(&ambient_visible),
        mean_rgb(&dark),
    );
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
fn gpu_shadow_softness_is_stable_distinct_bounded_and_sun_independent() {
    const WIDTH: u32 = 256;
    const HEIGHT: u32 = 192;
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let piece = || Piece {
        id: "softness-caster".into(),
        geometry: Geometry::mesh("stage_lab/speaker_dbr15.glb"),
        kind: "speaker".into(),
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

    let mut hard_settings = probe_settings([-6.0, -4.0, 8.0], 2.0, true);
    hard_settings.sun.as_mut().unwrap().shadow_softness = 0.0;
    let mut soft_settings = hard_settings.clone();
    soft_settings.sun.as_mut().unwrap().shadow_softness = 3.0;
    let mut direct_settings = hard_settings.clone();
    direct_settings.sun.as_mut().unwrap().shadows = false;
    let mut sunless_hard_settings = hard_settings.clone();
    sunless_hard_settings.sun.as_mut().unwrap().intensity = 0.0;
    let mut sunless_soft_settings = sunless_hard_settings.clone();
    sunless_soft_settings.sun.as_mut().unwrap().shadow_softness = 3.0;

    let hard = render(hard_settings);
    let soft = render(soft_settings.clone());
    let soft_again = render(soft_settings);
    let direct = render(direct_settings);
    let sunless_hard = render(sunless_hard_settings);
    // With zero sun intensity, its softness cannot leak into ambient or
    // background evaluation.
    let sunless_soft = render(sunless_soft_settings);

    assert_eq!(soft, soft_again, "soft PCF must be byte-stable");
    assert_eq!(
        sunless_hard, sunless_soft,
        "softness leaked into sun-off rendering"
    );
    let changed = hard
        .chunks_exact(4)
        .zip(soft.chunks_exact(4))
        .filter(|(hard, soft)| hard != soft)
        .count();
    assert!(
        changed > 32,
        "hard and soft shadows differed at only {changed} pixels"
    );

    for ((soft, ambient), direct) in soft
        .chunks_exact(4)
        .zip(sunless_hard.chunks_exact(4))
        .zip(direct.chunks_exact(4))
    {
        for channel in 0..3 {
            assert!(
                soft[channel].saturating_add(2) >= ambient[channel]
                    && soft[channel] <= direct[channel].saturating_add(2),
                "soft shadow escaped its ambient/direct energy bounds: {} not in {}..={}",
                soft[channel],
                ambient[channel],
                direct[channel]
            );
        }
    }
}

#[test]
fn gpu_far_cascade_keeps_directional_occlusion() {
    const WIDTH: u32 = 160;
    const HEIGHT: u32 = 120;
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let definitions = BTreeMap::new();
    let mut library = Library::new(meshes);
    let mut renderer = luma_render::Renderer::new().unwrap();
    let render = |shadows: bool, renderer: &mut luma_render::Renderer, library: &mut Library| {
        let mut render = probe_settings([2.0, -3.0, 6.0], 2.0, shadows);
        render.fov = 20.0;
        render.debug_view = DebugView::Shadow;
        let far_scene = Scene {
            id: "far-cascade-proof".into(),
            times: vec![0.0],
            camera: CameraPose {
                position: [0.0, -80.0, 20.0],
                target: [0.0, 0.0, 0.0],
            },
            editing: false,
            aim_arrows: false,
            render,
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
            fixtures: Vec::new(),
            pieces: vec![Piece {
                id: "far-shadow-caster".into(),
                geometry: Geometry::mesh("stage_lab/speaker_dbr15.glb"),
                kind: "speaker".into(),
                pos: [0.0, 0.0, 0.0],
                rot: [0.0, 0.0, 0.0],
                scale: 8.0,
            }],
            state: BTreeMap::new(),
        };
        let frame = build_frame_with(&far_scene, &definitions, &|_, _| None, 0.0, library).unwrap();
        renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap()
    };
    let shadowed = render(true, &mut renderer, &mut library);
    let unshadowed = render(false, &mut renderer, &mut library);
    let removed_light: u64 = shadowed
        .chunks_exact(4)
        .zip(unshadowed.chunks_exact(4))
        .map(|(shadow, direct)| {
            (0..3)
                .map(|channel| u64::from(direct[channel].saturating_sub(shadow[channel])))
                .sum::<u64>()
        })
        .sum();
    assert!(
        removed_light > 10_000,
        "third-cascade shadowing did not survive at 80 m: {removed_light}"
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
        geometry: Geometry::mesh("stage_lab/cdj_3000x.glb"),
        kind: "speaker".into(),
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
        first
            .draws
            .iter()
            .any(|draw| draw.textures.base_color.is_some()),
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
                    draw.textures,
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

    // The second draw takes the resident geometry/texture path. Capture mode
    // must remain byte deterministic across that lifetime transition.
    let mut renderer = luma_render::Renderer::new().unwrap();
    let first_pixels = renderer.render(&first, 160, 120, 2).unwrap();
    let first_uploads = renderer.upload_stats();
    let resident_pixels = renderer.render(&second, 160, 120, 2).unwrap();
    assert_eq!(first_pixels, resident_pixels);
    assert_eq!(
        renderer.upload_stats(),
        first_uploads,
        "unchanged mesh and texture identities must not upload again"
    );
}

fn stable_pixel_hash(pixels: &[u8]) -> u64 {
    pixels.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn mean_channel(pixels: &[u8], channel: usize) -> f64 {
    pixels
        .chunks_exact(4)
        .map(|pixel| f64::from(pixel[channel]))
        .sum::<f64>()
        / (pixels.len() / 4) as f64
}

#[test]
fn material_lab_maps_debug_views_and_uploads_are_deterministic() {
    const WIDTH: u32 = 128;
    const HEIGHT: u32 = 96;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens");
    let piece = || Piece {
        id: "material-lab".into(),
        geometry: Geometry::mesh("material-lab.gltf"),
        kind: "speaker".into(),
        pos: [0.0, 0.0, 0.75],
        rot: [0.0, 0.0, 0.0],
        scale: 1.0,
    };
    let mut base = RenderSettings::dark_stage(45.0, 1.0);
    base.environment = Environment {
        background: [0.0; 3],
        ambient_color: [1.0; 3],
        ambient_intensity: 0.5,
        probe: None,
    };
    base.haze.enabled = false;
    base.sun = Some(DirectionalLight {
        direction: [2.0, -3.0, 6.0],
        color: [1.0; 3],
        intensity: 1.0,
        shadows: true,
        shadow_softness: 1.0,
    });

    let mut library = Library::new(root);
    let definitions = BTreeMap::new();
    let mut renderer = luma_render::Renderer::new().unwrap();
    let mut hashes = Vec::new();
    for debug_view in [
        DebugView::Pbr,
        DebugView::BaseColor,
        DebugView::Normals,
        DebugView::Metallic,
        DebugView::Roughness,
        DebugView::Shadow,
        DebugView::Depth,
        DebugView::VolumetricAccumulation,
    ] {
        let mut settings = base.clone();
        settings.debug_view = debug_view;
        let frame = build_frame_with(
            &scene(settings, vec![piece()]),
            &definitions,
            &|_, _| None,
            0.0,
            &mut library,
        )
        .unwrap();
        let material = frame
            .draws
            .iter()
            .find(|draw| draw.textures.base_color.is_some())
            .expect("material-lab primitive");
        assert!(material.textures.normal.is_some());
        assert!(material.textures.metallic_roughness.is_some());
        assert!(material.textures.occlusion.is_some());
        assert!(material.textures.emissive.is_some());
        assert!(frame.meshes[material.mesh]
            .vertices
            .iter()
            .all(|vertex| vertex.tangent.iter().all(|value| value.is_finite())));

        let first = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        let uploads = renderer.upload_stats();
        let second = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        assert_eq!(
            first, second,
            "{debug_view:?} changed across identical draws"
        );
        assert_eq!(
            renderer.upload_stats(),
            uploads,
            "{debug_view:?} re-uploaded resident resources"
        );
        hashes.push(stable_pixel_hash(&first));
    }
    assert_eq!(
        hashes,
        [
            0xbb12_09d7_6211_83ca,
            0xadc8_cdaa_603a_d066,
            0xc75b_c91d_43aa_766f,
            0x3736_a13a_731a_8c57,
            0x9e3d_0e0a_4272_7a85,
            0x3afa_7678_5271_aed0,
            0xe8f3_3348_8f85_b940,
            0xac0c_ad64_9319_a325,
        ],
        "material/debug golden output drifted"
    );

    // The lab's normal map points strongly along tangent-space +Y. Reflecting
    // model X must leave that physical bitangent direction unchanged: T flips,
    // so tangent.w must flip too. Reverse triangle winding only compensates
    // raster culling; it does not change the tangent frame under test.
    let mut normal_settings = base;
    normal_settings.debug_view = DebugView::Normals;
    let mut original = build_frame_with(
        &scene(normal_settings.clone(), vec![piece()]),
        &definitions,
        &|_, _| None,
        0.0,
        &mut library,
    )
    .unwrap();
    let mut mirrored = build_frame_with(
        &scene(normal_settings, vec![piece()]),
        &definitions,
        &|_, _| None,
        0.0,
        &mut library,
    )
    .unwrap();
    let original_draw = original
        .draws
        .iter()
        .position(|draw| draw.textures.normal.is_some())
        .unwrap();
    let mirrored_draw = mirrored
        .draws
        .iter()
        .position(|draw| draw.textures.normal.is_some())
        .unwrap();
    let mesh = mirrored.draws[mirrored_draw].mesh;
    mirrored.draws[mirrored_draw].model *= Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    let mut reversed = Vec::with_capacity(mirrored.meshes[mesh].indices.len());
    for triangle in mirrored.meshes[mesh].indices.chunks_exact(3) {
        reversed.extend([triangle[0], triangle[2], triangle[1]]);
    }
    mirrored.meshes[mesh].indices = reversed.into();
    mirrored.meshes[mesh].key.push_str("#mirrored-x");
    let original_mesh = original.draws[original_draw].mesh;
    original.meshes[original_mesh]
        .key
        .push_str("#original-normal");
    let original_pixels = renderer.render(&original, WIDTH, HEIGHT, 1).unwrap();
    let mirrored_pixels = renderer.render(&mirrored, WIDTH, HEIGHT, 1).unwrap();
    assert_eq!(
        (
            stable_pixel_hash(&original_pixels),
            stable_pixel_hash(&mirrored_pixels),
        ),
        (0xc75b_c91d_43aa_766f, 0x0e74_ae02_1965_fcc4),
        "normal-map orientation golden drifted"
    );
    assert!(
        (mean_channel(&original_pixels, 1) - mean_channel(&mirrored_pixels, 1)).abs() < 1.0,
        "mirroring reversed the mapped bitangent: {:.2} vs {:.2}",
        mean_channel(&original_pixels, 1),
        mean_channel(&mirrored_pixels, 1),
    );
}

#[test]
fn hdr_ibl_is_resident_deterministic_and_energy_monotonic() {
    const WIDTH: u32 = 128;
    const HEIGHT: u32 = 96;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens");
    let mut settings = RenderSettings::dark_stage(45.0, 1.0);
    settings.haze.enabled = false;
    settings.environment.ambient_intensity = 0.0;
    settings.environment.probe = Some(EnvironmentProbe {
        asset: "../../../../resources/meshes/environments/studio.hdr".into(),
        intensity: 0.25,
        rotation_deg: 0.0,
        visible: true,
    });
    let mut library = Library::new(root);
    let mut frame = build_frame_with(
        &scene(
            settings,
            vec![Piece {
                id: "material-lab".into(),
                geometry: Geometry::mesh("material-lab.gltf"),
                kind: "speaker".into(),
                pos: [0.0, 0.0, 0.75],
                rot: [0.0, 0.0, 0.0],
                scale: 1.0,
            }],
        ),
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut library,
    )
    .unwrap();
    let mut renderer = luma_render::Renderer::new().unwrap();

    let low = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert_eq!(renderer.upload_stats().environments, 1);
    let low_repeat = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert_eq!(low, low_repeat);
    assert_eq!(renderer.upload_stats().environments, 1);

    frame.environment.as_mut().unwrap().intensity = 1.0;
    let high = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert!(mean_rgb(&high) > mean_rgb(&low) + 2.0);
    assert!(
        high.chunks_exact(4)
            .any(|pixel| pixel[..3].iter().copied().max().unwrap() > 32),
        "rough specular preprocessing returned a black probe"
    );
    assert_eq!(renderer.upload_stats().environments, 1);

    let resident = frame.environment.take();
    let ambient_fallback = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    frame.environment = resident;
    let enabled_again = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert_eq!(renderer.upload_stats().environments, 1);
    assert_eq!(high, enabled_again);
    assert!(mean_rgb(&ambient_fallback) < mean_rgb(&high));

    frame.environment.as_mut().unwrap().rotation = 90f32.to_radians();
    let rotated = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    assert_ne!(stable_pixel_hash(&high), stable_pixel_hash(&rotated));
    assert_eq!(stable_pixel_hash(&high), 0xa814_fc7d_8070_f668);
    assert_eq!(stable_pixel_hash(&rotated), 0x29c7_ea1c_8747_31bb);
}

#[test]
fn procedural_emissive_survives_the_absent_texture_identity() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens/scenes.json");
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut catalogue = luma_render::Catalogue::load(&path).unwrap();
    let scene = catalogue
        .scenes
        .iter_mut()
        .find(|scene| scene.id == "led-bar")
        .unwrap();
    scene.render.environment = Environment::DARK;
    scene.render.sun = None;
    scene.render.haze.enabled = false;
    scene.render.show_grid = false;
    scene.render.debug_view = DebugView::Pbr;
    let mut library = Library::new(meshes);
    let frame = luma_render::build_frame(scene, &catalogue.definitions, 4.2, &mut library).unwrap();
    assert!(
        frame
            .draws
            .iter()
            .any(|draw| draw.textures.emissive.is_none()
                && draw.material.emissive.max_element() > 1.0)
    );
    let pixels = luma_render::Renderer::new()
        .unwrap()
        .render(&frame, 128, 96, 1)
        .unwrap();
    assert_eq!(
        stable_pixel_hash(&pixels),
        0x18ca_fdc6_452f_4226,
        "legacy procedural-emissive output drifted"
    );
    assert!(
        pixels
            .chunks_exact(4)
            .any(|pixel| pixel[0].max(pixel[1]).max(pixel[2]) > 96),
        "procedural emitters went black when no emissive texture was bound"
    );
}

#[test]
fn live_async_presentation_matches_deterministic_capture_without_ui_polling() {
    const WIDTH: u32 = 96;
    const HEIGHT: u32 = 72;
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let proof_piece = || Piece {
        id: "async-proof".into(),
        geometry: Geometry::mesh("stage_lab/speaker_dbr15.glb"),
        kind: "speaker".into(),
        pos: [0.0, 0.0, 0.0],
        rot: [0.0, 0.0, 0.0],
        scale: 1.0,
    };
    let proof = scene(
        probe_settings([-6.0, -4.0, 8.0], 1.4, true),
        vec![proof_piece()],
    );
    let definitions = BTreeMap::new();
    let build = |library: &mut Library| {
        build_frame_with(&proof, &definitions, &|_, _| None, 0.0, library).unwrap()
    };

    let mut capture_library = Library::new(meshes.clone());
    let capture = build(&mut capture_library);
    let expected = luma_render::Renderer::new()
        .unwrap()
        .render(&capture, WIDTH, HEIGHT, 1)
        .unwrap();

    let mut live_library = Library::new(meshes);
    let mut live = luma_render::AsyncViewport::new();
    live.set_subframes(1);
    for _ in 0..4 {
        live.submit(build(&mut live_library), WIDTH, HEIGHT);
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let presented = loop {
        if let Some(frame) = live.take_latest() {
            let frame = frame.unwrap();
            if frame.serial == 4 {
                break frame;
            }
        }
        assert!(Instant::now() < deadline, "async GPU frame did not retire");
        std::thread::sleep(Duration::from_millis(2));
    };
    let mut rgba = presented.image.to_bytes();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    assert_eq!(
        rgba, expected,
        "live BGRA output diverged from capture RGBA"
    );
}
