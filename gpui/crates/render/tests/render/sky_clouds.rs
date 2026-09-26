//! Sky clouds: five presets over an open-air venue, and the light they make.
//!
//! A cloud layer is only right if the sky, the sun and the ambient agree on
//! it. These tests look at all of them: how much of the sky is cloud, whether
//! the sun still casts a hard shadow, whether a cloud's shadow falls on the
//! stage, whether the clouds move with the camera, and whether a clear sky
//! is exactly the sky the renderer drew before clouds existed.
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

use glam::{Mat4, Vec3};
use luma_render::{
    assets::{Library, Material},
    build_frame_with,
    frame::{Draw, FixtureCone, MaterialTextures},
    luminaire::Lens,
    scene_desc::{
        CameraPose, CloudCover, Glare, Look, Quality, RenderSettings, Scene, VenueEnvironment,
        VenueHaze,
    },
    Frame, Renderer,
};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

/// A frame of `environment` from `eye` toward `target` (world metres, Z up),
/// with a grey cube of side `cube` at the origin when it is non-zero.
fn frame(
    environment: VenueEnvironment,
    eye: Vec3,
    target: Vec3,
    cube_m: f32,
    quality: Quality,
) -> Frame {
    let mut render = RenderSettings::room(environment, VenueHaze::default(), 55.0, 1.0);
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.quality = quality;
    // Camera poses are three-space: (x, up, -y).
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "sky-clouds".into(),
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
    if cube_m > 0.0 {
        let mesh = frame.meshes.len();
        frame.meshes.push(crate::common::cube("::sky-clouds-cube"));
        let at = frame.draws.len() - frame.transparent.len();
        frame.draws.insert(
            at,
            Draw {
                mesh,
                model: Mat4::from_translation(Vec3::new(0.0, 0.0, cube_m * 0.5))
                    * Mat4::from_scale(Vec3::splat(cube_m)),
                material: Material {
                    base_color: Vec3::splat(0.5),
                    metallic: 0.0,
                    roughness: 0.8,
                    ..Default::default()
                },
                textures: MaterialTextures::default(),
                editor_object: None,
            },
        );
    }
    frame
}

fn outdoor(elevation: f32, clouds: CloudCover) -> VenueEnvironment {
    VenueEnvironment::outdoor(elevation).with_clouds(clouds)
}

/// Looking up at the sky, half-way to the zenith, away from the sun.
fn sky_view(environment: VenueEnvironment) -> Frame {
    sky_view_from(environment, Vec3::new(0.0, 0.0, 2.0))
}

fn sky_view_from(environment: VenueEnvironment, eye: Vec3) -> Frame {
    frame(
        environment,
        eye,
        eye + Vec3::new(10.0, 4.0, 5.0),
        0.0,
        Quality::High,
    )
}

/// Across a floor toward the horizon, with the sun in front: the view the
/// stage is seen in.
fn horizon_view(environment: VenueEnvironment) -> Frame {
    frame(
        environment,
        Vec3::new(0.0, 30.0, 3.0),
        Vec3::new(-2.0, 0.0, 6.0),
        0.0,
        Quality::High,
    )
}

/// The cube on the floor, from a few metres off.
fn stage_view(environment: VenueEnvironment) -> Frame {
    frame(
        environment,
        Vec3::new(3.0, 14.0, 2.5),
        Vec3::new(0.0, 0.0, 1.5),
        2.0,
        Quality::High,
    )
}

/// Down over kilometres of ground from a few hundred metres up, where a
/// cloud's shadow is a patch among others.
fn survey_view(environment: VenueEnvironment) -> Frame {
    frame(
        environment,
        Vec3::new(0.0, -1500.0, 500.0),
        Vec3::new(0.0, 1500.0, 0.0),
        0.0,
        Quality::High,
    )
}

fn capture(name: &str, pixels: &[u8], width: u32, height: u32) {
    if let Some(dir) = std::env::var_os("LUMA_SKY_CLOUDS_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, width, height)
            .unwrap();
    }
}

fn rgb(pixels: &[u8], x: u32, y: u32) -> [f32; 3] {
    let offset = ((y * WIDTH + x) * 4) as usize;
    [0, 1, 2].map(|c| f32::from(pixels[offset + c]) / 255.0)
}

fn luma([r, g, b]: [f32; 3]) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn pixels_in(rows: std::ops::Range<u32>) -> impl Iterator<Item = (u32, u32)> {
    rows.flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
}

/// Fraction of the upper part of the frame that reads as cloud rather than
/// blue sky: a pixel whose blue is not well clear of its red. The pale band
/// along the horizon is left out; a clear sky is pale there too.
fn cloud_fraction(pixels: &[u8]) -> f32 {
    let rows = 0..HEIGHT * 2 / 3;
    let count = rows.len() as u32 * WIDTH;
    let cloud = pixels_in(rows)
        .filter(|&(x, y)| {
            let [r, _, b] = rgb(pixels, x, y);
            b - r < 0.25 * b.max(1e-3)
        })
        .count();
    cloud as f32 / count as f32
}

fn mean_luma(pixels: &[u8], rows: std::ops::Range<u32>) -> f32 {
    let count = rows.len() as u32 * WIDTH;
    pixels_in(rows)
        .map(|(x, y)| luma(rgb(pixels, x, y)))
        .sum::<f32>()
        / count as f32
}

/// Standard deviation of luma over some rows: how much structure there is.
fn luma_spread(pixels: &[u8], rows: std::ops::Range<u32>) -> f32 {
    let mean = mean_luma(pixels, rows.clone());
    let count = rows.len() as u32 * WIDTH;
    let sum: f32 = pixels_in(rows)
        .map(|(x, y)| (luma(rgb(pixels, x, y)) - mean).powi(2))
        .sum();
    (sum / count as f32).sqrt()
}

/// Mean absolute luma difference of two images over some rows.
fn difference(a: &[u8], b: &[u8], rows: std::ops::Range<u32>) -> f32 {
    let count = rows.len() as u32 * WIDTH;
    pixels_in(rows)
        .map(|(x, y)| (luma(rgb(a, x, y)) - luma(rgb(b, x, y))).abs())
        .sum::<f32>()
        / count as f32
}

/// Mean luma over a 9x9 patch centred on the pixel `p` projects to.
fn patch(frame: &Frame, pixels: &[u8], p: Vec3) -> f32 {
    let projection = Mat4::perspective_rh(
        frame.camera.fov_y_deg.to_radians(),
        WIDTH as f32 / HEIGHT as f32,
        5000.0,
        0.1,
    );
    let view = Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z);
    let clip = projection * view * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    let x = ((ndc.x * 0.5 + 0.5) * WIDTH as f32) as u32;
    let y = ((0.5 - ndc.y * 0.5) * HEIGHT as f32) as u32;
    let mut sum = 0.0;
    for j in y - 4..=y + 4 {
        for i in x - 4..=x + 4 {
            sum += luma(rgb(pixels, i, j));
        }
    }
    sum / 81.0
}

/// Open floor over the floor in the cube's shadow.
fn shadow_contrast(frame: &Frame, pixels: &[u8]) -> f32 {
    let direction = frame.sky.expect("an outdoor room has a sky").sun_direction;
    let away = -Vec3::new(direction.x, direction.y, 0.0).normalize();
    let shadowed = patch(frame, pixels, away * 2.2);
    let open = patch(frame, pixels, away * 2.2 + away.cross(Vec3::Z) * 4.0);
    open / shadowed.max(1e-3)
}

#[test]
fn each_preset_covers_as_much_of_the_sky_as_it_names() {
    let mut renderer = Renderer::new().unwrap();
    let mut coverage = Vec::new();
    let mut brightness = Vec::new();
    let mut structure = Vec::new();
    for clouds in CloudCover::ALL {
        for (name, elevation) in [("noon", 50.0), ("dusk", 6.0)] {
            for (view, f) in [
                ("sky", sky_view(outdoor(elevation, clouds))),
                ("horizon", horizon_view(outdoor(elevation, clouds))),
            ] {
                let pixels = renderer.render(&f, WIDTH, HEIGHT, 1).unwrap();
                capture(&format!("{view}-{name}-{clouds:?}"), &pixels, WIDTH, HEIGHT);
                if name == "noon" && view == "sky" {
                    coverage.push(cloud_fraction(&pixels));
                    brightness.push(mean_luma(&pixels, 0..HEIGHT));
                    structure.push(luma_spread(&pixels, 0..HEIGHT));
                }
            }
        }
    }
    // A wide look along the horizon, for the far field: it should thin out
    // and merge, not repeat the same cloud in rows.
    for clouds in CloudCover::ALL {
        let mut wide = horizon_view(outdoor(35.0, clouds));
        wide.camera.target = wide.camera.eye + Vec3::new(1.0, 0.0, 0.08);
        wide.camera.fov_y_deg = 45.0;
        let pixels = renderer.render(&wide, 1800, 450, 1).unwrap();
        capture(&format!("wide-{clouds:?}"), &pixels, 1800, 450);
    }
    eprintln!("cloud fraction by preset: {coverage:?}");
    eprintln!("mean luma by preset: {brightness:?}");
    eprintln!("luma spread by preset: {structure:?}");
    let [clear, fair, wispy, scattered, overcast, storm] = coverage[..] else {
        unreachable!()
    };
    assert!(clear < 0.02, "a clear sky has cloud in it: {clear}");
    assert!(
        fair > 0.1 && fair < 0.6,
        "fair weather should be separate clouds with sky between: {fair}"
    );
    assert!(
        scattered > fair && scattered < 0.95,
        "scattered should cover more than fair weather and leave sky: {scattered}"
    );
    // Cirrus is thin: the sky shows through it, and much of the sky has
    // none.
    assert!(
        wispy < fair && brightness[2] > brightness[0] * 1.03,
        "wispy cirrus should brighten the sky and hide little of it: {wispy}, {brightness:?}"
    );
    assert!(overcast > 0.95, "overcast has gaps: {overcast}");
    assert!(storm > 0.95, "a storm has gaps: {storm}");
    assert!(
        brightness[5] < brightness[4] * 0.8,
        "a storm should be darker than overcast: {brightness:?}"
    );
    // A deck is not a flat fill: its base has light and dark patches.
    assert!(
        structure[4] > 0.01 && structure[5] > 0.01,
        "a deck is a flat plane: {structure:?}"
    );
}

#[test]
fn a_deck_takes_the_hard_shadow_away_and_a_clear_sky_keeps_it() {
    let mut renderer = Renderer::new().unwrap();
    let mut contrast = Vec::new();
    for clouds in [CloudCover::Clear, CloudCover::Overcast, CloudCover::Storm] {
        let frame = stage_view(outdoor(40.0, clouds));
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("stage-{clouds:?}"), &pixels, WIDTH, HEIGHT);
        contrast.push(shadow_contrast(&frame, &pixels));
    }
    eprintln!("open floor over shadowed floor, clear, overcast, storm: {contrast:?}");
    assert!(
        contrast[0] > 1.5,
        "a clear sky casts a hard shadow: {contrast:?}"
    );
    assert!(
        contrast[1] < 1.2 && contrast[2] < 1.2,
        "a deck still casts a hard shadow: {contrast:?}"
    );
}

#[test]
fn cloud_shadows_dapple_the_ground_and_drift_with_the_wind() {
    let mut renderer = Renderer::new().unwrap();
    let ground = HEIGHT / 2..HEIGHT;
    let mut spread = Vec::new();
    let mut drift = Vec::new();
    for clouds in [CloudCover::Clear, CloudCover::Scattered] {
        let mut frame = survey_view(outdoor(50.0, clouds));
        let before = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        // Five minutes of an eight-metre-a-second wind: over two kilometres.
        frame.time += 300.0;
        let after = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("survey-{clouds:?}-0"), &before, WIDTH, HEIGHT);
        capture(&format!("survey-{clouds:?}-300"), &after, WIDTH, HEIGHT);
        spread.push(luma_spread(&before, ground.clone()));
        drift.push(difference(&before, &after, ground.clone()));
    }
    eprintln!("ground luma spread, clear and scattered: {spread:?}");
    eprintln!("ground change over five minutes, clear and scattered: {drift:?}");
    assert!(
        spread[1] > spread[0] * 1.5,
        "scattered cloud should dapple the ground: {spread:?}"
    );
    assert!(drift[0] < 0.002, "a clear sky's ground changed: {drift:?}");
    assert!(drift[1] > 0.02, "the cloud shadows did not move: {drift:?}");
}

#[test]
fn clouds_move_against_the_sky_when_the_camera_moves() {
    // A layer a kilometre up shifts by degrees when the eye moves a few
    // hundred metres; a baked sky would not move at all.
    let mut renderer = Renderer::new().unwrap();
    let mut shift = Vec::new();
    for clouds in [CloudCover::Clear, CloudCover::FairWeather] {
        let mut images = Vec::new();
        for (step, x) in [0.0, 150.0, 300.0, 450.0].into_iter().enumerate() {
            let frame = sky_view_from(outdoor(50.0, clouds), Vec3::new(x, 0.0, 2.0));
            let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
            capture(
                &format!("parallax-{clouds:?}-{step}"),
                &pixels,
                WIDTH,
                HEIGHT,
            );
            images.push(pixels);
        }
        shift.push(difference(&images[0], &images[3], 0..HEIGHT * 2 / 3));
    }
    eprintln!("sky change over 450 m of camera travel, clear and fair: {shift:?}");
    assert!(
        shift[0] < 0.002,
        "a clear sky moved with the camera: {shift:?}"
    );
    assert!(
        shift[1] > 0.02,
        "the clouds stayed put as the camera moved: {shift:?}"
    );
}

#[test]
fn a_beam_stays_brighter_than_the_sky_behind_it() {
    // A white beam in light stage haze at dusk, raked up past the cube into
    // the sky upstage. Whatever the weather, the beam is the subject: it
    // must add light over the clouds, and a brighter overcast must not wash
    // it out.
    let mut renderer = Renderer::new().unwrap();
    let mut contrast = Vec::new();
    for clouds in CloudCover::ALL {
        let mut frame = stage_view(outdoor(6.0, clouds));
        frame.haze_density = 0.15;
        frame.haze_bounds =
            luma_scene::Aabb::new(Vec3::new(-15.0, -15.0, 0.0), Vec3::new(15.0, 15.0, 15.0));
        let dark = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        frame.fixture_cones.push(FixtureCone {
            position: Vec3::new(-3.0, 0.0, 0.5),
            range: 40.0,
            direction: Vec3::new(0.2, -0.6, 1.0).normalize(),
            cos_beam: 8.0_f32.to_radians().cos(),
            color: Vec3::ONE,
            intensity: 20.0,
            cos_field: 12.0_f32.to_radians().cos(),
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: Lens::POINT,
        });
        let lit = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("beam-{clouds:?}"), &lit, WIDTH, HEIGHT);
        // Over the sky only: the top third of the frame, above the horizon.
        let (mut beam, mut sky, mut pixels) = (0.0, 0.0, 0);
        for (x, y) in pixels_in(0..HEIGHT / 3) {
            let before = rgb(&dark, x, y);
            let after = rgb(&lit, x, y);
            let added: f32 = (0..3).map(|c| after[c] - before[c]).sum::<f32>() / 3.0;
            if added > 0.02 {
                beam += added;
                sky += before.iter().sum::<f32>() / 3.0;
                pixels += 1;
            }
        }
        assert!(
            pixels > 200,
            "{clouds:?}: the beam barely shows over the sky ({pixels} pixels)"
        );
        contrast.push(beam / sky.max(1e-3));
    }
    eprintln!("beam over sky, added / behind: {contrast:?}");
    for (clouds, ratio) in CloudCover::ALL.iter().zip(&contrast) {
        assert!(
            *ratio > 0.5 * contrast[0],
            "{clouds:?} washes the beam out: {ratio} against {} under a clear sky",
            contrast[0]
        );
    }
}

#[test]
fn a_clear_sky_is_left_as_it_was_after_a_storm() {
    let mut renderer = Renderer::new().unwrap();
    let clear = stage_view(outdoor(40.0, CloudCover::Clear));
    let first = renderer.render(&clear, WIDTH, HEIGHT, 1).unwrap();
    let storm = stage_view(outdoor(40.0, CloudCover::Storm));
    let stormy = renderer.render(&storm, WIDTH, HEIGHT, 1).unwrap();
    let again = renderer.render(&clear, WIDTH, HEIGHT, 1).unwrap();
    assert_ne!(first, stormy);
    assert!(
        first == again,
        "a clear sky changed after a storm was drawn"
    );
}

#[test]
fn low_quality_draws_the_same_weather_for_less() {
    // Timings come from the live path, where the trace is spread over
    // frames, with the camera turning so every frame reprojects. A report,
    // not a bound: GPU time on a shared machine is not a contract.
    let mut renderer = Renderer::new_profiled().unwrap();
    let mut coverage = Vec::new();
    for quality in [Quality::High, Quality::Low] {
        let frame = self::frame(
            outdoor(50.0, CloudCover::Scattered),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(10.0, 4.0, 7.0),
            0.0,
            quality,
        );
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(
            &format!("sky-scattered-{quality:?}"),
            &pixels,
            WIDTH,
            HEIGHT,
        );
        coverage.push(cloud_fraction(&pixels));

        let (mut view, mut shadow, mut shafts, mut total) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let started = Instant::now();
        let frames = 48;
        for step in 0..frames {
            let turn = step as f32 * 0.02;
            let eye = Vec3::new(step as f32 * 2.0, 0.0, 2.0);
            let mut moving = self::frame(
                outdoor(50.0, CloudCover::Scattered),
                eye,
                eye + Vec3::new(10.0 * turn.cos(), 10.0 * turn.sin(), 5.0),
                0.0,
                quality,
            );
            moving.time = step as f32 / 60.0;
            // Light stage haze, so the sun-shaft pass runs too.
            moving.haze_density = 0.1;
            let timings = renderer.profile_live_frame(&moving, 1920, 1080, 1).unwrap();
            let span = |name: &str| {
                timings
                    .passes
                    .iter()
                    .filter(|pass| pass.name == name)
                    .map(|pass| pass.end_ms - pass.start_ms)
                    .sum::<f64>()
            };
            if step >= 8 {
                view.push(span("atmosphere-cloud-view"));
                shadow.push(span("atmosphere-cloud-shadow"));
                shafts.push(span("sun-shafts"));
                total.push(timings.gpu_total_ms);
            }
        }
        let median = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        eprintln!(
            "{quality:?} at 1920x1080, camera moving: cloud view {:.3} ms, cloud shadow {:.3} ms, \
             sun shafts {:.3} ms, frame {:.2} ms (median of {}), wall {:?} a frame",
            median(&mut view),
            median(&mut shadow),
            median(&mut shafts),
            median(&mut total),
            total.len(),
            started.elapsed() / frames,
        );
    }
    assert!(
        (coverage[0] - coverage[1]).abs() < 0.05,
        "low quality changed the weather: {coverage:?}"
    );
}

#[test]
fn a_sunlit_box_casts_a_shadow_into_the_haze_behind_it() {
    // A six-metre box in outdoor haze with a low sun from +X. Its shadow
    // runs off toward -X through the air, below a line falling from the
    // box's top edge. From off to one side, looking partly toward the sun,
    // a ray through that shadow should come back darker with the box there
    // than without it, and a ray passing over the shadow should not.
    let mut renderer = Renderer::new().unwrap();
    let environment = VenueEnvironment::outdoor(10.0).with_sun_azimuth(0.0);
    let render = |renderer: &mut Renderer, cube_m: f32| {
        let mut frame = frame(
            environment,
            Vec3::new(-20.0, -15.0, 2.0),
            Vec3::new(-8.0, 0.0, 5.0),
            cube_m,
            Quality::High,
        );
        // Thick enough that the light scattered toward the camera comes from
        // the first few tens of metres, where the shadow is.
        frame.haze_density = 0.5;
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        (frame, pixels)
    };
    let (_, open) = render(&mut renderer, 0.0);
    let (frame, boxed) = render(&mut renderer, 6.0);
    capture("haze-shadow-open", &open, WIDTH, HEIGHT);
    capture("haze-shadow-box", &boxed, WIDTH, HEIGHT);
    let change = |p: Vec3| patch(&frame, &boxed, p) / patch(&frame, &open, p).max(1e-3);
    // Eight metres back from the box the shadow's top is 4.6 m up.
    let (through, over) = (
        change(Vec3::new(-8.0, 0.0, 3.0)),
        change(Vec3::new(-8.0, 0.0, 9.0)),
    );
    eprintln!(
        "haze with the box over haze without it: through its shadow {through}, over it {over}"
    );
    assert!(
        through < over - 0.02,
        "the box casts no shadow into the haze: {through} through it, {over} over it"
    );
    assert!((over - 1.0).abs() < 0.02, "sunlit haze changed: {over}");
}

/// Thin posts against the far ground, with a sun 1.8 degrees up ahead and
/// haze in the air: the truss the app drew speckled.
///
/// Under overcast the sun does not reach the air, so the haze has no sun in
/// it (as in the stage's shadow). The composite once took that sun off each
/// pixel at the depth of the pixel's centre. On a post's edge the
/// centre sees the far ground, while most of the pixel's colour is post,
/// whose haze is the short run of air in front of it: the far ground's sun
/// came off the post, more than the post had, red and green clipped to
/// nothing and blue was left. Now each fragment takes its own share off in
/// the scene pass. An edge pixel is part post and part ground, both warm
/// in a low sun's haze, so it is not blue.
#[test]
fn a_thin_post_against_the_far_ground_keeps_its_colour_in_sunlit_haze() {
    let mut renderer = Renderer::new().unwrap();
    let render = |renderer: &mut Renderer, posts: bool| {
        let mut frame = frame(
            outdoor(1.8, CloudCover::Overcast).with_sun_azimuth(0.0),
            Vec3::new(-20.0, 0.0, 6.0),
            Vec3::new(20.0, 0.0, 3.0),
            0.0,
            Quality::High,
        );
        frame.haze_density = 0.1;
        if posts {
            let mesh = frame.meshes.len();
            frame.meshes.push(crate::common::cube("::thin-posts"));
            let at = frame.draws.len() - frame.transparent.len();
            for i in 0..13 {
                frame.draws.insert(
                    at + i,
                    Draw {
                        mesh,
                        model: Mat4::from_translation(Vec3::new(0.0, -9.0 + 1.5 * i as f32, 4.5))
                            * Mat4::from_scale(Vec3::new(0.1, 0.1, 9.0)),
                        material: Material {
                            base_color: Vec3::splat(0.5),
                            metallic: 0.0,
                            roughness: 0.8,
                            ..Default::default()
                        },
                        textures: MaterialTextures::default(),
                        editor_object: None,
                    },
                );
            }
        }
        renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap()
    };
    let open = render(&mut renderer, false);
    let posts = render(&mut renderer, true);
    capture("thin-posts-open", &open, WIDTH, HEIGHT);
    capture("thin-posts", &posts, WIDTH, HEIGHT);
    // Below the horizon, where the ground stands behind the posts.
    let blue = |[r, _, b]: [f32; 3]| b - r;
    let (mut covered, mut speckled) = (0, 0);
    for (x, y) in pixels_in(HEIGHT / 2..HEIGHT) {
        let (ground, pixel) = (rgb(&open, x, y), rgb(&posts, x, y));
        if (luma(pixel) - luma(ground)).abs() < 2.0 / 255.0 {
            continue;
        }
        covered += 1;
        if blue(ground) < 0.0 && blue(pixel) > 10.0 / 255.0 {
            speckled += 1;
        }
    }
    eprintln!("thin posts: {covered} pixels show a post, {speckled} of them blue");
    assert!(covered > 500, "the posts do not show: {covered}");
    assert!(
        speckled == 0,
        "{speckled} of {covered} post pixels are blue on warm ground"
    );
}

/// The sun at 25 degrees, at azimuth 0 (+X), and a camera looking either
/// toward it or away from it, 22 degrees up.
fn sun_view(clouds: CloudCover, toward: bool, haze: f32) -> Frame {
    let target = if toward {
        Vec3::new(10.0, 0.0, 6.0)
    } else {
        Vec3::new(-10.0, 0.0, 6.0)
    };
    let mut frame = frame(
        outdoor(25.0, clouds).with_sun_azimuth(0.0),
        Vec3::new(0.0, 0.0, 2.0),
        target,
        0.0,
        Quality::High,
    );
    frame.haze_density = haze;
    frame
}

fn is_cloud(pixels: &[u8], x: u32, y: u32) -> bool {
    let [r, _, b] = rgb(pixels, x, y);
    b - r < 0.25 * b.max(1e-3)
}

/// For each pixel of the upper two thirds of the frame, how many pixels it
/// is from the nearest sky pixel (0 for sky), by a two-pass chamfer
/// transform, capped at 255.
fn distance_to_sky(pixels: &[u8]) -> Vec<u8> {
    let (w, h) = (WIDTH as usize, (HEIGHT * 2 / 3) as usize);
    let mut d = vec![255u8; w * h];
    for y in 0..h {
        for x in 0..w {
            if !is_cloud(pixels, x as u32, y as u32) {
                d[y * w + x] = 0;
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut v = d[y * w + x];
            if x > 0 {
                v = v.min(d[y * w + x - 1].saturating_add(1));
            }
            if y > 0 {
                v = v.min(d[(y - 1) * w + x].saturating_add(1));
            }
            d[y * w + x] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let mut v = d[y * w + x];
            if x + 1 < w {
                v = v.min(d[y * w + x + 1].saturating_add(1));
            }
            if y + 1 < h {
                v = v.min(d[(y + 1) * w + x].saturating_add(1));
            }
            d[y * w + x] = v;
        }
    }
    d
}

/// Mean luma of cloud pixels at most two pixels in from the sky (the
/// edge), and of the cloud a little further in around each of them (six to
/// ten pixels from the sky, within twelve of the edge pixel): an outline is
/// an edge brighter than the cloud just inside it.
fn edge_and_inside(pixels: &[u8]) -> (f32, f32) {
    let d = distance_to_sky(pixels);
    let (w, h) = (WIDTH as i32, (HEIGHT * 2 / 3) as i32);
    let (mut edge, mut inside, mut count) = (0.0, 0.0, 0);
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            if !(1..=2).contains(&d[(y * w + x) as usize]) {
                continue;
            }
            let (mut sum, mut n) = (0.0, 0);
            for j in (y - 12).max(0)..(y + 13).min(h) {
                for i in (x - 12).max(0)..(x + 13).min(w) {
                    if (6..=10).contains(&d[(j * w + i) as usize]) {
                        sum += luma(rgb(pixels, i as u32, j as u32));
                        n += 1;
                    }
                }
            }
            if n > 0 {
                edge += luma(rgb(pixels, x as u32, y as u32));
                inside += sum / n as f32;
                count += 1;
            }
        }
    }
    assert!(count > 200, "too little cloud edge to measure: {count}");
    (edge / count as f32, inside / count as f32)
}

#[test]
fn a_cloud_edge_glows_toward_the_sun_and_not_away_from_it() {
    // The same broken sky seen toward the sun behind it and with the sun at
    // the camera's back. Toward the sun a thin edge scatters the sun forward
    // and glows (the silver lining); away from it an edge is thin cloud with
    // little depth to gather light in, no brighter than the cloud inside.
    let mut renderer = Renderer::new().unwrap();
    for clouds in [CloudCover::FairWeather, CloudCover::Scattered] {
        let toward = renderer
            .render(&sun_view(clouds, true, 0.0), WIDTH, HEIGHT, 1)
            .unwrap();
        let away = renderer
            .render(&sun_view(clouds, false, 0.0), WIDTH, HEIGHT, 1)
            .unwrap();
        capture(&format!("edges-{clouds:?}-toward"), &toward, WIDTH, HEIGHT);
        capture(&format!("edges-{clouds:?}-away"), &away, WIDTH, HEIGHT);
        let (toward_edge, toward_inside) = edge_and_inside(&toward);
        let (away_edge, away_inside) = edge_and_inside(&away);
        eprintln!(
            "{clouds:?}: toward the sun edge {toward_edge} inside {toward_inside}, \
             away edge {away_edge} inside {away_inside}"
        );
        assert!(
            toward_edge > away_edge,
            "{clouds:?}: an edge toward the sun is no brighter than one away from it"
        );
        assert!(
            away_edge <= away_inside * 1.02,
            "{clouds:?}: edges away from the sun are outlined: {away_edge} against {away_inside}"
        );
    }
}

#[test]
fn under_overcast_the_air_has_no_sun_in_it() {
    // In stage haze, the sun's forward scattering makes the air toward it
    // far brighter than the air away from it under a clear sky. Under a deck
    // no sun reaches the air, so the two directions differ by the deck's own
    // shading only; and the lens has no sun to veil.
    let mut renderer = Renderer::new().unwrap();
    let mut ratio = Vec::new();
    for clouds in [CloudCover::Clear, CloudCover::Overcast, CloudCover::Storm] {
        let toward = renderer
            .render(&sun_view(clouds, true, 0.3), WIDTH, HEIGHT, 1)
            .unwrap();
        let away = renderer
            .render(&sun_view(clouds, false, 0.3), WIDTH, HEIGHT, 1)
            .unwrap();
        capture(&format!("air-{clouds:?}-toward"), &toward, WIDTH, HEIGHT);
        capture(&format!("air-{clouds:?}-away"), &away, WIDTH, HEIGHT);
        ratio.push(mean_luma(&toward, 0..HEIGHT) / mean_luma(&away, 0..HEIGHT));
    }
    eprintln!("haze toward the sun over haze away from it, clear, overcast, storm: {ratio:?}");
    // The lens glare, at a fixed exposure: with the sun behind a deck at
    // the camera's back there is no sun off the frame to veil it.
    let glare = |clouds: CloudCover, glare: Glare, renderer: &mut Renderer| {
        let mut frame = sun_view(clouds, false, 0.0);
        frame.look = Look {
            glare,
            ..Look::NEUTRAL
        };
        mean_luma(
            &renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap(),
            0..HEIGHT,
        )
    };
    let mut veil = |clouds| {
        glare(clouds, Glare::STAGE, &mut renderer) / glare(clouds, Glare::OFF, &mut renderer)
    };
    let (clear_veil, overcast_veil) = (veil(CloudCover::Clear), veil(CloudCover::Overcast));
    eprintln!("glare over no glare, sun at the back: clear {clear_veil}, overcast {overcast_veil}");
    assert!(
        overcast_veil < 1.01 && overcast_veil < clear_veil,
        "a sun behind the deck still veils the lens: {overcast_veil} (clear {clear_veil})"
    );
    assert!(
        ratio[0] > 1.3,
        "clear haze does not glow toward the sun: {ratio:?}"
    );
    for (deck, r) in ["overcast", "storm"].iter().zip(&ratio[1..]) {
        assert!(
            (r - 1.0).abs() < 0.05,
            "{deck} haze depends on the sun: {ratio:?}"
        );
    }
}
