//! A shallow fixture wash must not turn an unobstructed floor into shadow bands.
use std::{collections::BTreeMap, path::PathBuf};

use glam::Vec3;
use luma_render::{
    assets::Library,
    build_frame_with,
    frame::FixtureCone,
    scene_desc::{CameraPose, RenderSettings, Scene, VenueEnvironment},
    Frame, Renderer,
};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 640;

fn floor() -> Frame {
    let mut render = RenderSettings::room(VenueEnvironment::outdoor(-12.0), 45.0, 1.0);
    render.show_grid = false;
    render.show_gizmos = true;
    render.haze.enabled = false;
    let scene = Scene {
        id: "floor-bands".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, 3.0, -27.0],
            target: [0.0, 0.0, 0.0],
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: vec![],
        editor: Default::default(),
        fixtures: vec![],
        pieces: vec![],
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
    frame.fixture_cones.push(FixtureCone {
        position: Vec3::new(0.0, 0.0, 5.0),
        range: 60.0,
        direction: Vec3::new(0.0, 1.0, -0.3).normalize(),
        cos_beam: 30_f32.to_radians().cos(),
        cos_field: 60_f32.to_radians().cos(),
        color: Vec3::new(1.0, 0.0, 0.4),
        intensity: 1.0,
        wash: 1.0,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
    });
    frame
}

fn capture(label: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_FLOOR_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{label}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
    }
}

#[test]
fn grazing_fixture_washes_do_not_make_the_floor_shadow_itself() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = floor();
    for (label, position) in [
        ("front", Vec3::new(0.0, 0.0, 5.0)),
        ("low", Vec3::new(0.0, 0.0, 1.0)),
        ("diagonal", Vec3::new(4.0, -2.0, 2.0)),
    ] {
        frame.fixture_cones[0].position = position;
        frame.fixture_cones[0].direction = (Vec3::new(0.0, 12.0, 0.0) - position).normalize();
        frame.fixture_shadows = true;
        frame.geometry_shadows = true;
        let shadowed = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("{label}-shadows"), &shadowed);
        frame.fixture_shadows = false;
        frame.geometry_shadows = false;
        let unshadowed = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("{label}-unshadowed"), &unshadowed);
        // Exclude the white compass by requiring a visibly magenta light pool.
        let lit_pixels = unshadowed
            .chunks_exact(4)
            .filter(|p| p[0] > p[1].saturating_add(20))
            .count();
        assert!(lit_pixels > 1000, "{label}: no visible floor wash");
        let error = shadowed
            .iter()
            .zip(&unshadowed)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(
            error <= 2,
            "{label}: unobstructed floor shadowed itself by {error} RGB levels"
        );
    }
}
