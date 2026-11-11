//! A house lamp hangs in a body, and its face is as bright as the light it
//! throws at whoever looks at it.
//!
//! An empty room with the house lights up, haze off, at a fixed exposure.
//! Set `LUMA_RENDER_CAPTURE_DIR` to write the renders beside the assertions.
use std::collections::BTreeMap;

use glam::Vec3;
use luma_render::{
    assets::Library,
    build_frame_with,
    frame::Camera,
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene},
    Frame, Renderer,
};
use luma_scene::environment::{VenueEnvironment, VenueHaze};

use crate::common::capture;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;

/// The room with its house at `level`.
fn room(level: f32) -> Frame {
    let mut render = RenderSettings::room(
        VenueEnvironment::indoor(level),
        VenueHaze::default(),
        20.0,
        1.0,
    );
    render.show_grid = false;
    render.show_gizmos = false;
    render.show_cables = false;
    render.haze.enabled = false;
    render.debug_view = DebugView::Pbr;
    let scene = Scene {
        id: "house-lamp".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, 1.6, 8.0],
            target: [0.0; 3],
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
        &mut Library::new(luma_scene::meshes_root()),
    )
    .unwrap();
    frame.haze_density = 0.0;
    frame
}

/// The draws of the house lamps' bodies.
fn bodies(frame: &Frame) -> usize {
    frame
        .draws
        .iter()
        .filter(|draw| frame.meshes[draw.mesh].key.starts_with("::house-lamp"))
        .count()
}

/// The brightest the lamp is in the picture, 0 to 255, looking at `lamp` from
/// three metres off at `off_axis_deg` from straight under it.
fn seen(
    renderer: &mut Renderer,
    frame: &mut Frame,
    lamp: Vec3,
    off_axis_deg: f32,
    name: &str,
) -> f32 {
    let (sin, cos) = off_axis_deg.to_radians().sin_cos();
    frame.camera = Camera {
        eye: lamp + Vec3::new(sin, 0.0, -cos) * 3.0,
        target: lamp,
        fov_y_deg: 20.0,
    };
    let rgba = renderer.render(frame, WIDTH, HEIGHT, 1).unwrap();
    capture(name, &rgba, WIDTH, HEIGHT);
    // The lamp and a little around it: its face fills the middle from here.
    let mut brightest = 0.0_f32;
    for y in HEIGHT / 2 - 12..HEIGHT / 2 + 12 {
        for x in WIDTH / 2 - 12..WIDTH / 2 + 12 {
            let i = ((y * WIDTH + x) * 4) as usize;
            brightest = brightest
                .max((f32::from(rgba[i]) + f32::from(rgba[i + 1]) + f32::from(rgba[i + 2])) / 3.0);
        }
    }
    brightest
}

/// The bodies hang there at blackout as with the house up, and a body is
/// the lamp's own: it leaves the lamp's light as it was.
#[test]
fn the_bodies_hang_whether_the_house_is_lit_or_not() {
    let (dark, lit) = (room(0.0), room(1.0));
    assert!(dark.fixture_cones.is_empty() && !lit.fixture_cones.is_empty());
    assert!(bodies(&lit) > 0, "the house lamps have bodies");
    assert_eq!(bodies(&dark), bodies(&lit));
    assert_eq!(bodies(&lit) % lit.fixture_cones.len(), 0, "a body a lamp");
}

/// From inside its beam a lit lamp's face is white; from beside it, outside
/// the beam, the lamp is dark, face, rim and body, as a downlight is. Dark,
/// the lamp shows no light from anywhere.
#[test]
fn a_lamps_face_is_bright_from_inside_its_beam_and_dark_beside_it() {
    let mut renderer = Renderer::new().unwrap();
    let mut lit = room(1.0);
    let lamp = lit.fixture_cones[0].position;
    let inside = seen(&mut renderer, &mut lit, lamp, 10.0, "inside");
    assert!(inside > 240.0, "the face from inside the beam is {inside}");
    for off_axis in [50.0, 60.0, 70.0, 80.0, 88.0] {
        let name = format!("beside-{off_axis}");
        let beside = seen(&mut renderer, &mut lit, lamp, off_axis, &name);
        assert!(
            beside < inside / 4.0,
            "from {off_axis} degrees beside it the lamp is {beside}, from inside {inside}"
        );
    }
    let mut dark = room(0.0);
    let off = seen(&mut renderer, &mut dark, lamp, 10.0, "off");
    assert!(off < inside / 4.0, "dark, the face is {off}");
}
