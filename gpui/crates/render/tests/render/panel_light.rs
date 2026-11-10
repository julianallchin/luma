//! A face of cells lights what is close to it in each cell's own colour, and
//! what is far from it in their mean.
//!
//! One cone lies along the floor, as a wall washer grazes a wall, with a
//! panel of eight cells behind it: red on one half, blue on the other. Set
//! `LUMA_RENDER_CAPTURE_DIR` to write the renders beside the assertions.
use std::collections::BTreeMap;

use glam::Vec3;
use luma_render::{
    assets::Library,
    build_frame_with,
    frame::FixtureCone,
    panel::Panel,
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene},
    Frame, Renderer,
};
use luma_scene::{
    environment::{VenueEnvironment, VenueHaze},
    luminaire::{cone_from_opening, Lens, LensClass, Luminaire, LUMENS_PER_UNIT},
};

use crate::common::capture;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// The floor seen from above `target`, lit by the cone alone. `flip` swaps
/// the panel's halves; `cells` leaves the panel off, for the cone's own
/// colour everywhere.
fn frame(target: Vec3, flip: bool, cells: bool) -> Frame {
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(-12.0),
        VenueHaze::default(),
        40.0,
        1.0,
    );
    render.show_grid = false;
    render.show_gizmos = false;
    render.show_cables = false;
    render.fixture_shadows = false;
    render.haze.enabled = false;
    render.debug_view = DebugView::Pbr;
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "panel-light".into(),
        times: vec![0.0],
        camera: CameraPose {
            // Straight down; the tiny offset keeps look-at defined.
            position: three(target + Vec3::new(0.0, -0.001, 1.0)),
            target: three(target),
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
    frame.haze_density = 0.0;
    // A wash, a hand's width above the floor and shining along it.
    let cone = cone_from_opening(Luminaire {
        field_angle_deg: 120.0,
        lumens: LensClass::Wash.output_lumens() / LUMENS_PER_UNIT,
    });
    frame.fixture_cones = vec![FixtureCone {
        position: Vec3::new(0.0, 0.0, 0.1),
        range: 30.0,
        direction: Vec3::Y,
        cos_beam: cone.cos_beam,
        color: Vec3::new(1.0, 0.0, 1.0),
        intensity: cone.gain,
        cos_field: cone.cos_field,
        wash: cone.wash,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: Lens { radius: 0.03 },
    }];
    let (red, blue) = (Vec3::new(2.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 2.0));
    frame.panels = if cells {
        vec![Panel {
            cone: 0,
            right: Vec3::X,
            half: [0.3, 0.03],
            optic: 0.03,
            grid: [8, 1],
            cells: (0..8)
                .map(|column| if (column < 4) != flip { red } else { blue })
                .collect(),
        }]
    } else {
        Vec::new()
    };
    frame
}

/// How much redder than blue the picture's left half is than its right:
/// the difference of `(red - blue) / (red + blue)` between the halves.
fn lean(renderer: &mut Renderer, frame: &Frame, name: &str) -> f32 {
    let rgba = renderer.render(frame, WIDTH, HEIGHT, 1).unwrap();
    capture(name, &rgba, WIDTH, HEIGHT);
    let half = |left: bool| {
        let (mut red, mut blue) = (0.0_f32, 0.0_f32);
        for (i, pixel) in rgba.chunks_exact(4).enumerate() {
            if (i as u32 % WIDTH < WIDTH / 2) == left {
                red += f32::from(pixel[0]);
                blue += f32::from(pixel[2]);
            }
        }
        assert!(red + blue > 0.0, "{name}: the floor is lit");
        (red - blue) / (red + blue)
    };
    half(true) - half(false)
}

/// The floor just in front of the face is red before the red cells and blue
/// before the blue ones, whichever side each is on; lit by the cone alone it
/// is one colour across.
#[test]
fn the_floor_before_the_face_takes_each_cells_colour() {
    let mut renderer = Renderer::new().unwrap();
    let near = Vec3::new(0.0, 0.12, 0.0);
    let one_way = lean(&mut renderer, &frame(near, false, true), "near");
    let other_way = lean(&mut renderer, &frame(near, true, true), "near-flipped");
    let plain = lean(&mut renderer, &frame(near, false, false), "near-plain");
    // A lean of two is one half all red and the other all blue. The picture
    // takes in the floor past the cells' reach too, so it leans less; a
    // tenth of the way is still a difference anyone would see.
    assert!(
        one_way.abs() > 0.2 && other_way.abs() > 0.2 && one_way * other_way < 0.0,
        "the halves lean {one_way} and, flipped, {other_way}"
    );
    assert!(
        plain.abs() < 0.1 * one_way.abs(),
        "the cone alone leans {plain}"
    );
}

/// Metres off, the floor is lit by the whole face: the cells' mean, the same
/// across, and the picture the cone alone gives.
#[test]
fn the_floor_far_from_the_face_takes_the_cells_mean() {
    let mut renderer = Renderer::new().unwrap();
    let far = Vec3::new(0.0, 3.0, 0.0);
    let near = Vec3::new(0.0, 0.12, 0.0);
    let close = lean(&mut renderer, &frame(near, false, true), "close");
    let cells = lean(&mut renderer, &frame(far, false, true), "far");
    let plain = lean(&mut renderer, &frame(far, false, false), "far-plain");
    assert!(
        cells.abs() < 0.05 * close.abs(),
        "the far floor leans {cells}, the near floor {close}"
    );
    assert!((cells - plain).abs() < 0.01, "cells {cells}, cone {plain}");
}
