//! Sun shadows must reach a surface a few centimetres under its occluder.
//!
//! A beam hung under a deck plate, facing the sun, is in the plate's shadow
//! however close to the plate it is. A depth bias measured in shadow-map NDC
//! instead of metres let the sun through for the first 10 to 30 cm under
//! every occluder: the sides of deck beams were sunlit deep under a stage,
//! with a stepped edge where the leak ran out, and props on a table had no
//! shadow under them.
use std::collections::BTreeMap;

use glam::{Mat4, Vec3, Vec4};
use luma_render::{
    assets::{Library, Material, Vertex},
    build_frame_with,
    frame::{Draw, MaterialTextures, MeshData},
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene, VenueEnvironment, VenueHaze},
    Frame, Renderer,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

fn cube() -> MeshData {
    let faces: [(Vec3, Vec3, Vec3); 6] = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::NEG_Y, Vec3::Z),
        (Vec3::Y, Vec3::NEG_X, Vec3::Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::X, Vec3::NEG_Y),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (n, u, w) in faces {
        let base = vertices.len() as u32;
        for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = n * 0.5 + u * 0.5 * a + w * 0.5 * b;
            vertices.push(Vertex {
                position: p.to_array(),
                normal: n.to_array(),
                uv: [0.0; 2],
                tangent: [u.x, u.y, u.z, 1.0],
            });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    MeshData {
        key: "::sun-shadow-contact-cube".into(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

/// A 4 m square plate, 2 cm thick, with its underside at 1 m, and a beam
/// 10 cm deep hung under its middle along X. The sun comes from -Y.
fn frame(eye: Vec3, target: Vec3) -> Frame {
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(40.0),
        VenueHaze::default(),
        45.0,
        1.0,
    );
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.debug_view = DebugView::Shadow;
    // Camera poses are three-space: (x, up, -y).
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "sun-shadow-contact".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: three(eye),
            target: three(target),
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
    let sun = frame
        .directional
        .expect("an outdoor room has a sun")
        .direction;
    assert!(
        sun.y < -0.5 && sun.z > 0.5,
        "the scene assumes a sun from -Y, up about 40 degrees; got {sun:?}"
    );
    let mesh = frame.meshes.len();
    frame.meshes.push(cube());
    let material = Material {
        base_color: Vec3::splat(0.5),
        metallic: 0.0,
        roughness: 0.8,
        normal_scale: 1.0,
        occlusion_strength: 1.0,
        ..Default::default()
    };
    let at = frame.draws.len() - frame.transparent.len();
    let draws = [
        (Vec3::new(0.0, 0.0, 1.01), Vec3::new(4.0, 4.0, 0.02)),
        (Vec3::new(0.0, 0.0, 0.95), Vec3::new(4.0, 0.05, 0.1)),
    ]
    .map(|(centre, size)| Draw {
        mesh,
        model: Mat4::from_translation(centre) * Mat4::from_scale(size),
        material: material.clone(),
        textures: MaterialTextures::default(),
        editor_object: None,
    });
    frame.draws.splice(at..at, draws);
    frame
}

fn pixel(frame: &Frame, p: Vec3) -> (u32, u32) {
    let projection = Mat4::perspective_rh(
        frame.camera.fov_y_deg.to_radians(),
        WIDTH as f32 / HEIGHT as f32,
        5000.0,
        0.1,
    );
    let view = Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z);
    let clip = projection * view * Vec4::new(p.x, p.y, p.z, 1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x * 0.5 + 0.5) * WIDTH as f32) as u32,
        ((0.5 - ndc.y * 0.5) * HEIGHT as f32) as u32,
    )
}

/// Mean red channel, zero to one, over a 5x5 patch.
fn shadow_at(pixels: &[u8], (x, y): (u32, u32)) -> f32 {
    let mut sum = 0.0;
    for j in y - 2..=y + 2 {
        for i in x - 2..=x + 2 {
            sum += f32::from(pixels[((j * WIDTH + i) * 4) as usize]) / 255.0;
        }
    }
    sum / 25.0
}

#[test]
fn a_beam_under_a_deck_plate_is_in_its_shadow_up_to_the_plate() {
    let mut renderer = Renderer::new().unwrap();
    // Close up and across the first cascade. In the second cascade (from
    // 12 m) a texel is about 4 cm, and the 3x3 filter still lets a little
    // sun into the first 10 cm under a flat occluder.
    for distance in [1.5, 8.0] {
        let eye = Vec3::new(0.3, -distance, 0.6);
        let frame = frame(eye, Vec3::new(0.0, 0.0, 0.95));
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        // The beam's sun-facing side: its middle, and 1.5 cm under the plate.
        for z in [0.95, 0.985] {
            let at = pixel(&frame, Vec3::new(0.0, -0.026, z));
            let shadow = shadow_at(&pixels, at);
            assert!(
                shadow < 0.05,
                "from {distance} m the beam side at z = {z} has sun visibility {shadow}; \
                 it is 2 m under a solid plate"
            );
        }
        if distance > 5.0 {
            // Open ground in front of the plate, as a control.
            let at = pixel(&frame, Vec3::new(0.0, -2.6, 0.0));
            let open = shadow_at(&pixels, at);
            assert!(
                open > 0.95,
                "open ground from {distance} m has sun visibility {open}"
            );
        }
    }
}
