//! The camera look: exposure, lens glow and glare (`post.rs`).
//!
//! ```sh
//! cargo test -p luma-render --test post_look -- --test-threads=1
//! ```
//!
//! One light aimed at the camera over a dark stage. Each test renders the
//! same frame under two looks and compares the pictures.

use std::collections::BTreeMap;
use std::path::PathBuf;

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::frame::FixtureCone;
use luma_render::scene_desc::{
    CameraPose, DebugView, Environment, Exposure, Glare, GlareStyle, Look, Piece, Quality,
    RenderSettings, Scene, ToneCurve,
};
use luma_render::{build_frame_with, Frame, Renderer};

const WIDTH: u32 = 192;
const HEIGHT: u32 = 128;
const EYE: Vec3 = Vec3::new(0.0, -8.0, 1.6);

/// A dark stage, `haze` and one beam from the stage's centre. `aim` is how
/// far the beam is turned from straight up toward the camera: 1 puts the
/// camera on its axis.
fn frame(intensity: f32, aim: f32, look: Look) -> Frame {
    hazy_frame(intensity, aim, 0.05, look)
}

fn hazy_frame(intensity: f32, aim: f32, haze: f32, look: Look) -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut render = RenderSettings::dark_stage(40.0, 1.0);
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.show_gizmos = false;
    render.haze.enabled = true;
    render.haze.steps = 8;
    render.haze.density = haze;
    render.haze.enabled = haze > 0.0;
    render.debug_view = DebugView::Pbr;
    render.look = look;
    let scene = Scene {
        id: "post-look".into(),
        times: vec![0.0],
        // Three-space: y is up.
        camera: CameraPose {
            position: [EYE.x, EYE.z, -EYE.y],
            target: [0.0, 1.6, 0.0],
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
    let mut frame =
        build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap();
    let position = Vec3::new(0.0, 0.0, 1.6);
    let to_eye = (frame.camera.eye - position).normalize();
    frame.fixture_cones = vec![FixtureCone {
        position,
        range: 24.0,
        direction: Vec3::Z.lerp(to_eye, aim).normalize(),
        cos_beam: 0.985,
        color: Vec3::new(0.2, 0.5, 1.0),
        intensity,
        cos_field: 0.96,
        wash: 0.0,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: luma_render::luminaire::Lens { radius: 0.05 },
    }];
    frame
}

fn render(renderer: &mut Renderer, frame: &Frame) -> Vec<u8> {
    let pixels = renderer.render(frame, WIDTH, HEIGHT, 1).unwrap();
    dump(&pixels);
    pixels
}

/// `POST_LOOK_DUMP=dir` writes every rendered picture there, numbered.
fn dump(pixels: &[u8]) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    if let Some(dir) = std::env::var_os("POST_LOOK_DUMP") {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(dir).join(format!("{n:02}.png"));
        luma_render::image_out::write(&path, pixels, WIDTH, HEIGHT).unwrap();
    }
}

/// Where the lens is on screen: the brightest pixel of a frame with it in.
fn lens_pixel(pixels: &[u8]) -> (u32, u32) {
    let (mut best, mut at) = (0u32, 0usize);
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let sum = u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]);
        if sum > best {
            best = sum;
            at = i;
        }
    }
    (at as u32 % WIDTH, at as u32 / WIDTH)
}

fn luma(pixels: &[u8], x: u32, y: u32) -> f32 {
    let i = ((y * WIDTH + x) * 4) as usize;
    0.2126 * f32::from(pixels[i])
        + 0.7152 * f32::from(pixels[i + 1])
        + 0.0722 * f32::from(pixels[i + 2])
}

/// Mean display luminance of a ring of pixels at `radius` around `centre`.
fn ring(pixels: &[u8], centre: (u32, u32), radius: f32) -> f32 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for step in 0..32 {
        let angle = step as f32 / 32.0 * std::f32::consts::TAU;
        let x = centre.0 as f32 + radius * angle.cos();
        let y = centre.1 as f32 + radius * angle.sin();
        if x >= 0.0 && y >= 0.0 && x < WIDTH as f32 && y < HEIGHT as f32 {
            sum += luma(pixels, x as u32, y as u32);
            n += 1.0;
        }
    }
    sum / n
}

fn mean(pixels: &[u8]) -> f32 {
    pixels
        .chunks_exact(4)
        .map(|px| 0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]))
        .sum::<f32>()
        / (WIDTH * HEIGHT) as f32
}

fn manual(glare: Glare) -> Look {
    Look {
        tone: ToneCurve::Agx,
        exposure: Exposure {
            auto: false,
            ..Exposure::STAGE
        },
        glare,
    }
}

/// A beam aimed into the camera shows its lens white-hot, and its glare
/// spreads over the dark around it; off the axis the lens is barely there.
#[test]
fn a_lens_in_its_beam_glares() {
    let mut renderer = Renderer::new().unwrap();
    // No haze: the lens and its glare are all there is to see.
    let glaring = render(
        &mut renderer,
        &hazy_frame(0.5, 1.0, 0.0, manual(Glare::STAGE)),
    );
    let plain = render(
        &mut renderer,
        &hazy_frame(0.5, 1.0, 0.0, manual(Glare::OFF)),
    );
    let lens = lens_pixel(&glaring);
    assert!(
        luma(&glaring, lens.0, lens.1) > 245.0,
        "the lens on its beam's axis is white: {}",
        luma(&glaring, lens.0, lens.1)
    );
    // Two degrees out.
    let halo = ring(&glaring, lens, 6.0);
    let without = ring(&plain, lens, 6.0);
    assert!(
        halo > without + 10.0,
        "glare lights the dark around the lens: {halo} against {without} without it"
    );

    // Turned away, the same lens is a fraction of that.
    let away = render(
        &mut renderer,
        &hazy_frame(0.5, 0.0, 0.0, manual(Glare::STAGE)),
    );
    let away_halo = ring(&away, lens, 6.0);
    assert!(
        away_halo < halo * 0.5,
        "the glow falls off out of the beam: {away_halo} against {halo} on axis"
    );
}

/// The veil is a long power-law tail: from one lens it still lights the dark
/// a quarter of the frame away, and it falls smoothly all the way out, with no
/// bright ring where a kernel or a fade might end.
#[test]
fn the_veil_reaches_far_without_a_ring() {
    let mut renderer = Renderer::new().unwrap();
    let bloom = Glare {
        style: GlareStyle::Bloom,
        ..Glare::STAGE
    };
    let glaring = render(&mut renderer, &hazy_frame(0.5, 1.0, 0.0, manual(bloom)));
    let plain = render(
        &mut renderer,
        &hazy_frame(0.5, 1.0, 0.0, manual(Glare::OFF)),
    );
    let lens = lens_pixel(&glaring);
    let far = ring(&glaring, lens, 30.0);
    assert!(
        far > ring(&plain, lens, 30.0) + 1.0,
        "the veil reaches 30 px out: {far}"
    );
    let profile: Vec<f32> = (3..60).map(|r| ring(&glaring, lens, r as f32)).collect();
    for (i, pair) in profile.windows(2).enumerate() {
        assert!(
            pair[1] <= pair[0] + 0.5,
            "the veil rises at {} px: {profile:?}",
            i + 4
        );
    }
}

/// Low quality convolves on a coarser grid; the glare around a lens is much
/// the same.
#[test]
fn low_quality_glares_alike() {
    let mut renderer = Renderer::new().unwrap();
    let high = render(
        &mut renderer,
        &hazy_frame(0.5, 1.0, 0.0, manual(Glare::STAGE)),
    );
    let mut frame = hazy_frame(0.5, 1.0, 0.0, manual(Glare::STAGE));
    frame.quality = Quality::Low;
    let low = render(&mut renderer, &frame);
    let lens = lens_pixel(&high);
    for radius in [4.0, 8.0, 16.0] {
        let (h, l) = (ring(&high, lens, radius), ring(&low, lens, radius));
        assert!(
            (h - l).abs() < 0.3 * h.max(l) + 2.0,
            "at {radius} px: high {h}, low {l}"
        );
    }
}

/// With glare off and a neutral tone curve, manual 0 EV through the post
/// chain draws what the single composite pass draws.
#[test]
fn the_post_chain_at_rest_is_the_composite_picture() {
    let mut renderer = Renderer::new().unwrap();
    let single = render(&mut renderer, &frame(1.0, 0.3, Look::NEUTRAL));
    // Not NEUTRAL, so the chain runs; no glare, no metering, same curve.
    let chained = render(
        &mut renderer,
        &frame(
            1.0,
            0.3,
            Look {
                exposure: Exposure {
                    min_ev: -3.0,
                    ..Look::NEUTRAL.exposure
                },
                ..Look::NEUTRAL
            },
        ),
    );
    // Half-float storage and the chain's dither move a code value or two.
    let worst = single
        .iter()
        .zip(&chained)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(
        worst <= 3,
        "the chain changed a pixel by {worst} code values"
    );
}

/// A lit room follows the light: four times the light meters about two stops
/// down. A dark room with one thin beam is exposed for what is lit and held
/// at the top clamp rather than lifted until the room is grey.
#[test]
fn auto_exposure_meters_the_lit_part_of_the_frame() {
    let mut renderer = Renderer::new().unwrap();
    let auto = Look {
        glare: Glare::OFF,
        ..Look::STAGE
    };
    let mut metered = |intensity: f32, haze: f32, look: Look| {
        let _ = render(&mut renderer, &hazy_frame(intensity, 0.4, haze, look));
        renderer.metered_exposure().unwrap()[0]
    };
    let dim = metered(1.0, 0.3, auto);
    let bright = metered(4.0, 0.3, auto);
    assert!(
        (1.5..=2.5).contains(&(dim - bright)),
        "four times the light meters about two stops down: {dim} then {bright}"
    );

    let dark_room = metered(1.0, 0.05, auto);
    assert_eq!(
        dark_room,
        Exposure::STAGE.max_ev,
        "a dark room stops at the clamp"
    );

    // Compensation is added after the clamp, so it still moves a dark room.
    let lifted = metered(
        1.0,
        0.05,
        Look {
            exposure: Exposure {
                ev: 1.0,
                ..auto.exposure
            },
            ..auto
        },
    );
    assert_eq!(lifted, Exposure::STAGE.max_ev + 1.0);

    // Manual exposure is the number itself.
    let half_stop = Look {
        exposure: Exposure {
            auto: false,
            ev: 0.5,
            ..Exposure::STAGE
        },
        ..auto
    };
    assert_eq!(metered(4.0, 0.3, half_stop), 0.5);
}

/// Live frames adapt over time: a light that suddenly brightens is first
/// seen through the exposure the dim frame metered, and the exposure only
/// then moves toward the bright frame's.
#[test]
fn live_exposure_adapts_rather_than_jumps() {
    let mut renderer = Renderer::new().unwrap();
    let auto = Look {
        glare: Glare::OFF,
        ..Look::STAGE
    };
    // A standalone frame snaps.
    let _ = render(&mut renderer, &hazy_frame(1.0, 0.4, 0.3, auto));
    let [dim, _] = renderer.metered_exposure().unwrap();
    let _ = renderer
        .render_next(&hazy_frame(4.0, 0.4, 0.3, auto), WIDTH, HEIGHT, 1)
        .unwrap();
    let [current, target] = renderer.metered_exposure().unwrap();
    assert!(
        target < dim - 1.5,
        "the bright frame meters lower: {target} against {dim}"
    );
    assert!(
        current > target + 1.0 && current <= dim,
        "one live frame later the exposure is still on its way: {current}, from {dim} to {target}"
    );
}
