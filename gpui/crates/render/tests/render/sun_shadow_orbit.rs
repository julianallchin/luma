//! A sun shadow must not depend on where the camera stands.
//!
//! Under a low sun, a flown object is far from its shadow along the light:
//! a box 8 m up under a 6 degree sun is about 76 m from its shadow. Each
//! sun cascade used to take its depth range from the camera's view slice
//! plus 25 m, so from a camera near the shadow the box sat in front of the
//! near plane, was clipped, and cast nothing. Which casters survived
//! depended on the view slice, so shadows came and went as the camera
//! orbited.
use std::collections::BTreeMap;

use glam::{Mat4, Vec3, Vec4};
use luma_render::{
    assets::{Library, Material},
    build_frame_with,
    frame::{Draw, MaterialTextures},
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene, VenueEnvironment, VenueHaze},
    Frame, Renderer,
};

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const SUN_ELEVATION: f32 = 6.0;
const BOX_CENTRE: Vec3 = Vec3::new(0.0, 0.0, 8.0);
const BOX_SIZE: f32 = 2.0;

/// A 2 m box flown 8 m over open ground under a 6 degree sun.
fn frame(eye: Vec3, target: Vec3) -> Frame {
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(SUN_ELEVATION),
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
        id: "sun-shadow-orbit".into(),
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
    let mesh = frame.meshes.len();
    frame
        .meshes
        .push(crate::common::cube("::sun-shadow-orbit-cube"));
    let at = frame.draws.len() - frame.transparent.len();
    frame.draws.insert(
        at,
        Draw {
            strobe: luma_render::strobe::Rows::STEADY,
            mesh,
            model: Mat4::from_translation(BOX_CENTRE) * Mat4::from_scale(Vec3::splat(BOX_SIZE)),
            material: Material {
                base_color: Vec3::splat(0.5),
                roughness: 0.8,
                ..Default::default()
            },
            textures: MaterialTextures::default(),
            editor_object: None,
        },
    );
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
fn a_flown_box_shadows_the_ground_from_every_orbit_position() {
    let mut renderer = Renderer::new().unwrap();
    let sun = frame(Vec3::new(0.0, -10.0, 2.0), Vec3::ZERO)
        .directional
        .expect("an outdoor room has a sun")
        .direction
        .normalize();
    // Where the box's centre falls on the ground, along the sun.
    let shadow = BOX_CENTRE - sun * (BOX_CENTRE.z / sun.z);
    let mut failures = Vec::new();
    // Orbit the shadow at three distances, so it lands in each cascade. The
    // far cascade's texels and filter leave up to about 0.16 of sun in the
    // middle of a 2 m shadow; a clipped caster leaves all of it.
    for distance in [6.0_f32, 20.0, 70.0] {
        for step in 0..12 {
            let angle = step as f32 * std::f32::consts::TAU / 12.0;
            let eye = shadow
                + Vec3::new(angle.cos(), angle.sin(), 0.0) * distance
                + Vec3::Z * (distance * 0.5);
            let frame = frame(eye, shadow);
            let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
            let visibility = shadow_at(&pixels, pixel(&frame, shadow));
            if visibility > 0.2 {
                failures.push(format!(
                    "{distance} m at {:.0} deg: sun visibility {visibility:.2}",
                    angle.to_degrees()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the ground under the box's shadow is sunlit from some orbit positions:\n{}",
        failures.join("\n")
    );
}
