//! Where the ground meets the sky or the background there is no line.
//!
//! The horizon is a step: sky above, ground below, and haze or aerial
//! perspective can soften it to nothing. What it must never have is a row
//! of its own, lighter or darker than the rows on both sides of it: a line
//! drawn along the horizon. That has come from the cloud layer fading its
//! distant cloud toward the wrong colour, and from the far ground and the
//! sky at the horizon each being lit by its own estimate of the same air.
use std::{collections::BTreeMap, path::PathBuf};

use luma_render::{
    assets::Library,
    build_frame_with,
    scene_desc::{CameraPose, CloudCover, RenderSettings, Scene, VenueEnvironment, VenueHaze},
    Frame, Renderer,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

/// A camera `eye_y` metres up, 20 m from the origin: level at head height,
/// and looking a little down from higher up, so the horizon crosses the
/// frame at a different row.
fn frame(environment: VenueEnvironment, haze: bool, eye_y: f32) -> Frame {
    frame_in(environment, haze.then(VenueHaze::default), eye_y)
}

/// [`frame`] through `haze`, or through clear air when `None`. The floor is
/// the venue's own material (`floor.rs`), as the app draws it.
fn frame_in(environment: VenueEnvironment, haze: Option<VenueHaze>, eye_y: f32) -> Frame {
    let mut render = RenderSettings::room(environment, haze.unwrap_or_default(), 45.0, 0.5);
    render.haze.enabled = haze.is_some();
    render.show_grid = false;
    render.show_gizmos = false;
    let scene = Scene {
        id: "horizon-seam".into(),
        times: vec![0.0],
        // Three-space: (x, up, -y).
        camera: CameraPose {
            position: [0.0, eye_y, 20.0],
            target: [0.0, eye_y - 0.4 * (eye_y - 1.7), 0.0],
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
    build_frame_with(
        &scene,
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut Library::new(crate::common::meshes()),
    )
    .unwrap()
}

/// The row the horizon crosses: level at head height, and about eleven
/// and a half degrees down from twelve metres at 45 degrees of view.
fn horizon_row(eye_y: f32) -> u32 {
    if eye_y > 2.0 {
        89
    } else {
        180
    }
}

fn row_luma(pixels: &[u8], y: u32) -> f32 {
    let mut sum = 0.0;
    for x in 0..WIDTH {
        let o = ((y * WIDTH + x) * 4) as usize;
        sum += 0.2126 * f32::from(pixels[o])
            + 0.7152 * f32::from(pixels[o + 1])
            + 0.0722 * f32::from(pixels[o + 2]);
    }
    sum / WIDTH as f32
}

fn capture(name: &str, pixels: &[u8]) {
    if let Some(dir) = std::env::var_os("LUMA_HORIZON_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join(format!("{name}.png")), pixels, WIDTH, HEIGHT)
            .unwrap();
    }
}

/// The strongest line within 25 rows of the horizon, in 8-bit levels: how
/// far one row's mean luma lies outside the range of the rows either side
/// of it. A step between two flat regions scores zero, however sharp.
fn worst_line(pixels: &[u8], horizon: u32) -> (f32, u32) {
    (horizon - 25..horizon + 25)
        .map(|y| {
            let (above, row, below) = (
                row_luma(pixels, y - 1),
                row_luma(pixels, y),
                row_luma(pixels, y + 1),
            );
            let outside = (row - above.max(below)).max(0.0) + (above.min(below) - row).max(0.0);
            (outside, y)
        })
        .fold((0.0, horizon), |a, b| if b.0 > a.0 { b } else { a })
}

#[test]
fn the_ground_meets_the_sky_without_a_line() {
    let mut renderer = Renderer::new().unwrap();
    // Evenly lit ground: indoors, a clear sky, and a deck, which shades all
    // the ground alike.
    let mut even = vec![
        ("indoor", VenueEnvironment::default()),
        ("indoor-dark", VenueEnvironment::indoor(0.0)),
        (
            "dusk-toward-sun",
            VenueEnvironment::outdoor(4.0).with_sun_azimuth(90.0),
        ),
        (
            "dusk-away",
            VenueEnvironment::outdoor(4.0).with_sun_azimuth(270.0),
        ),
    ];
    let mut broken = Vec::new();
    for clouds in CloudCover::ALL {
        let environment = VenueEnvironment::outdoor(40.0)
            .with_sun_azimuth(90.0)
            .with_clouds(clouds);
        if matches!(
            clouds,
            CloudCover::FairWeather | CloudCover::Wispy | CloudCover::Scattered
        ) {
            broken.push((clouds.label(), environment));
        } else {
            even.push((clouds.label(), environment));
        }
    }
    let mut worst_even = (0.0, String::new());
    let mut worst_broken = (0.0, String::new());
    for (set, environments) in [(&mut worst_even, even), (&mut worst_broken, broken)] {
        for (name, environment) in environments {
            for haze in [false, true] {
                for eye_y in [1.7, 12.0] {
                    let frame = frame(environment, haze, eye_y);
                    // A still, and the live path once its history has settled.
                    let still = renderer.render(&frame, WIDTH, HEIGHT, 1).unwrap();
                    let mut live = Vec::new();
                    for _ in 0..6 {
                        live = renderer.render_next(&frame, WIDTH, HEIGHT, 1).unwrap();
                    }
                    for (path, pixels) in [("still", still), ("live", live)] {
                        let label = format!("{name} haze={haze} eye={eye_y} {path}");
                        capture(&label.replace([' ', '='], "-"), &pixels);
                        let (line, row) = worst_line(&pixels, horizon_row(eye_y));
                        if line > set.0 {
                            *set = (line, format!("{label}, row {row}"));
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "strongest line by the horizon: {:.2} levels under even light ({}), {:.2} under broken cloud ({})",
        worst_even.0, worst_even.1, worst_broken.0, worst_broken.1
    );
    // The floor's far field and the ground a few hundred metres out differ
    // by a level or two under a sun ahead; the long lens sees the same
    // (`the_last_row_is_what_a_long_lens_sees_there`).
    assert!(
        worst_even.0 < 3.0,
        "a line runs along the horizon: {:.2} levels in {}",
        worst_even.0,
        worst_even.1
    );
    // Under broken cloud the far ground is in sun and shade by turns, and a
    // pixel there spans kilometres of it: it shows the average, darker than
    // sunlit ground near a camera that stands in the sun. From head height
    // that far ground is one row. Sampling the map there instead drew
    // near-black lines and stair-steps along the horizon, fifty levels and
    // more. The ground now reads the map over the pixel's whole span
    // (`ground_cloud_shadow` in `scene.wgsl`).
    assert!(
        worst_broken.0 < 10.0,
        "cloud shadows draw a line along the horizon: {:.2} levels in {}",
        worst_broken.0,
        worst_broken.1
    );
}

/// A frame from head height, level, toward a sun `elevation` degrees up
/// ahead, through `fov` degrees of view.
fn low_sun(clouds: CloudCover, haze: Option<f32>, fov: f32) -> Frame {
    let environment = VenueEnvironment::outdoor(6.0)
        .with_sun_azimuth(90.0)
        .with_clouds(clouds);
    let mut render = RenderSettings::room(
        environment,
        VenueHaze {
            density: haze.unwrap_or(0.0),
            ..VenueHaze::default()
        },
        fov,
        0.5,
    );
    render.haze.enabled = haze.is_some();
    render.show_grid = false;
    render.show_gizmos = false;
    let scene = Scene {
        id: "horizon-seam".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, 1.7, 20.0],
            target: [0.0, 1.7, 0.0],
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
    build_frame_with(
        &scene,
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut Library::new(crate::common::meshes()),
    )
    .unwrap()
}

/// Mean sRGB colour of a block, averaged as light.
fn block(pixels: &[u8], x: std::ops::Range<u32>, y: std::ops::Range<u32>) -> [f32; 3] {
    let linear = |v: u8| {
        let c = f32::from(v) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let mut sum = [0.0; 3];
    for row in y.clone() {
        for column in x.clone() {
            let o = ((row * WIDTH + column) * 4) as usize;
            for c in 0..3 {
                sum[c] += linear(pixels[o + c]);
            }
        }
    }
    let n = (x.len() * y.len()) as f32;
    sum.map(|v| {
        let v = v / n;
        255.0
            * if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            }
    })
}

/// A low sun ahead, from head height: the view of the bright line in the
/// app (`images/31.png`), and its dark twin without haze.
///
/// From 1.7 m the ground from a kilometre out to the end of it is one row.
/// What that row shows is the test: through a lens sixteen times longer the
/// same row is sixteen rows, and their mean is what the one row should be.
/// A row that differs from it is drawn by the renderer, not by what is
/// there: a line. Rows that match it may still stand out — at a low sun
/// under broken cloud the far country really is in shadow while the ground
/// at one's feet is not — and the lens is what tells the two apart.
///
/// The lines were three. The sky took the cloud-shaded air off its clear
/// table for 20 km and the ground's air for 100; past its map the cloud
/// shadow took the layer's share of clear sky, which a low sun's slant ray
/// does not see; and the haze's shafts were matched to the last row by its
/// centre's depth, which neither the sky nor the ground below it has.
#[test]
fn the_last_row_is_what_a_long_lens_sees_there() {
    let mut renderer = Renderer::new().unwrap();
    let mut worst = (0.0f32, String::new());
    for clouds in [
        CloudCover::Clear,
        CloudCover::FairWeather,
        CloudCover::Scattered,
        CloudCover::Overcast,
    ] {
        for haze in [None, Some(0.012), Some(VenueHaze::default().density)] {
            let wide = renderer
                .render(&low_sun(clouds, haze, 45.0), WIDTH, HEIGHT, 1)
                .unwrap();
            let long = renderer
                .render(&low_sun(clouds, haze, 45.0 / 16.0), WIDTH, HEIGHT, 1)
                .unwrap();
            let label = format!("{} haze={haze:?}", clouds.label());
            capture(&format!("{label} wide").replace([' ', '='], "-"), &wide);
            capture(&format!("{label} long").replace([' ', '='], "-"), &long);
            // The wide view's middle sixteenth of columns is the long view's
            // width; its row `y` is the long view's rows 16 (y - 180) on.
            let horizon = horizon_row(1.7);
            for y in horizon - 1..=horizon + 1 {
                let seen = block(&wide, WIDTH / 2 - 20..WIDTH / 2 + 20, y..y + 1);
                let top = horizon + 16 * (y + 1 - horizon) - 16;
                let there = block(&long, 0..WIDTH, top..top + 16);
                let off = (0..3)
                    .map(|c| (seen[c] - there[c]).abs())
                    .fold(0.0, f32::max);
                if off > worst.0 {
                    worst = (off, format!("{label}, row {y}: {seen:.0?} for {there:.0?}"));
                }
            }
        }
    }
    eprintln!(
        "the last rows against a long lens: {:.1} levels at worst ({})",
        worst.0, worst.1
    );
    // The two are compared after the display transform, which is not
    // linear: sixteen rows that run from ground to air do not tone-map to
    // their mean. That alone is some ten levels in the worst channel. The
    // lines were thirty to sixty.
    assert!(
        worst.0 < 20.0,
        "the horizon row is not what is there: {:.1} levels in {}",
        worst.0,
        worst.1
    );
}

/// The view the app drew the line in (`fixtures/gasworks-horizon-line.json`,
/// exported from Gasworks Park): the sun 2 degrees up behind a camera 5 m
/// up, 50 degrees of view at 2039 by 1155, under overcast and haze 0.035.
/// The export holds the camera, sun, sky and haze; the venue's clouds and
/// floor are its own settings, set here as the venue had them.
fn gasworks() -> (Frame, u32, u32) {
    use luma_render::camera_export::CameraExport;
    use luma_render::scene_desc::Floor;
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gasworks-horizon-line.json");
    let export = CameraExport::read(&path).unwrap();
    let sun = export.sun.unwrap();
    let environment = VenueEnvironment::outdoor(sun.elevation_deg)
        .with_sun_azimuth(sun.azimuth_deg)
        .with_clouds(CloudCover::Overcast)
        .with_floor(Floor::GravellySand);
    let appearance = export.haze.appearance;
    let haze = VenueHaze {
        enabled: true,
        density: export.haze.density,
        appearance,
    };
    let mut render = RenderSettings::room(
        environment,
        haze,
        export.camera.fov_y_deg,
        export.haze.resolution,
    );
    render.show_grid = false;
    render.show_gizmos = false;
    let scene = Scene {
        id: "gasworks-horizon".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, 5.0, 20.0],
            target: [0.0, 5.0, 0.0],
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
        &mut Library::new(crate::common::meshes()),
    )
    .unwrap();
    export.apply(&mut frame);
    frame.haze_bounds = luma_scene::Aabb {
        min: glam::Vec3::from(export.haze.bounds.min),
        max: glam::Vec3::from(export.haze.bounds.max),
    };
    let sky = frame.sky.as_ref().unwrap();
    eprintln!(
        "exposure {} (app {}), ground albedo {:?} (app {:?}), sun radiance {:?} (app {:?})",
        sky.exposure,
        export.sky.unwrap().exposure,
        sky.ground_albedo,
        export.sky.unwrap().ground_albedo,
        frame.directional.map(|d| d.radiance),
        sun.radiance,
    );
    let (width, height) = export.render_size();
    (frame, width, height)
}

/// The line the app drew at Gasworks Park, one pixel high across the whole
/// width. Along the horizon the ground covers part of a pixel and the sky
/// the rest, and with MSAA the ground's shader ran at the pixel's centre,
/// above the horizon, where the plane's interpolated position lies behind
/// the camera. That pixel was lit and hazed as if it looked back toward the
/// sun, which here stands 2 degrees up behind the camera: 100 levels over
/// the rows either side. The ground now takes the farthest ground in the
/// pixel's heading instead (`ground_fragment` in `scene.wgsl`).
#[test]
fn the_exported_gasworks_view_has_no_line_at_the_horizon() {
    use luma_render::camera_export::CameraExport;
    let (frame, width, height) = gasworks();
    let pixels = Renderer::new()
        .unwrap()
        .render(&frame, width, height, 1)
        .unwrap();
    if let Some(dir) = std::env::var_os("LUMA_HORIZON_CAPTURE_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        luma_render::image_out::write(&dir.join("gasworks.png"), &pixels, width, height).unwrap();
    }
    // The horizon's row, from the exported matrices: a level ray far out.
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gasworks-horizon-line.json");
    let export = CameraExport::read(&path).unwrap();
    let eye = glam::Vec3::from(export.camera.eye);
    let target = glam::Vec3::from(export.camera.target);
    let level = (target - eye).with_z(0.0).normalize();
    let clip = export.projection() * export.view() * (eye + level * 1e4).extend(1.0);
    let horizon = ((0.5 - 0.5 * clip.y / clip.w) * height as f32) as u32;
    let row = |y: u32| {
        (0..width)
            .map(|x| {
                let o = ((y * width + x) * 4) as usize;
                0.2126 * f32::from(pixels[o])
                    + 0.7152 * f32::from(pixels[o + 1])
                    + 0.0722 * f32::from(pixels[o + 2])
            })
            .sum::<f32>()
            / width as f32
    };
    let (line, at) = (horizon - 10..horizon + 10)
        .map(|y| {
            let (above, here, below) = (row(y - 1), row(y), row(y + 1));
            (
                (here - above.max(below)).max(0.0) + (above.min(below) - here).max(0.0),
                y,
            )
        })
        .fold((0.0, horizon), |a, b| if b.0 > a.0 { b } else { a });
    eprintln!("Gasworks: horizon at row {horizon}, strongest line {line:.2} levels at row {at}");
    assert!(
        line < 3.0,
        "a line runs along the horizon: {line:.2} levels at row {at}"
    );
}
