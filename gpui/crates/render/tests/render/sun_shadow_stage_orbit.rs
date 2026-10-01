//! Sun shadows of a whole stage under a low sun, from an orbiting camera.
//!
//! Every ground point the camera sees must be in shadow exactly when the ray
//! from it to the sun hits the stage, from every camera position. A 4 degree
//! sun throws a 9 m tower's shadow over 100 m, so this checks far more of the
//! light's depth range than the view slice around the camera.
use std::collections::BTreeMap;

use glam::{Mat4, Vec3, Vec4};
use luma_render::{
    assets::{Library, Material},
    build_frame_with,
    frame::{Draw, MaterialTextures},
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene, VenueEnvironment, VenueHaze},
    Frame, Renderer,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const SUN_ELEVATION: f32 = 4.0;

/// `(centre, size)` of each box: a deck, two towers, a flown truss, a crowd
/// barrier and a row of fixtures on the deck.
fn stage() -> Vec<(Vec3, Vec3)> {
    let mut boxes = vec![
        (Vec3::new(0.0, 0.0, 0.5), Vec3::new(10.0, 6.0, 1.0)),
        (Vec3::new(-7.0, 0.0, 4.5), Vec3::new(0.4, 0.4, 9.0)),
        (Vec3::new(7.0, 0.0, 4.5), Vec3::new(0.4, 0.4, 9.0)),
        (Vec3::new(0.0, 0.0, 8.0), Vec3::new(14.0, 0.4, 0.4)),
        (Vec3::new(0.0, -8.0, 0.55), Vec3::new(12.0, 0.1, 1.1)),
    ];
    for i in 0..6 {
        boxes.push((
            Vec3::new(-4.0 + 1.6 * i as f32, 2.0, 1.25),
            Vec3::splat(0.5),
        ));
    }
    boxes
}

fn frame(eye: Vec3, target: Vec3, azimuth: f32) -> Frame {
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(SUN_ELEVATION).with_sun_azimuth(azimuth),
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
        id: "sun-shadow-stage-orbit".into(),
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
        .push(crate::common::cube("::sun-shadow-stage-cube"));
    let at = frame.draws.len() - frame.transparent.len();
    let draws: Vec<_> = stage()
        .into_iter()
        .map(|(centre, size)| Draw {
            mesh,
            model: Mat4::from_translation(centre) * Mat4::from_scale(size),
            material: Material {
                base_color: Vec3::splat(0.5),
                roughness: 0.8,
                ..Default::default()
            },
            textures: MaterialTextures::default(),
            editor_object: None,
            strobe: luma_render::strobe::Rows::STEADY,
        })
        .collect();
    frame.draws.splice(at..at, draws);
    frame
}

/// Whether the segment from `from` along `direction` (up to `length`) hits a box.
fn hits(from: Vec3, direction: Vec3, length: f32) -> bool {
    stage().iter().any(|&(centre, size)| {
        let lo = centre - size * 0.5;
        let hi = centre + size * 0.5;
        let inv = direction.recip();
        let a = (lo - from) * inv;
        let b = (hi - from) * inv;
        let enter = a.min(b).max_element();
        let exit = a.max(b).min_element();
        enter <= exit && exit > 0.0 && enter < length
    })
}

fn project(frame: &Frame, p: Vec3) -> Option<(u32, u32)> {
    let projection = Mat4::perspective_rh(
        frame.camera.fov_y_deg.to_radians(),
        WIDTH as f32 / HEIGHT as f32,
        5000.0,
        0.1,
    );
    let view = Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z);
    let clip = projection * view * Vec4::new(p.x, p.y, p.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    let x = (ndc.x * 0.5 + 0.5) * WIDTH as f32;
    let y = (0.5 - ndc.y * 0.5) * HEIGHT as f32;
    (x >= 3.0 && y >= 3.0 && x < WIDTH as f32 - 3.0 && y < HEIGHT as f32 - 3.0)
        .then_some((x as u32, y as u32))
}

#[test]
fn a_stage_under_a_low_sun_shadows_the_ground_the_same_from_every_orbit_position() {
    let mut renderer = Renderer::new().unwrap();
    let target = Vec3::new(0.0, 0.0, 1.0);
    let mut failures = Vec::new();
    for azimuth in [0.0_f32, 135.0] {
        let sun = frame(Vec3::new(0.0, -30.0, 10.0), target, azimuth)
            .directional
            .expect("an outdoor room has a sun")
            .direction
            .normalize();
        for distance in [20.0_f32, 45.0, 80.0] {
            for step in 0..12 {
                let angle = step as f32 * std::f32::consts::TAU / 12.0;
                let eye = target
                    + Vec3::new(angle.cos(), angle.sin(), 0.0) * distance
                    + Vec3::Z * (distance * 0.3);
                let forward = (target - eye).normalize();
                let frame = frame(eye, target, azimuth);
                let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
                // Indexed by whether the point should be in shadow.
                let mut wrong = [0_u32; 2];
                let mut checked = [0_u32; 2];
                let mut example = None;
                for gy in -150..=150 {
                    for gx in -150..=150 {
                        let p = Vec3::new(gx as f32, gy as f32, 0.0);
                        let depth = (p - eye).dot(forward);
                        if !(1.0..170.0).contains(&depth) {
                            continue;
                        }
                        let Some((x, y)) = project(&frame, p) else {
                            continue;
                        };
                        let to_eye = eye - p;
                        if hits(p, to_eye.normalize(), to_eye.length()) {
                            continue;
                        }
                        // Stay clear of shadow edges by the filter's width.
                        // Across the sun that is a few texels; along it, the
                        // grazing sun stretches each texel 14 times. A 2 m
                        // penumbra there is also what the real sun's disc
                        // gives a 1 m deck under a 4 degree sun.
                        let margin = 0.2 + 0.006 * depth;
                        let along = sun.truncate().normalize().extend(0.0) * (1.0 + 0.02 * depth);
                        let expected: Vec<bool> = [
                            Vec3::ZERO,
                            Vec3::X * margin,
                            -Vec3::X * margin,
                            Vec3::Y * margin,
                            -Vec3::Y * margin,
                            along,
                            -along,
                        ]
                        .iter()
                        .map(|offset| hits(p + *offset + Vec3::Z * 0.01, sun, 1000.0))
                        .collect();
                        if expected.iter().any(|e| *e != expected[0]) {
                            continue;
                        }
                        let visibility = f32::from(pixels[((y * WIDTH + x) * 4) as usize]) / 255.0;
                        let shadowed = expected[0];
                        checked[usize::from(shadowed)] += 1;
                        let ok = if shadowed {
                            visibility < 0.35
                        } else {
                            visibility > 0.65
                        };
                        if !ok {
                            wrong[usize::from(shadowed)] += 1;
                            example.get_or_insert((p, shadowed, visibility));
                        }
                    }
                }
                // A clipped caster or a receiver offset that moves with the
                // cascade left 50 to 75 % of the shadowed points lit from
                // some positions. What is left is texel stairs on the edges.
                if wrong[1] * 4 > checked[1] || wrong[0] * 50 > checked[0] {
                    failures.push(format!(
                        "sun azimuth {azimuth}, {distance} m at {:.0} deg: wrong {}/{} \
                         shadowed and {}/{} lit ground points, e.g. {example:?}",
                        angle.to_degrees(),
                        wrong[1],
                        checked[1],
                        wrong[0],
                        checked[0],
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "sun shadows on the ground depend on the camera:\n{}",
        failures.join("\n")
    );
}
