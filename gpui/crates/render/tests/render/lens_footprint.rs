//! A beam leaves a lens-sized disc, not a point.
//!
//! The haze column and the lit floor pool are one cone, lens-wide at the lens
//! and opening at the field angle from there; nothing scatters between the
//! cone's virtual apex and the lens. Set `LUMA_LENS_CAPTURE_DIR` to write the
//! renders beside the assertions.
use std::{collections::BTreeMap, path::PathBuf};

use glam::Vec3;
use luma_render::{
    assets::Library,
    build_frame_with,
    frame::FixtureCone,
    luminaire::Lens,
    scene_desc::{CameraPose, DebugView, RenderSettings, Scene, VenueEnvironment, VenueHaze},
    Frame, Renderer,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
const FOV_DEG: f32 = 40.0;

/// A 2° beam (4° field) from a 200 mm lens: the virtual apex sits 2.86 m
/// behind the glass, where the difference from a point source is largest.
const HALF_FIELD_DEG: f32 = 2.0;
const LENS: Lens = Lens { radius: 0.1 };

fn narrow(position: Vec3, direction: Vec3, lens: Lens) -> FixtureCone {
    let half = HALF_FIELD_DEG.to_radians();
    FixtureCone {
        position,
        range: 30.0,
        direction,
        cos_beam: (half * 0.6).cos(),
        color: Vec3::new(1.0, 0.25, 0.1),
        intensity: 1.0,
        cos_field: half.cos(),
        wash: 0.0,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens,
    }
}

/// `eye` and `target` are world space (Z-up).
fn frame(eye: Vec3, target: Vec3, haze: bool, debug_view: DebugView) -> Frame {
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(-12.0),
        VenueHaze::default(),
        FOV_DEG,
        1.0,
    );
    render.show_grid = false;
    render.show_gizmos = false;
    render.show_cables = false;
    render.fixture_shadows = false;
    render.haze.enabled = haze;
    render.haze.density = 0.6;
    render.debug_view = debug_view;
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "lens-footprint".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: three(eye),
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
    frame.haze_density = if haze { 0.6 } else { 0.0 };
    frame
}

fn capture(label: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_LENS_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{label}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
    }
}

fn luminance(pixels: &[u8], x: u32, y: u32) -> u8 {
    let i = ((y * WIDTH + x) * 4) as usize;
    pixels[i].max(pixels[i + 1]).max(pixels[i + 2])
}

/// Metres per pixel on a plane `depth` metres in front of the camera.
fn metres_per_pixel(depth: f32) -> f32 {
    2.0 * depth * (FOV_DEG.to_radians() / 2.0).tan() / HEIGHT as f32
}

/// Lit pixels along one image row, whose run is the pool's diameter.
fn lit_run(pixels: &[u8], y: u32, threshold: u8) -> u32 {
    (0..WIDTH)
        .filter(|&x| luminance(pixels, x, y) > threshold)
        .count() as u32
}

/// The lit floor pool is the beam's footprint: seen from the same virtual
/// apex, so a lens-sized source's pool is the point source's pool scaled by
/// `(H + L) / H`, and at the field edge it is `r + H·tan(half)` wide.
#[test]
fn the_lit_pool_is_the_beam_footprint() {
    const HEIGHT_M: f32 = 6.0;
    const CAMERA_M: f32 = 9.0;
    let mut renderer = Renderer::new().unwrap();
    // Straight down onto the floor; the tiny offset keeps look-at defined.
    let mut frame = frame(
        Vec3::new(0.0, -0.001, CAMERA_M),
        Vec3::ZERO,
        false,
        DebugView::Pbr,
    );
    let mut runs = Vec::new();
    for (label, lens) in [("point", Lens::POINT), ("lens", LENS)] {
        frame.fixture_cones = vec![narrow(Vec3::new(0.0, 0.0, HEIGHT_M), Vec3::NEG_Z, lens)];
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("floor-pool-{label}"), &pixels);
        let run = lit_run(&pixels, HEIGHT / 2, 8);
        assert!(run > 10, "{label}: no visible pool");
        runs.push(run as f32);
    }
    let lens_distance = narrow(Vec3::ZERO, Vec3::NEG_Z, LENS).lens_distance();
    let want = (HEIGHT_M + lens_distance) / HEIGHT_M;
    let got = runs[1] / runs[0];
    let scale = metres_per_pixel(CAMERA_M);
    let field_edge = LENS.radius + HEIGHT_M * HALF_FIELD_DEG.to_radians().tan();
    println!(
        "pool diameter: point {:.3} m, lens {:.3} m; ratio {got:.3} (model {want:.3}); \
         field-edge footprint radius r + H·tan = {field_edge:.3} m",
        runs[0] * scale,
        runs[1] * scale,
    );
    // Two pixels of edge quantisation on each run.
    let tolerance = 2.0 * (1.0 / runs[0] + 1.0 / runs[1]) * want;
    assert!(
        (got - want).abs() <= tolerance,
        "pool ratio {got}, model {want} ± {tolerance}"
    );
    assert!(
        runs[1] * scale / 2.0 <= field_edge + 2.0 * scale,
        "the pool may not outgrow the cone's field-edge footprint"
    );
}

/// Side view of a horizontal beam in haze. Past the lens plane the column is
/// already lens-wide; behind it, inside the virtual cone that converges on the
/// apex, nothing scatters.
#[test]
fn the_haze_beam_starts_lens_wide_at_the_lens() {
    const DEPTH_M: f32 = 2.5;
    let lens_at = Vec3::new(0.0, 0.0, 2.0);
    let mut renderer = Renderer::new().unwrap();
    let mut frame = frame(
        lens_at + Vec3::new(0.0, -DEPTH_M, 0.0),
        lens_at,
        true,
        DebugView::VolumetricAccumulation,
    );
    let scale = metres_per_pixel(DEPTH_M);
    let column = |x_m: f32| (WIDTH as f32 / 2.0 + x_m / scale) as u32;
    let mut past = Vec::new();
    for (label, lens) in [("point", Lens::POINT), ("lens", LENS)] {
        frame.fixture_cones = vec![narrow(lens_at, Vec3::X, lens)];
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 4).unwrap();
        capture(&format!("haze-root-{label}"), &pixels);
        let rows = |x_m: f32| {
            (0..HEIGHT)
                .filter(|&y| luminance(&pixels, column(x_m), y) > 4)
                .count() as f32
        };
        past.push(rows(0.03));
        if lens.radius > 0.0 {
            assert_eq!(
                rows(-0.2),
                0.0,
                "haze scattered behind the lens plane, inside the virtual cone"
            );
        }
    }
    let diameter = 2.0 * LENS.radius / scale;
    println!(
        "haze column 3 cm past the lens: point {:.0} px, lens {:.0} px (lens diameter {diameter:.0} px)",
        past[0], past[1]
    );
    assert!(past[0] < 0.3 * diameter, "a point source starts narrow");
    assert!(
        past[1] >= 0.8 * diameter && past[1] <= 1.1 * diameter + 2.0,
        "a lens source starts lens-wide: {} px against {diameter} px",
        past[1]
    );
}
