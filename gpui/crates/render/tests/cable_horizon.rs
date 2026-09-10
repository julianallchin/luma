//! Hanging cables retain their own coverage across the distant floor's fade.

use std::{collections::BTreeMap, path::PathBuf};

use luma_render::{
    assets::Library,
    build_frame_with,
    scene_desc::{
        CameraPose, Geometry, Piece, Procedural, RenderSettings, Scene, VenueEnvironment,
    },
    Renderer,
};

const WIDTH: u32 = 480;
const HEIGHT: u32 = 320;

fn scene() -> Scene {
    let mut render = RenderSettings::room(VenueEnvironment::outdoor(30.0), 45.0, 1.0);
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    Scene {
        id: "cable-horizon".into(),
        times: vec![0.0],
        camera: CameraPose {
            // A level view puts the horizon through the solid part of each
            // cable. The truss is below it and the ceiling fade is above it.
            position: [0.0, 7.0, 20.0],
            target: [0.0, 7.0, 0.0],
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: vec![Piece {
            id: "flown-truss".into(),
            geometry: Geometry::Procedural(Procedural::Truss { span: 10.0 }),
            kind: "truss".into(),
            pos: [0.0, 0.0, 6.0],
            rot: [0.0; 3],
            scale: 1.0,
        }],
        state: BTreeMap::new(),
    }
}

fn capture(name: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_CABLE_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
    }
}

#[test]
fn foreground_cables_remain_visible_across_the_horizon() {
    check_foreground_cables(false);
}

#[test]
fn global_fog_uses_the_cables_own_distance() {
    check_foreground_cables(true);
}

fn check_foreground_cables(haze: bool) {
    let mut renderer = Renderer::new().unwrap();
    let mut library = Library::default();
    let mut scene = scene();
    scene.render.haze.enabled = haze;
    scene.render.haze.density = 0.24;
    scene.render.haze.appearance.cloudiness = 0.0;
    let mut render = |scene: &Scene| {
        let frame =
            build_frame_with(scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap();
        renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap()
    };
    let cables = render(&scene);
    capture(if haze { "fog-cables" } else { "horizon-cables" }, &cables);
    scene.render.show_cables = false;
    let bare = render(&scene);
    capture(if haze { "fog-bare" } else { "horizon-bare" }, &bare);

    // Find cable pixels against the sky, then follow those same drops across
    // the distant floor and into the foreground. Measuring the
    // difference from a cable-free render avoids assuming a sky colour.
    let contrast = |x: u32, y: u32| -> u32 {
        let offset = ((y * WIDTH + x) * 4) as usize;
        (0..3)
            .map(|c| u32::from(bare[offset + c].saturating_sub(cables[offset + c])))
            .sum()
    };
    let above = HEIGHT / 2 - 4;
    let columns: Vec<_> = (0..WIDTH).filter(|&x| contrast(x, above) > 20).collect();
    assert!(
        columns.len() >= 3,
        "the rig must have visible cables against the sky"
    );
    for y in HEIGHT / 2..HEIGHT / 2 + 12 {
        for &x in &columns {
            assert!(
                contrast(x, y) * 5 > contrast(x, above) * 3,
                "cable disappeared over the floor at ({x}, {y}): contrast {}",
                contrast(x, y),
            );
        }
    }
}

#[test]
fn indoor_floor_keeps_its_distance_dissolve() {
    let mut renderer = Renderer::new().unwrap();
    let mut library = Library::default();
    let mut scene = scene();
    scene.pieces.clear();
    scene.camera.position = [0.0, 30.0, 20.0];
    scene.camera.target = [0.0, 30.0, 0.0];
    scene.render.sky = None;
    scene.render.environment.background = [0.15, 0.25, 0.4];
    let mut render = |floor| {
        scene.render.show_floor = floor;
        let mut frame =
            build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap();
        // Emission makes coverage observable independently of room lighting.
        if let Some(floor) = frame.draws.first_mut() {
            floor.material.emissive = glam::Vec3::X;
        }
        renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap()
    };
    let floor = render(true);
    let background = render(false);
    let delta = |x: u32, y: u32| -> u32 {
        let offset = ((y * WIDTH + x) * 4) as usize;
        (0..3)
            .map(|c| u32::from(floor[offset + c].abs_diff(background[offset + c])))
            .sum()
    };
    // These rays hit the floor roughly 750–860 m away. It must have
    // completely dissolved, even though its opaque depth still exists.
    for y in HEIGHT / 2 + 13..HEIGHT / 2 + 16 {
        for x in 20..WIDTH - 20 {
            assert!(delta(x, y) <= 3, "floor rim at ({x}, {y})");
        }
    }
    assert!(
        delta(WIDTH / 2, HEIGHT - 1) > 20,
        "the near floor must remain visible",
    );
}
