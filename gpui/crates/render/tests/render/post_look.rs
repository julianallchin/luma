//! The camera look: exposure and glare (`post.rs`).
//!
//! ```sh
//! cargo test -p luma-render --test render post_look:: -- --test-threads=1
//! ```
//!
//! One light aimed at the camera over a dark stage, or the sun over open
//! ground. Each test renders the same frame under two looks and compares the
//! pictures.

use std::collections::BTreeMap;

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::frame::FixtureCone;
use luma_render::scene_desc::{
    CameraPose, DebugView, Environment, Exposure, Glare, GlareStyle, Look, Piece, Quality,
    RenderSettings, Scene, ToneCurve,
};
use luma_render::{build_frame_with, Frame, Renderer};
use luma_scene::environment::{VenueEnvironment, VenueHaze};

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
    let meshes = luma_scene::meshes_root();
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
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: luma_scene::luminaire::Lens { radius: 0.05 },
    }];
    frame
}

fn render(renderer: &mut Renderer, frame: &Frame) -> Vec<u8> {
    let pixels = renderer.render(frame, WIDTH, HEIGHT, 1).unwrap();
    capture_numbered(&pixels);
    pixels
}

/// Every rendered picture, numbered in render order.
fn capture_numbered(pixels: &[u8]) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    crate::common::capture(&format!("{n:02}"), pixels, WIDTH, HEIGHT);
}

/// Where the source is on screen: the brightest pixel of the frame.
fn brightest(pixels: &[u8]) -> (u32, u32) {
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

fn manual(glare: Glare) -> Look {
    Look {
        footage: luma_render::scene_desc::Footage::OFF,
        tone: ToneCurve::Agx,
        exposure: Exposure {
            auto: false,
            ..Exposure::STAGE
        },
        glare,
    }
}

/// The veil is a long power-law tail: from the sun it still lights the
/// frame well away from it, and it falls smoothly all the way out, with no
/// bright ring where a kernel or a fade might end.
#[test]
fn the_veil_reaches_far_without_a_ring() {
    let mut renderer = Renderer::new().unwrap();
    let bloom = Glare {
        style: GlareStyle::Bloom,
        ..Glare::STAGE
    };
    let glaring = render(&mut renderer, &sun_frame(0.0, manual(bloom)));
    let plain = render(&mut renderer, &sun_frame(0.0, manual(Glare::OFF)));
    let sun = brightest(&glaring);
    let far = ring(&glaring, sun, 30.0);
    assert!(
        far > ring(&plain, sun, 30.0) + 1.0,
        "the veil reaches 30 px out: {far}"
    );
    // From outside the sun's clipped core, where both pictures are white and
    // their difference says nothing about the veil.
    let profile: Vec<f32> = (6..60)
        .map(|r| ring(&glaring, sun, r as f32) - ring(&plain, sun, r as f32))
        .collect();
    for (i, pair) in profile.windows(2).enumerate() {
        assert!(
            pair[1] <= pair[0] + 0.5,
            "the veil rises at {} px: {profile:?}",
            i + 7
        );
    }
}

/// Medium quality convolves on a coarser grid; the glare around the sun is
/// much the same.
#[test]
fn medium_quality_glares_alike() {
    let mut renderer = Renderer::new().unwrap();
    let high = render(&mut renderer, &sun_frame(0.0, manual(Glare::STAGE)));
    let mut frame = sun_frame(0.0, manual(Glare::STAGE));
    frame.quality = Quality::Medium;
    let low = render(&mut renderer, &frame);
    let sun = brightest(&high);
    for radius in [4.0, 8.0, 16.0] {
        let (h, l) = (ring(&high, sun, radius), ring(&low, sun, radius));
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
                // A camera with no grain and a still hand: the chain's own
                // picture of the same light.
                footage: luma_render::scene_desc::Footage {
                    enabled: true,
                    noise: 0.0,
                    handheld: 0.0,
                    bass: 0.0,
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

/// A lit room follows the light most of the way: four times the light meters
/// down by less than its two stops, and what is left shows, so the brighter
/// room is the brighter picture. A dark room with one dim beam is exposed
/// for what is lit and opens no further for a dimmer one, rather than being
/// lifted until the room is grey.
#[test]
fn auto_exposure_meters_the_lit_part_of_the_frame() {
    let mut renderer = Renderer::new().unwrap();
    let auto = Look {
        glare: Glare::OFF,
        exposure: Exposure {
            auto: true,
            ..Exposure::STAGE
        },
        ..Look::STAGE
    };
    // The exposure a frame meters, and how bright its picture is.
    let mut shot = |intensity: f32, haze: f32, look: Look| {
        let pixels = render(&mut renderer, &hazy_frame(intensity, 0.4, haze, look));
        let light: f32 = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .map(|(x, y)| luma(&pixels, x, y))
            .sum();
        (renderer.metered_exposure().unwrap()[0], light)
    };
    let (dim, dim_picture) = shot(1.0, 0.3, auto);
    let (bright, bright_picture) = shot(4.0, 0.3, auto);
    assert!(
        (1.0..2.0).contains(&(dim - bright)),
        "four times the light meters most of two stops down: {dim} then {bright}"
    );
    assert!(
        bright_picture > 1.1 * dim_picture,
        "the brighter room is the brighter picture: {bright_picture} against {dim_picture}"
    );
    let mut metered = |intensity: f32, haze: f32, look: Look| shot(intensity, haze, look).0;

    // A cue almost out: the beam at a hundredth, in light haze. It is past
    // what the exposure opens for, so a dimmer one meters the same.
    let dark_room = metered(0.01, 0.05, auto);
    assert_eq!(
        dark_room,
        metered(0.005, 0.05, auto),
        "a dark room stops at the clamp"
    );

    // Compensation is added after the clamp, so it still moves a dark room.
    let lifted = metered(
        0.01,
        0.05,
        Look {
            exposure: Exposure {
                ev: 1.0,
                ..auto.exposure
            },
            ..auto
        },
    );
    assert_eq!(lifted, dark_room + 1.0);

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
        exposure: Exposure {
            auto: true,
            ..Exposure::STAGE
        },
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
        target < dim - 1.0,
        "the bright frame meters lower: {target} against {dim}"
    );
    assert!(
        current > (dim + target) / 2.0 && current <= dim,
        "one live frame later the exposure is still on its way: {current}, from {dim} to {target}"
    );
}

/// A light that arrives in an eye used to the dark shuts its pupil and, for
/// a while, no more: the exposure closes by the pupil's stops where the meter
/// asks for far more, and goes on closing slowly. The dark opens it again
/// all the way.
#[test]
fn a_sudden_light_closes_the_exposure_only_as_far_as_a_pupil() {
    let pupil = luma_render::photometry::PUPIL_STOPS;
    let auto = Look {
        glare: Glare::OFF,
        exposure: Exposure::STAGE,
        ..Look::STAGE
    };
    let dark = |time: f32| Frame {
        time,
        ..hazy_frame(0.01, 0.4, 0.05, auto)
    };
    // The same beam many thousand times brighter, down the camera's throat.
    let blinding = |time: f32| Frame {
        time,
        ..hazy_frame(4000.0, 1.0, 0.3, auto)
    };
    let mut still = Renderer::new().unwrap();
    let _ = render(&mut still, &blinding(0.0));
    let [asked, _] = still.metered_exposure().unwrap();

    let mut renderer = Renderer::new().unwrap();
    let _ = render(&mut renderer, &dark(0.0));
    let [used_to, _] = renderer.metered_exposure().unwrap();
    assert!(
        asked < used_to - pupil - 2.0,
        "the light asks for much more than a pupil closes: {asked} from {used_to}"
    );
    // Ten frames a second, the longest step one adaptation takes.
    let mut live = |frame: &dyn Fn(f32) -> Frame, seconds: std::ops::Range<u32>| {
        for tick in seconds.start * 10..seconds.end * 10 {
            let _ = renderer
                .render_next(&frame((tick + 1) as f32 / 10.0), WIDTH, HEIGHT, 1)
                .unwrap();
        }
        renderer.metered_exposure().unwrap()[0]
    };
    let at_once = live(&blinding, 0..5);
    let closed = used_to - at_once;
    assert!(
        closed > pupil - 0.1 && closed < pupil + 0.25 * (used_to - asked - pupil),
        "after five seconds the pupil has closed and little else: {closed} of {} stops",
        used_to - asked
    );
    let later = live(&blinding, 5..15);
    assert!(
        later < at_once && later > asked,
        "it goes on closing, slowly: {at_once} then {later}, toward {asked}"
    );
    let reopened = live(&dark, 15..25);
    assert!(
        (reopened - used_to).abs() < 0.1,
        "the dark opens it all the way: {reopened}, as before the light {used_to}"
    );
}

/// Open ground under a low sun, looking `away_deg` to the side of it, level.
fn sun_frame(away_deg: f32, look: Look) -> Frame {
    let mut render = RenderSettings::room(
        VenueEnvironment::outdoor(4.0),
        VenueHaze::default(),
        45.0,
        1.0,
    );
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.look = look;
    let scene = Scene {
        id: "post-look-sun".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, 1.6, 0.0],
            target: [0.0, 1.6, -10.0],
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
    let meshes = luma_scene::meshes_root();
    let mut frame = build_frame_with(
        &scene,
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut Library::new(meshes),
    )
    .unwrap();
    let sun = frame
        .sky
        .as_ref()
        .expect("an outdoor frame has a sky")
        .sun_direction;
    let level = Vec3::new(sun.x, sun.y, 0.0).normalize();
    let turn = glam::Quat::from_rotation_z(away_deg.to_radians());
    frame.camera.target = frame.camera.eye + turn * level * 10.0;
    frame
}

/// Mean luma of one image column.
fn column(pixels: &[u8], x: u32) -> f32 {
    (0..HEIGHT).map(|y| luma(pixels, x, y)).sum::<f32>() / HEIGHT as f32
}

/// The sun's glare does not change as the sun crosses the frame's edge: a
/// lens just turned off it still shows its rays.
#[test]
fn the_suns_glare_is_the_same_on_both_sides_of_the_frames_edge() {
    // Large enough that the disc is several pixels across in the frame.
    const W: u32 = 768;
    const H: u32 = 512;
    let mut renderer = Renderer::new().unwrap();
    let star = Glare {
        style: GlareStyle::Star,
        ..Glare::STAGE
    };
    let level = sun_frame(0.0, manual(star));
    let fov = level.camera.fov_y_deg;
    let elevation = level.sky.as_ref().unwrap().sun_direction.z.asin();
    let focal = H as f32 * 0.5 / (fov.to_radians() * 0.5).tan();
    let edge = (W as f32 * 0.5 / focal).atan().to_degrees();
    let luma = |pixels: &[u8], x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        0.2126 * f32::from(pixels[i])
            + 0.7152 * f32::from(pixels[i + 1])
            + 0.0722 * f32::from(pixels[i + 2])
    };
    // How far a ray stands out of the glare around it, along the column a
    // twelfth of the frame's width in from the sun, with the sun `away`
    // degrees to the side. The veil is smooth in the sky above the sun's
    // own row; a ray is a ridge.
    let mut ray = |away: f32| {
        let glaring = renderer
            .render(&sun_frame(away, manual(star)), W, H, 1)
            .unwrap();
        let plain = renderer
            .render(&sun_frame(away, manual(Glare::OFF)), W, H, 1)
            .unwrap();
        crate::common::capture(&format!("edge-{away:.0}"), &glaring, W, H);
        let inward = focal * away.to_radians().tan() - W as f32 / 12.0;
        let added = |x: f32, y: u32| {
            let x = (W as f32 * 0.5 + x).round() as u32;
            luma(&glaring, x, y) - luma(&plain, x, y)
        };
        const SPAN: u32 = 12;
        let sun_row = H as f32 * 0.5 - focal * elevation.tan() / away.to_radians().cos();
        // The sun is on one side or the other; the glare is on its side.
        [-inward, inward]
            .into_iter()
            .flat_map(|x| (SPAN..H - SPAN).map(move |y| (x, y)))
            .filter(|(_, y)| (*y as f32) < sun_row - 2.0 * SPAN as f32)
            .map(|(x, y)| added(x, y) - 0.5 * (added(x, y - SPAN) + added(x, y + SPAN)))
            .fold(0.0, f32::max)
    };
    let inside = ray(edge - 2.0);
    let outside = ray(edge + 2.0);
    assert!(inside > 2.0, "the sun in the frame throws rays: {inside}");
    assert!(
        (outside / inside - 1.0).abs() < 0.3,
        "its rays off the frame ({outside}) are its rays in it ({inside})"
    );
}

/// A sun just past the frame's edge still veils it, as it veils a real
/// lens; the convolution alone sees only the light inside the frame.
#[test]
fn a_sun_off_the_frame_still_glares() {
    let mut renderer = Renderer::new().unwrap();
    let eye = Glare {
        style: GlareStyle::Eye,
        ..Glare::STAGE
    };
    let look = |glare| Look {
        footage: luma_render::scene_desc::Footage::OFF,
        tone: ToneCurve::Agx,
        exposure: Exposure::STAGE,
        glare,
    };
    // The frame spans about ±32° across; the sun is 40° to one side.
    let glaring = render(&mut renderer, &sun_frame(40.0, look(eye)));
    let plain = render(&mut renderer, &sun_frame(40.0, look(Glare::OFF)));
    let near = |p: &[u8]| column(p, 0).max(column(p, WIDTH - 1));
    assert!(
        near(&glaring) > near(&plain) + 3.0,
        "the edge nearer the sun is veiled: {} against {} without glare",
        near(&glaring),
        near(&plain)
    );
}
