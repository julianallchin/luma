//! A face of cells lights what is close to it in each cell's own colour, and
//! what is far from it in their mean.
//!
//! One light lies along the floor, as a wall washer grazes a wall: a panel
//! of eight cells, red on one half and blue on the other. Set
//! `LUMA_RENDER_CAPTURE_DIR` to write the renders beside the assertions.
use std::collections::BTreeMap;

use glam::{Vec2, Vec3};
use luma_render::{
    assets::Library,
    build_frame_with,
    frame::FixtureCone,
    panel::{Cell, Footprint, Panel},
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

/// How the eight cells light the floor.
#[derive(Clone, Copy, PartialEq)]
enum Lit {
    /// One light: the panel, with the cells as its texture.
    Panel,
    /// One cone of the cells' mean colour, and no cells.
    Cone,
    /// A cone a cell: what the panel stands for.
    Cells,
}

/// The floor seen from above `target`. `flip` swaps the red half and the
/// blue.
fn frame(target: Vec3, flip: bool, lit: Lit) -> Frame {
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
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: Lens { radius: 0.03 },
    }];
    let (red, blue) = (Vec3::new(2.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 2.0));
    let half = Vec2::new(0.3, 0.03);
    let cells: Vec<Cell> = (0..8)
        .map(|column| Cell {
            at: Vec2::new((column as f32 - 3.5) * 0.075, 0.0),
            size: Vec2::new(0.075, 0.06),
            colour: if (column < 4) != flip { red } else { blue },
            intensity: 1.0,
        })
        .collect();
    match lit {
        Lit::Cone => {}
        Lit::Panel => {
            // Eight cells in a row, 60 cm across and 6 cm tall.
            frame.fixture_cones[0].lens = Lens { radius: 0.31 };
            frame.panels = vec![Panel::new(
                0,
                Vec3::X,
                (-half, half),
                0.06,
                (&cells, cells.len()),
                Footprint::of(cone.cos_beam, cone.cos_field),
            )];
        }
        Lit::Cells => {
            let whole = frame.fixture_cones[0];
            frame.fixture_cones = cells
                .iter()
                .map(|cell| FixtureCone {
                    position: whole.position + Vec3::X * cell.at.x,
                    color: cell.colour,
                    intensity: whole.intensity / 8.0,
                    ..whole
                })
                .collect();
        }
    }
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
    let one_way = lean(&mut renderer, &frame(near, false, Lit::Panel), "near");
    let other_way = lean(
        &mut renderer,
        &frame(near, true, Lit::Panel),
        "near-flipped",
    );
    let plain = lean(&mut renderer, &frame(near, false, Lit::Cone), "near-plain");
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

/// Metres off, the floor is lit by the whole face: nearly the cells' mean,
/// and what a cone a cell gives. The two halves are 30 cm apart, so their
/// colours still lean a little each way, as theirs do.
#[test]
fn the_floor_far_from_the_face_is_lit_as_a_cone_a_cell_lights_it() {
    let mut renderer = Renderer::new().unwrap();
    let far = Vec3::new(0.0, 3.0, 0.0);
    let near = Vec3::new(0.0, 0.12, 0.0);
    let close = lean(&mut renderer, &frame(near, false, Lit::Panel), "close");
    let panel = lean(&mut renderer, &frame(far, false, Lit::Panel), "far");
    let cells = lean(&mut renderer, &frame(far, false, Lit::Cells), "far-cells");
    assert!(
        panel.abs() < 0.3 * close.abs(),
        "the far floor leans {panel}, the near floor {close}"
    );
    assert!((panel - cells).abs() < 0.01, "panel {panel}, cells {cells}");
}
