//! Floor materials: each floor is its own ground, it does not repeat in a
//! grid, and a laptop draws it without parallax.
//!
//! `LUMA_FLOOR_CAPTURE_DIR` saves every frame these tests look at.
use std::{collections::BTreeMap, path::PathBuf};

use glam::{Mat4, Vec3};
use luma_render::{
    assets::Material,
    build_frame_with,
    frame::{Draw, FixtureCone, MaterialTextures},
    luminaire::Lens,
    scene_desc::{
        CameraPose, CloudCover, Floor, Geometry, Piece, Procedural, Quality, RenderSettings, Scene,
        VenueEnvironment, VenueHaze,
    },
    Frame, Renderer,
};

use crate::common::library;

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

/// A frame of an empty venue on `floor`, from `eye` toward `target` (world
/// metres, Z up).
fn frame(floor: Floor, eye: Vec3, target: Vec3, quality: Quality) -> Frame {
    let indoor = !Floor::OUTDOOR.contains(&floor);
    let environment = if indoor {
        VenueEnvironment::indoor(1.0)
    } else {
        VenueEnvironment::outdoor(35.0).with_sun_azimuth(200.0)
    };
    frame_in(environment.with_floor(floor), eye, target, quality)
}

/// [`frame`] under `environment`, which names the floor.
fn frame_in(environment: VenueEnvironment, eye: Vec3, target: Vec3, quality: Quality) -> Frame {
    frame_with(environment, eye, target, quality, Vec::new())
}

/// [`frame_in`] with `pieces` standing on the floor.
fn frame_with(
    environment: VenueEnvironment,
    eye: Vec3,
    target: Vec3,
    quality: Quality,
    pieces: Vec<Piece>,
) -> Frame {
    let mut render = RenderSettings::room(environment, VenueHaze::default(), 50.0, 1.0);
    render.haze.enabled = false;
    render.show_grid = false;
    render.show_gizmos = false;
    render.quality = quality;
    // Camera poses are three-space: (x, up, -y).
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "floor-materials".into(),
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
        pieces,
        state: BTreeMap::new(),
    };
    build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library()).unwrap()
}

/// A wash light `height` metres over `at`, pointing straight down.
fn wash(at: Vec3, height: f32) -> FixtureCone {
    FixtureCone {
        strobe: luma_render::strobe::Rows::STEADY,
        position: at + Vec3::Z * height,
        range: 40.0,
        direction: -Vec3::Z,
        cos_beam: 15.0_f32.to_radians().cos(),
        color: Vec3::ONE,
        intensity: 6.0,
        cos_field: 22.0_f32.to_radians().cos(),
        wash: 1.0,
        gobo: 0,
        gobo_rotation: 0.0,
        haze_gain: 1.0,
        lens: Lens::POINT,
    }
}

/// One piece: a mesh under `resources/meshes`, or a truss span.
fn piece(id: String, geometry: Geometry, pos: [f32; 3], rot: [f32; 3]) -> Piece {
    Piece {
        id,
        geometry,
        kind: String::new(),
        pos,
        rot,
        scale: 1.0,
    }
}

/// A stage like the one in `images/33.png`: a 12 by 6 m deck a metre high,
/// a row of subs in front, a flown truss rectangle on four towers.
fn stage() -> Vec<Piece> {
    let mut pieces = Vec::new();
    for column in 0..12 {
        for row in 0..3 {
            pieces.push(piece(
                format!("deck-{column}-{row}"),
                Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                [-6.0 + column as f32, row as f32 * 2.0, 0.0],
                [0.0; 3],
            ));
        }
    }
    for i in 0..10 {
        pieces.push(piece(
            format!("sub-{i}"),
            Geometry::mesh("stage_lab/speaker_dual18sub.glb"),
            [-5.4 + i as f32 * 1.1, -1.6, 0.0],
            [0.0; 3],
        ));
    }
    for (i, y) in [0.3, 5.7].into_iter().enumerate() {
        pieces.push(piece(
            format!("span-{i}"),
            Geometry::Procedural(Procedural::Truss { span: 12.0 }),
            [0.0, y, 7.0],
            [0.0; 3],
        ));
        for (j, x) in [-6.2, 6.2].into_iter().enumerate() {
            pieces.push(piece(
                format!("tower-{i}-{j}"),
                Geometry::Procedural(Procedural::Truss { span: 7.0 }),
                [x, y, 3.5],
                [0.0, std::f32::consts::FRAC_PI_2, 0.0],
            ));
        }
    }
    pieces
}

/// The orbit view of `images/33.png`: from 24 m up and 30 m in front of the
/// stage, under a grey sky, with a row of washes pooling on the ground in
/// front of it.
fn orbit(floor: Floor) -> Frame {
    let environment = VenueEnvironment::outdoor(35.0)
        .with_sun_azimuth(200.0)
        .with_clouds(CloudCover::Overcast)
        .with_floor(floor);
    let mut frame = frame_with(
        environment,
        Vec3::new(4.0, -32.0, 24.0),
        Vec3::new(0.0, 2.0, 0.0),
        Quality::High,
        stage(),
    );
    for x in [-8.0, -4.0, 0.0, 4.0, 8.0] {
        frame.fixture_cones.push(wash(Vec3::new(x, -4.0, 0.0), 7.0));
    }
    frame
}

/// Standing on the floor, looking across it.
fn standing(floor: Floor) -> Frame {
    frame(
        floor,
        Vec3::new(0.0, -12.0, 1.7),
        Vec3::new(0.0, 10.0, 0.0),
        Quality::High,
    )
}

fn capture(name: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_FLOOR_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
    }
}

fn rgb(pixels: &[u8], x: u32, y: u32) -> [f32; 3] {
    let o = ((y * WIDTH + x) * 4) as usize;
    [0, 1, 2].map(|c| f32::from(pixels[o + c]) / 255.0)
}

/// Mean colour of the lower half: floor only, in every view here.
fn floor_colour(pixels: &[u8]) -> [f32; 3] {
    let mut sum = [0.0; 3];
    let mut n = 0.0;
    for y in HEIGHT / 2..HEIGHT {
        for x in 0..WIDTH {
            let c = rgb(pixels, x, y);
            for i in 0..3 {
                sum[i] += c[i];
            }
            n += 1.0;
        }
    }
    sum.map(|s| s / n)
}

/// Mean luminance of each pixel's neighbourhood removed: what is left is the
/// texture, without the slow brightness change across the view.
fn detail(pixels: &[u8]) -> Vec<f32> {
    let luma: Vec<f32> = (0..WIDTH * HEIGHT)
        .map(|i| {
            let c = rgb(pixels, i % WIDTH, i / WIDTH);
            0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
        })
        .collect();
    let r = 8i32;
    let at = |x: i32, y: i32| {
        luma[(y.clamp(0, HEIGHT as i32 - 1) as u32 * WIDTH + x.clamp(0, WIDTH as i32 - 1) as u32)
            as usize]
    };
    let mut out = vec![0.0; luma.len()];
    for y in 0..HEIGHT as i32 {
        for x in 0..WIDTH as i32 {
            let mut sum = 0.0;
            for dy in -r..=r {
                for dx in -r..=r {
                    sum += at(x + dx, y + dy);
                }
            }
            out[(y as u32 * WIDTH + x as u32) as usize] =
                at(x, y) - sum / ((2 * r + 1) * (2 * r + 1)) as f32;
        }
    }
    out
}

/// Correlation of `image` with itself shifted by (`dx`, `dy`) pixels.
fn correlation(image: &[f32], dx: u32, dy: u32) -> f32 {
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for y in 0..HEIGHT - dy {
        for x in 0..WIDTH - dx {
            let a = image[(y * WIDTH + x) as usize];
            let b = image[((y + dy) * WIDTH + x + dx) as usize];
            ab += a * b;
            aa += a * a;
            bb += b * b;
        }
    }
    ab / (aa * bb).sqrt().max(1e-9)
}

/// The strongest correlation within a pixel of one tile's shift, across and
/// along: how plainly the floor repeats at its tile.
fn periodicity(pixels: &[u8], period: f32) -> f32 {
    let image = detail(pixels);
    let p = period.round() as u32;
    let mut best = f32::MIN;
    for q in p - 1..=p + 1 {
        for (dx, dy) in [(q, 0), (0, q)] {
            best = best.max(correlation(&image, dx, dy));
        }
    }
    best
}

#[test]
fn each_floor_is_its_own_ground() {
    let mut renderer = Renderer::new().unwrap();
    let mut colours = Vec::new();
    let mut images = Vec::new();
    for floor in Floor::OUTDOOR.iter().chain(&Floor::INDOOR) {
        let frame = standing(*floor);
        assert_eq!(
            frame.floor.map(|f| f.floor),
            Some(*floor),
            "{floor:?} did not load"
        );
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("floor-{}", floor.set()), &pixels);
        colours.push((*floor, floor_colour(&pixels)));
        images.push((*floor, pixels));
    }
    eprintln!("{colours:?}");
    let colour = |floor: Floor| colours.iter().find(|(f, _)| *f == floor).unwrap().1;
    let green = |c: [f32; 3]| c[1] - 0.5 * (c[0] + c[2]);
    let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    assert!(green(colour(Floor::Grass)) > green(colour(Floor::Concrete)) + 0.03);
    assert!(luma(colour(Floor::Beach)) > luma(colour(Floor::Asphalt)) + 0.1);
    assert!(luma(colour(Floor::HallFloor)) > luma(colour(Floor::BlackStage)) + 0.03);
    // Under the same light, no two floors of a kind of venue look alike:
    // pixel by pixel, the floor half of the view differs.
    let image = |floor: Floor| &images.iter().find(|(f, _)| *f == floor).unwrap().1;
    for group in [&Floor::OUTDOOR[..], &Floor::INDOOR[..]] {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                let (pa, pb) = (image(*a), image(*b));
                let start = (WIDTH * HEIGHT / 2 * 4) as usize;
                let d = pa[start..]
                    .iter()
                    .zip(&pb[start..])
                    .map(|(x, y)| f32::from(x.abs_diff(*y)) / 255.0)
                    .sum::<f32>()
                    / (pa.len() - start) as f32;
                assert!(d > 0.01, "{a:?} and {b:?} draw the same ground: {d:.4}");
            }
        }
    }
}

#[test]
fn anti_tiling_hides_the_tile() {
    let mut renderer = Renderer::new().unwrap();
    // Straight down, from where one tile is 40 pixels: gravel, whose copies
    // turn, and the beach, whose copies are only shifted so its ripples keep
    // one direction (`max_rotation_deg`). Shifts alone must break the repeat.
    let focal = (HEIGHT as f32 / 2.0) / 25.0_f32.to_radians().tan();
    // The least correlation plain tiling shows: the beach's ripples are
    // finer than a pixel at this height, so its repeat is fainter.
    for (floor, repeats) in [(Floor::Gravel, 0.6), (Floor::Beach, 0.3)] {
        let tile = luma_render::floor::Surface::of(floor, Quality::High).tile_m;
        let height = tile * focal / 40.0;
        let mut measured = Vec::new();
        for anti_tiling in [false, true] {
            let name = if anti_tiling { "anti-tiling" } else { "plain" };
            let mut above = frame(
                floor,
                Vec3::new(0.0, -0.01, height),
                Vec3::ZERO,
                Quality::High,
            );
            above.floor.as_mut().unwrap().anti_tiling = anti_tiling;
            let pixels = renderer.render(&above, WIDTH, HEIGHT, 1).unwrap();
            capture(&format!("above-{}-{name}", floor.set()), &pixels);
            measured.push(periodicity(&pixels, tile * focal / height));
            if floor == Floor::Gravel {
                // The far view: a few metres up, over 20 to 100 m of grass.
                let mut across = frame(
                    Floor::Grass,
                    Vec3::new(0.0, -20.0, 6.0),
                    Vec3::new(0.0, 40.0, 0.0),
                    Quality::High,
                );
                across.floor.as_mut().unwrap().anti_tiling = anti_tiling;
                capture(
                    &format!("across-{name}"),
                    &renderer.render(&across, WIDTH, HEIGHT, 1).unwrap(),
                );
            }
        }
        eprintln!(
            "{floor:?}: correlation at one tile: plain {:.3}, anti-tiling {:.3}",
            measured[0], measured[1]
        );
        assert!(
            measured[0] > repeats,
            "{floor:?}: plain tiling should repeat: {measured:?}"
        );
        assert!(
            measured[1] < 0.5 * measured[0],
            "{floor:?}: the tile still shows: {measured:?}"
        );
    }
}

#[test]
fn low_quality_draws_no_parallax() {
    let low = frame(
        Floor::Dirt,
        Vec3::new(0.0, -4.0, 1.7),
        Vec3::new(0.0, 2.0, 0.0),
        Quality::Low,
    );
    assert_eq!(low.floor.unwrap().parallax_m, 0.0);
    // On High the near dirt is marched as a height field, and that moves
    // the texture.
    let high = || {
        frame(
            Floor::Dirt,
            Vec3::new(0.0, -4.0, 1.7),
            Vec3::new(0.0, 2.0, 0.0),
            Quality::High,
        )
    };
    let marched = high();
    assert!(marched.floor.unwrap().parallax_m > 0.0);
    let mut flat = high();
    flat.floor.as_mut().unwrap().parallax_m = 0.0;
    let mut renderer = Renderer::new().unwrap();
    let marched = renderer.render(&marched, WIDTH, HEIGHT, 1).unwrap();
    let plain = renderer.render(&flat, WIDTH, HEIGHT, 1).unwrap();
    capture("parallax-high", &marched);
    capture("parallax-off", &plain);
    let moved = (HEIGHT / 2..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let (a, b) = (rgb(&marched, x, y), rgb(&plain, x, y));
            (0..3).any(|c| (a[c] - b[c]).abs() > 0.03)
        })
        .count();
    let share = moved as f32 / (WIDTH * HEIGHT / 2) as f32;
    eprintln!("parallax moves {:.0}% of the near floor", share * 100.0);
    assert!(
        share > 0.2,
        "parallax changed only {:.1}% of the floor",
        share * 100.0
    );
}

/// The frame as it was before the floors had a least roughness, tints or
/// softer patches: for the before-and-after pictures only.
fn as_before(frame: &mut Frame) {
    if std::env::var_os("LUMA_FLOOR_AS_BEFORE").is_some() {
        let floor = frame.floor.as_mut().unwrap();
        floor.min_roughness = 0.0;
        floor.tints = [[1.0; 3]; 2];
        floor.hex = true;
        floor.max_rotation = 1.0;
        floor.transition_share = match floor.floor {
            Floor::Grass => 0.2,
            Floor::Dirt => 0.15,
            _ => floor.transition_share,
        };
    }
}

#[test]
fn the_orbit_view_shows_the_ground() {
    let mut renderer = Renderer::new().unwrap();
    for floor in [Floor::Grass, Floor::Dirt, Floor::Concrete, Floor::Beach] {
        let mut frame = orbit(floor);
        as_before(&mut frame);
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("orbit-{}", floor.set()), &pixels);
    }
}

/// Mean linear luminance of a block of an sRGB frame.
fn luminance(pixels: &[u8], x: std::ops::Range<u32>, y: std::ops::Range<u32>) -> f32 {
    let linear = |v: u8| {
        let c = f32::from(v) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let mut sum = 0.0;
    for row in y.clone() {
        for column in x.clone() {
            let o = ((row * WIDTH + column) * 4) as usize;
            sum += 0.2126 * linear(pixels[o])
                + 0.7152 * linear(pixels[o + 1])
                + 0.0722 * linear(pixels[o + 2]);
        }
    }
    sum / (x.len() * y.len()) as f32
}

/// Every floor is drawn at least as rough as its set says, and a light
/// reflects off it as that roughness would: a wash seen in the mirror
/// direction lights a rough floor little more than one seen from the side.
/// Grass read as its scan, roughness 0.26, looked wet: its pool under a
/// mirrored wash was several times the side-lit one.
#[test]
fn every_floor_is_as_rough_as_its_set_says() {
    let mut renderer = Renderer::new().unwrap();
    let eye = Vec3::new(0.0, -10.0, 6.0);
    let mut failures = Vec::new();
    for floor in Floor::OUTDOOR.iter().chain(&Floor::INDOOR) {
        let indoor = !Floor::OUTDOOR.contains(floor);
        let dark = if indoor {
            VenueEnvironment::indoor(0.0)
        } else {
            VenueEnvironment::outdoor(-12.0)
        }
        .with_floor(*floor);
        // What the shader was given.
        let mut view = frame_in(dark, eye, Vec3::ZERO, Quality::High);
        as_before(&mut view);
        let least = luma_render::floor::Surface::of(*floor, Quality::High).min_roughness;
        view.debug_view = luma_render::scene_desc::DebugView::Roughness;
        let rough = renderer.render(&view, WIDTH, HEIGHT, 1).unwrap();
        let drawn = luminance(&rough, 380..580, 300..500);
        // The same pool lit from the mirror direction and from the side, at
        // the same distance and angle: the diffuse light is the same.
        let lit = |renderer: &mut Renderer, from: Vec3| {
            let mut frame = frame_in(dark, eye, Vec3::ZERO, Quality::High);
            as_before(&mut frame);
            let mut cone = wash(Vec3::ZERO, 0.0);
            cone.position = from;
            cone.direction = -from.normalize();
            cone.cos_beam = 30.0_f32.to_radians().cos();
            cone.cos_field = 40.0_f32.to_radians().cos();
            frame.fixture_cones.push(cone);
            renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap()
        };
        let mirror = lit(&mut renderer, Vec3::new(0.0, 10.0, 6.0));
        let side = lit(&mut renderer, Vec3::new(10.0, 0.0, 6.0));
        capture(&format!("mirror-{}", floor.set()), &mirror);
        let gloss = luminance(&mirror, 440..520, 230..310) / luminance(&side, 440..520, 230..310);
        // The mirrored light's excess, in the floor's own albedo: the
        // specular reflectance toward the camera. A dark floor shows its
        // specular more for the same lobe, so the excess alone would ask
        // asphalt to be rougher than grass.
        let albedo = luma_render::floor::mean_albedo(*floor);
        let sheen = (gloss - 1.0) * albedo;
        eprintln!(
            "{floor:?}: least roughness {least:.2}, drawn {drawn:.2}, mirror/side {gloss:.2}, sheen {sheen:.3}"
        );
        if drawn < least - 0.03 {
            failures.push(format!(
                "{floor:?} is drawn at roughness {drawn:.2} under its least {least:.2}"
            ));
        }
        // The outdoor grounds, concrete (0.7) and rougher. The stage and
        // hall floors are meant to shine.
        if least >= 0.7 && sheen > 0.1 {
            failures.push(format!(
                "{floor:?} at roughness {least:.2} has a sheen of {sheen:.3}"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The strongest repeat of `pixels` across, between `from` and `to` pixels
/// of shift, and its shift.
fn repeat_across(pixels: &[u8], from: u32, to: u32) -> (u32, f32) {
    let image = detail(pixels);
    (from..=to)
        .map(|shift| (shift, correlation(&image, shift, 0)))
        .fold((0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a })
}

/// Every floor's maps repeat every `tile_m` metres of ground, as its source
/// states it, on High and on Low: seen straight down with plain tiling, the
/// picture repeats at the tile's width in pixels. And from one camera, with
/// a metre-square deck on each, the floors are to scale with each other
/// (the `metre-*` captures).
#[test]
fn every_floor_repeats_at_its_real_size() {
    let mut renderer = Renderer::new().unwrap();
    let focal = (HEIGHT as f32 / 2.0) / 25.0_f32.to_radians().tan();
    let mut failures = Vec::new();
    for floor in Floor::OUTDOOR.iter().chain(&Floor::INDOOR[1..]) {
        let tile = luma_render::floor::Surface::of(*floor, Quality::High).tile_m;
        // High enough that a tile is 96 pixels across.
        let height = tile * focal / 96.0;
        for quality in [Quality::High, Quality::Low] {
            let mut above = frame(*floor, Vec3::new(0.0, -0.01, height), Vec3::ZERO, quality);
            above.floor.as_mut().unwrap().anti_tiling = false;
            let pixels = renderer.render(&above, WIDTH, HEIGHT, 1).unwrap();
            let (shift, _) = repeat_across(&pixels, 60, 150);
            let metres = shift as f32 * height / focal;
            eprintln!(
                "{floor:?} {quality:?}: repeats every {metres:.2} m, the set says {tile:.2} m"
            );
            if (metres - tile).abs() > 0.03 * tile {
                failures.push(format!(
                    "{floor:?} {quality:?} repeats every {metres:.2} m, not {tile:.2} m"
                ));
            }
        }
        let deck = vec![piece(
            "metre".into(),
            Geometry::mesh("stage_lab/stage_praticavel_1x1.glb"),
            [-0.5, -0.5, 0.0],
            [0.0; 3],
        )];
        let indoor = !Floor::OUTDOOR.contains(floor);
        let environment = if indoor {
            VenueEnvironment::indoor(1.0)
        } else {
            VenueEnvironment::outdoor(35.0).with_sun_azimuth(200.0)
        };
        let view = frame_with(
            environment.with_floor(*floor),
            Vec3::new(0.0, -5.0, 4.0),
            Vec3::new(0.0, 1.0, 0.0),
            Quality::High,
            deck,
        );
        capture(
            &format!("metre-{}", floor.set()),
            &renderer.render(&view, WIDTH, HEIGHT, 1).unwrap(),
        );
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A neutral grey box takes the floor's colour from below: the sky's ground
/// and the ambient probe's lower half are the floor's mean colour
/// (`floor::mean_color`), not one grey number. The box floats a metre up
/// and the camera looks at its underside, which only the ground lights.
/// Crowd barriers on warm dirt were a cool grey pasted on the ground
/// (`images/34.png`).
#[test]
fn a_grey_box_takes_the_floors_colour_from_below() {
    let mut renderer = Renderer::new().unwrap();
    let eye = Vec3::new(0.0, -3.0, 0.4);
    let target = Vec3::new(0.0, 0.0, 1.2);
    let centre = Vec3::new(0.0, 0.0, 1.5);
    let chroma = |floor: Floor, renderer: &mut Renderer| {
        let environment = VenueEnvironment::outdoor(30.0)
            .with_sun_azimuth(200.0)
            .with_floor(floor);
        let mut frame = frame_in(environment, eye, target, Quality::High);
        let mesh = frame.meshes.len();
        frame
            .meshes
            .push(crate::common::cube("::floor-bounce-cube"));
        let at = frame.draws.len() - frame.transparent.len();
        frame.draws.insert(
            at,
            Draw {
                strobe: luma_render::strobe::Rows::STEADY,
                mesh,
                model: Mat4::from_translation(centre),
                material: Material {
                    base_color: Vec3::splat(0.5),
                    roughness: 0.9,
                    ..Default::default()
                },
                textures: MaterialTextures::default(),
                editor_object: None,
            },
        );
        let pixels = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
        capture(&format!("underside-{}", floor.set()), &pixels);
        // The underside's middle, where the camera sees it.
        let view_proj = Mat4::perspective_rh(
            frame.camera.fov_y_deg.to_radians(),
            WIDTH as f32 / HEIGHT as f32,
            0.1,
            1000.0,
        ) * Mat4::look_at_rh(eye, target, Vec3::Z);
        let clip = view_proj * (centre - Vec3::Z * 0.5).extend(1.0);
        let x = ((clip.x / clip.w * 0.5 + 0.5) * WIDTH as f32) as u32;
        let y = ((0.5 - clip.y / clip.w * 0.5) * HEIGHT as f32) as u32;
        let mut sum = [0.0f32; 3];
        for row in y - 8..y + 8 {
            for column in x - 8..x + 8 {
                let o = ((row * WIDTH + column) * 4) as usize;
                for c in 0..3 {
                    sum[c] += f32::from(pixels[o + c]);
                }
            }
        }
        // Green over the mean of red and blue.
        sum[1] / (0.5 * (sum[0] + sum[2]))
    };
    let grass = chroma(Floor::Grass, &mut renderer);
    let concrete = chroma(Floor::Concrete, &mut renderer);
    eprintln!("underside green over red and blue: grass {grass:.3}, concrete {concrete:.3}");

    // The user's view: crowd barriers and subs on the ground, a few metres
    // up and back. For the pictures only.
    let pieces = || {
        let mut pieces = Vec::new();
        for i in 0..6 {
            pieces.push(piece(
                format!("barrier-{i}"),
                Geometry::mesh("stage_lab/guardrail.glb"),
                [-7.5 + 2.5 * i as f32, -2.0, 0.0],
                [0.0; 3],
            ));
        }
        for i in 0..4 {
            pieces.push(piece(
                format!("sub-{i}"),
                Geometry::mesh("stage_lab/speaker_dual18sub.glb"),
                [-3.0 + 1.5 * i as f32, 0.0, 0.0],
                [0.0; 3],
            ));
        }
        pieces
    };
    for floor in [Floor::Dirt, Floor::Grass, Floor::Concrete] {
        let environment = VenueEnvironment::outdoor(35.0)
            .with_sun_azimuth(200.0)
            .with_clouds(CloudCover::Overcast)
            .with_floor(floor);
        let frame = frame_with(
            environment,
            Vec3::new(3.0, -14.0, 4.0),
            Vec3::new(0.0, 0.0, 0.5),
            Quality::High,
            pieces(),
        );
        capture(
            &format!("barriers-{}", floor.set()),
            &renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap(),
        );
    }
    assert!(
        grass > concrete + 0.05,
        "the box is no greener over grass: {grass:.3} against {concrete:.3}"
    );
}
