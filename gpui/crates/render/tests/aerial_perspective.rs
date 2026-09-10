//! Finite surface transport through the actual outdoor rendering path.

use std::{collections::BTreeMap, path::PathBuf};

use glam::{Mat4, Vec3};
use luma_render::{
    assets::{Library, Material, Vertex},
    build_frame_with,
    frame::{Draw, Frame, MaterialTextures, MeshData},
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene, VenueEnvironment},
    Renderer,
};

const WIDTH: u32 = 600;
const HEIGHT: u32 = 360;
const OFFSETS: [f32; 3] = [-0.4, 0.0, 0.4];

fn cards() -> Frame {
    let mut render = RenderSettings::room(VenueEnvironment::outdoor(30.0), 45.0, 1.0);
    render.haze.enabled = false;
    render.show_floor = false;
    render.show_grid = false;
    render.show_gizmos = false;
    let scene = Scene {
        id: "aerial-distance-cards".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, 10.0, 0.0],
            target: [0.0, 10.0, -1.0],
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
    let mesh = frame.meshes.len();
    frame.meshes.push(MeshData {
        key: "::aerial-distance-card".into(),
        vertices: [
            [-1.0, 0.0, -1.0],
            [1.0, 0.0, -1.0],
            [1.0, 0.0, 1.0],
            [-1.0, 0.0, 1.0],
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
    // Equal angular sizes make the sampled pixels independent of resolution.
    // A black metal has zero reflected radiance; its only light comes from air.
    for (distance, offset) in [20.0, 1000.0, 10_000.0].into_iter().zip(OFFSETS) {
        frame.draws.push(Draw {
            mesh,
            model: Mat4::from_translation(Vec3::new(offset * distance, distance, 10.0))
                * Mat4::from_scale(Vec3::splat(distance * 0.1)),
            material: Material {
                base_color: Vec3::ZERO,
                metallic: 1.0,
                ..Default::default()
            },
            textures: MaterialTextures::default(),
            editor_object: None,
        });
    }
    frame
}

fn center(pixels: &[u8], card: usize) -> Vec3 {
    let focal = HEIGHT as f32 / (2.0 * 22.5_f32.to_radians().tan());
    let x = (WIDTH as f32 * 0.5 + OFFSETS[card] * focal) as u32;
    let offset = ((HEIGHT / 2 * WIDTH + x) * 4) as usize;
    Vec3::new(
        pixels[offset] as f32,
        pixels[offset + 1] as f32,
        pixels[offset + 2] as f32,
    )
}

fn capture(name: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_AERIAL_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
    }
}

#[test]
fn distant_surfaces_gain_airlight_and_retain_their_radiance() {
    let mut renderer = Renderer::new().unwrap();
    let mut frame = cards();
    let black = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    capture("black-cards-20m-1km-10km", &black);
    for draw in &mut frame.draws {
        draw.material.emissive = Vec3::ONE;
    }
    let white = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    capture("white-cards-20m-1km-10km", &white);
    let mut previous = f32::INFINITY;
    for card in 0..3 {
        let dark = center(&black, card);
        let bright = center(&white, card);
        let contrast = (bright - dark).element_sum();
        assert!(
            contrast > 60.0,
            "surface {card} dissolved into the background: {dark:?}, {bright:?}"
        );
        assert!(
            contrast < previous,
            "contrast must fall with distance: {contrast} vs {previous}"
        );
        previous = contrast;
    }
    assert!(
        center(&black, 0).max_element() < 12.0,
        "20 m of clear air barely affects the rig"
    );
    assert!(
        center(&black, 2).min_element() > 30.0,
        "10 km of air adds scattered light"
    );

    // The renderer's material diagnostics must still report the material,
    // independently of atmospheric transport or the former 700 m dissolve.
    frame.debug_view = DebugView::BaseColor;
    for draw in &mut frame.draws {
        draw.material.base_color = Vec3::new(0.2, 0.4, 0.8);
    }
    let debug = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
    for card in 1..3 {
        assert!(
            (center(&debug, card) - center(&debug, 0))
                .abs()
                .max_element()
                <= 1.0
        );
    }
}
