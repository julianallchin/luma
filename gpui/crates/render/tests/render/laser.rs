//! Laser projectors in haze (`laser.rs`).
//!
//! ```sh
//! cargo test -p luma-render --test render laser:: -- --test-threads=1
//! ```
//!
//! One projector at stage left scanning across in front of the camera, over
//! a dark stage. `LUMA_LASER_CAPTURE_DIR` writes each picture as a PNG.

use std::collections::BTreeMap;
use std::path::PathBuf;

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::laser::{LaserPoint, LaserScan};
use luma_render::scene_desc::{
    CameraPose, DebugView, Environment, Look, Piece, RenderSettings, Scene,
};
use luma_render::{build_frame_with, Frame, Renderer};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 160;
const EYE: Vec3 = Vec3::new(0.0, -10.0, 1.6);

fn frame(haze: f32) -> Frame {
    let mut library = Library::new(crate::common::meshes());
    let mut render = RenderSettings::dark_stage(40.0, 1.0);
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.show_gizmos = false;
    render.haze.steps = 8;
    render.haze.density = haze;
    render.haze.enabled = haze > 0.0;
    render.debug_view = DebugView::Pbr;
    render.look = Look::STAGE;
    let scene = Scene {
        id: "laser".into(),
        times: vec![0.0],
        // Three-space: y is up.
        camera: CameraPose {
            position: [EYE.x, EYE.z, -EYE.y],
            target: [0.0, 2.0, 0.0],
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: Vec::<Piece>::new(),
        state: BTreeMap::new(),
    };
    build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap()
}

/// A projector at stage left, 1 m up, scanning toward stage right.
fn projector(points: Vec<LaserPoint>) -> LaserScan {
    LaserScan {
        origin: Vec3::new(-6.0, 0.0, 1.0),
        forward: Vec3::X,
        right: Vec3::Y,
        up: Vec3::Z,
        half_field: [30.0, 30.0],
        rate: 30_000.0,
        points,
        phase: 0.0,
    }
}

fn point(x: f32, y: f32, lit: bool) -> LaserPoint {
    LaserPoint {
        x,
        y,
        rgb: if lit { [0.1, 1.0, 0.2] } else { [0.0; 3] },
    }
}

/// `count` beams fanned up the field, each dwelt on for `dwell` points,
/// jumps in black.
fn fan(count: usize, dwell: usize) -> Vec<LaserPoint> {
    let mut points = Vec::new();
    for i in 0..count {
        let y = i as f32 / (count - 1) as f32 * 0.6 - 0.1;
        points.extend(std::iter::repeat_n(point(0.0, y, false), 24));
        points.extend(std::iter::repeat_n(point(0.0, y, true), dwell));
    }
    points
}

fn render(renderer: &mut Renderer, frame: &Frame, name: &str) -> Vec<u8> {
    let pixels = renderer.render(frame, WIDTH, HEIGHT, 1).unwrap();
    if let Some(dir) = std::env::var_os("LUMA_LASER_CAPTURE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        let path = PathBuf::from(dir).join(format!("{name}.png"));
        luma_render::image_out::write(&path, &pixels, WIDTH, HEIGHT).unwrap();
    }
    pixels
}

fn brightness(pixels: &[u8]) -> u64 {
    pixels
        .chunks_exact(4)
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum()
}

/// The brightest green in one column of the picture.
fn column_peak(pixels: &[u8], column: u32) -> u8 {
    (0..HEIGHT)
        .map(|row| pixels[((row * WIDTH + column) * 4 + 1) as usize])
        .max()
        .unwrap_or(0)
}

#[test]
fn beams_show_in_haze_and_not_in_clear_air() {
    let mut renderer = Renderer::new().unwrap();
    let mut hazy = frame(0.6);
    let dark = render(&mut renderer, &hazy, "hazy-dark");
    hazy.lasers = vec![projector(fan(5, 120))];
    let lit = render(&mut renderer, &hazy, "hazy-fan");
    assert!(
        brightness(&lit) > brightness(&dark) + 2_000,
        "{} vs {}",
        brightness(&lit),
        brightness(&dark)
    );
    let mut clear = frame(0.0);
    let before = render(&mut renderer, &clear, "clear-dark");
    clear.lasers = vec![projector(fan(5, 120))];
    let after = render(&mut renderer, &clear, "clear-fan");
    assert_eq!(
        brightness(&after),
        brightness(&before),
        "with nothing to scatter off, a beam is invisible"
    );
}

/// The same lit time on one direction or swept across the field: the dwell
/// stacks its light into a bright line, the sweep spreads it into a dim
/// sheet.
#[test]
fn a_dwell_is_brighter_than_a_sweep() {
    let mut renderer = Renderer::new().unwrap();
    let mut beam = frame(0.6);
    let mut points = vec![point(0.0, 0.2, false); 24];
    points.extend(vec![point(0.0, 0.2, true); 400]);
    beam.lasers = vec![projector(points)];
    let beam = render(&mut renderer, &beam, "dwell");
    let mut sheet = frame(0.6);
    let sweep: Vec<LaserPoint> = (0..400)
        .map(|i| point(0.0, i as f32 / 400.0 * 0.8 - 0.2, true))
        .collect();
    sheet.lasers = vec![projector(sweep)];
    let sheet = render(&mut renderer, &sheet, "sweep");
    // Compared well out from the projector, where neither saturates: right
    // at the source a sheet's light per metre grows without bound.
    let column = WIDTH * 3 / 4;
    let (beam, sheet) = (column_peak(&beam, column), column_peak(&sheet, column));
    assert!(beam > sheet, "{beam} vs {sheet}");
    assert!(sheet > 0, "a sheet shows");
}

#[test]
fn a_projector_with_no_light_draws_nothing() {
    let mut renderer = Renderer::new().unwrap();
    let mut f = frame(0.6);
    let dark = render(&mut renderer, &f, "unlit-before");
    f.lasers = vec![projector(vec![point(0.0, 0.0, false); 100])];
    assert_eq!(render(&mut renderer, &f, "unlit"), dark);
}
