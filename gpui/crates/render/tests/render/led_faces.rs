//! A library LED panel's face, seen from close up to far away: sharp LEDs
//! with dark gaps when they are big on screen, their average colour, evenly,
//! once they are smaller than a pixel, and every cell its own colour at any
//! distance.

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::{Quat, Vec3};
use luma_render::scene_desc::{CameraPose, Fixture, PrimitiveState, RenderSettings, Scene};
use luma_render::{build_frame, Frame, Renderer};
use luma_scene::definition::{Definition, Dimensions, Lens, Mode, Physical};

use crate::common::{capture, library};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 400;
/// Narrow, so a far face still covers enough pixels to measure.
const FOV: f32 = 20.0;

const HYT: &str = "oppsk/hyt-006-384sl";
const STROBE_960: &str = "oppsk/960led-stage-strobe-light";

fn spec(id: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../../harness/fixture-specs/{id}.luma.json")),
    )
    .unwrap()
}

fn build(text: &str) -> Arc<luma_fixture::Built> {
    Arc::new(luma_fixture::build(&luma_fixture::Fixture::from_json(text).unwrap()).unwrap())
}

/// The HYT with its LEDs grown to fill their pitch: one solid lit face, the
/// look its real face should average to.
fn solid_hyt() -> Arc<luma_fixture::Built> {
    let text = spec(HYT);
    assert!(text.contains("\"fill\": 0.6"));
    build(&text.replace("\"fill\": 0.6", "\"fill\": 1.0"))
}

/// The share of the HYT's face its LEDs light: 0.6 of the pitch each way.
const HYT_LIT_SHARE: f32 = 0.6 * 0.6;

fn definition(built: Arc<luma_fixture::Built>) -> Definition {
    Definition {
        built: Some(built),
        kind: "Strobe".into(),
        modes: vec![Mode {
            name: "Standard".into(),
            heads: Vec::new(),
        }],
        physical: Some(Physical {
            dimensions: Some(Dimensions {
                width: 325.0,
                height: 185.0,
                depth: 76.0,
            }),
            layout: None,
            lens: Some(Lens {
                degrees_min: 40.0,
                degrees_max: 40.0,
                radius_m: None,
                offset_m: None,
            }),
        }),
    }
}

/// The fixture with cell `k` lit `colour(k)` at `dimmer`, seen face on from
/// `distance` metres, `turn` degrees round to its side. `path` keys its meshes: one renderer keeps a key's
/// mesh for its lifetime, so each build needs its own.
fn frame(
    path: &str,
    built: &Arc<luma_fixture::Built>,
    dimmer: f32,
    colour: &dyn Fn(usize) -> [f32; 3],
    distance: f32,
    turn: f32,
) -> Frame {
    let cells = built.cells.len();
    let mut state = BTreeMap::new();
    for k in 0..cells {
        state.insert(
            format!("panel:{k}"),
            PrimitiveState {
                dimmer,
                color: colour(k),
                strobe: 0.0,
                position: [0.0, 0.0],
                gobo: 0,
                gobo_rotation: 0.0,
            },
        );
    }
    let mut render = RenderSettings::dark_stage(FOV, 0.5);
    render.haze.enabled = false;
    render.show_floor = false;
    let mut scene = Scene {
        id: "led-faces".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [0.0, -distance, 3.0],
            target: [0.0, 0.0, 3.0],
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: vec![Fixture {
            id: "panel".into(),
            fixture_path: path.into(),
            mode_name: "Standard".into(),
            pos: [0.0, 0.0, 3.0],
            rot: [std::f32::consts::FRAC_PI_2, 0.0, 0.0],
            aim: [0.0; 2],
        }],
        pieces: Vec::new(),
        state,
    };
    let mut definitions = BTreeMap::new();
    definitions.insert(path.to_string(), definition(built.clone()));
    // Face on: the camera sits on the mean beam, out from the mean emitter.
    let probe = build_frame(&scene, &definitions, 0.0, &mut library()).unwrap();
    let cones = &probe.fixture_cones;
    assert!(!cones.is_empty(), "the panel is lit");
    let n = cones.len() as f32;
    let centre = cones.iter().map(|c| c.position).sum::<Vec3>() / n;
    let beam = (cones.iter().map(|c| c.direction).sum::<Vec3>() / n).normalize();
    // The camera is posed in three (Y-up) space; cones are in world space.
    let three = luma_scene::coords::three_from_world;
    scene.camera = CameraPose {
        position: three(centre + Quat::from_rotation_z(turn.to_radians()) * beam * distance)
            .to_array(),
        target: three(centre).to_array(),
    };
    build_frame(&scene, &definitions, 0.0, &mut library()).unwrap()
}

fn linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear RGB per pixel.
fn pixels(rgba: &[u8]) -> Vec<Vec3> {
    rgba.chunks_exact(4)
        .map(|p| Vec3::new(linear(p[0]), linear(p[1]), linear(p[2])))
        .collect()
}

fn luminance(c: Vec3) -> f32 {
    c.dot(Vec3::new(0.2126, 0.7152, 0.0722))
}

/// The face's inside: pixels of a solid lit face, less a pixel all round so
/// no pixel is part face and part background.
fn interior(solid: &[Vec3]) -> Vec<bool> {
    let peak = solid.iter().copied().map(luminance).fold(0.0, f32::max);
    let lit: Vec<bool> = solid.iter().map(|&c| luminance(c) > 0.25 * peak).collect();
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            x > 0
                && y > 0
                && x + 1 < w
                && y + 1 < h
                && [i, i - 1, i + 1, i - w, i + w].iter().all(|&j| lit[j])
        })
        .collect()
}

fn mean(of: impl Iterator<Item = Vec3>) -> Vec3 {
    let (sum, n) = of.fold((Vec3::ZERO, 0usize), |(s, n), c| (s + c, n + 1));
    sum / n.max(1) as f32
}

fn render(renderer: &mut Renderer, frame: &Frame, name: &str) -> Vec<Vec3> {
    let rgba = renderer.render(frame, WIDTH, HEIGHT, 1).unwrap();
    capture(name, &rgba, WIDTH, HEIGHT);
    pixels(&rgba)
}

const WHITE: [f32; 3] = [1.0, 0.85, 0.6];

/// Past a pixel per LED the face is its LEDs' average, everywhere on it:
/// as bright as a solid face lit at the LEDs' share of it, and even, with
/// no bands where the LEDs and their gaps beat against the pixels.
#[test]
fn a_far_led_face_is_its_average_colour_evenly() {
    let real = build(&spec(HYT));
    let solid = solid_hyt();
    let mut renderer = Renderer::new().unwrap();
    for distance in [7.0f32, 8.5, 10.0, 12.0] {
        let name = format!("hyt-far-{distance}");
        let shown = render(
            &mut renderer,
            &frame("lib:real", &real, 1.0, &|_| WHITE, distance, 0.0),
            &name,
        );
        let flat = render(
            &mut renderer,
            &frame(
                "lib:solid",
                &solid,
                HYT_LIT_SHARE,
                &|_| WHITE,
                distance,
                0.0,
            ),
            &format!("{name}-solid"),
        );
        let inside = interior(&flat);
        let count = inside.iter().filter(|&&b| b).count();
        assert!(count > 150, "{distance} m: {count} pixels of face");
        let pick = |img: &[Vec3]| -> Vec<f32> {
            img.iter()
                .zip(&inside)
                .filter(|(_, &b)| b)
                .map(|(&c, _)| luminance(c))
                .collect()
        };
        let (lum, reference) = (pick(&shown), pick(&flat));
        let avg = lum.iter().sum::<f32>() / lum.len() as f32;
        let want = reference.iter().sum::<f32>() / reference.len() as f32;
        assert!(
            (avg / want - 1.0).abs() < 0.12,
            "{distance} m: the face averages {avg:.4}, a solid face at its lit share {want:.4}"
        );
        let spread =
            (lum.iter().map(|l| (l - avg).powi(2)).sum::<f32>() / lum.len() as f32).sqrt() / avg;
        eprintln!(
            "{distance} m: mean {avg:.4} against {want:.4}, spread {:.1}%",
            spread * 100.0
        );
        assert!(
            spread < 0.12,
            "{distance} m: the face varies {:.0}% across itself",
            spread * 100.0
        );
    }
}

/// Up close each LED is its own spot, with dark gaps between: a good share
/// of the face is far darker than its LEDs.
#[test]
fn a_near_led_face_shows_each_led() {
    let real = build(&spec(HYT));
    let solid = solid_hyt();
    let mut renderer = Renderer::new().unwrap();
    let distance = 0.9;
    let shown = render(
        &mut renderer,
        &frame("lib:real", &real, 1.0, &|_| WHITE, distance, 0.0),
        "hyt-near",
    );
    let flat = render(
        &mut renderer,
        &frame("lib:solid", &solid, 1.0, &|_| WHITE, distance, 0.0),
        "hyt-near-solid",
    );
    let inside = interior(&flat);
    let mut lum: Vec<f32> = shown
        .iter()
        .zip(&inside)
        .filter(|(_, &b)| b)
        .map(|(&c, _)| luminance(c))
        .collect();
    lum.sort_by(f32::total_cmp);
    let bright = lum[lum.len() * 95 / 100];
    let dark = lum.iter().filter(|&&l| l < 0.1 * bright).count() as f32 / lum.len() as f32;
    // The gaps are 1 - 0.36 of the face; most of them must read as gaps.
    assert!(
        dark > 0.4,
        "only {:.0}% of the face is gap-dark",
        dark * 100.0
    );
}

/// Cells lit different colours stay those colours, near and far: the
/// HYT's left half red, its right half blue (cells run left to right, three
/// to a column of 16).
#[test]
fn each_cell_keeps_its_own_colour_near_and_far() {
    let real = build(&spec(HYT));
    let solid = solid_hyt();
    let colour = |k: usize| {
        if k / 3 < 8 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 0.0, 1.0]
        }
    };
    let mut renderer = Renderer::new().unwrap();
    for distance in [1.5f32, 11.0] {
        let shown = render(
            &mut renderer,
            &frame("lib:real", &real, 1.0, &colour, distance, 0.0),
            &format!("hyt-halves-{distance}"),
        );
        let flat = render(
            &mut renderer,
            &frame("lib:solid", &solid, 1.0, &|_| WHITE, distance, 0.0),
            &format!("hyt-halves-{distance}-solid"),
        );
        let inside = interior(&flat);
        let columns: Vec<usize> = (0..inside.len())
            .filter(|&i| inside[i])
            .map(|i| i % WIDTH as usize)
            .collect();
        let (lo, hi) = (
            *columns.iter().min().unwrap(),
            *columns.iter().max().unwrap(),
        );
        let third = (hi - lo) / 3;
        let side = |range: std::ops::RangeInclusive<usize>| {
            mean(
                (0..inside.len())
                    .filter(|&i| inside[i] && range.contains(&(i % WIDTH as usize)))
                    .map(|i| shown[i]),
            )
        };
        let (left, right) = (side(lo..=lo + third), side(hi - third..=hi));
        assert!(
            left.x > 4.0 * left.z && right.z > 4.0 * right.x,
            "{distance} m: left {left}, right {right}"
        );
    }
}

/// Pictures only: both dense panels close, mid and far, and from the side, lit cell by cell
/// in a rainbow, for `LUMA_RENDER_CAPTURE_DIR`.
#[test]
fn dense_panels_close_mid_and_far() {
    let mut renderer = Renderer::new().unwrap();
    for id in [HYT, STROBE_960] {
        let built = build(&spec(id));
        let cells = built.cells.len() as f32;
        let rainbow = |k: usize| {
            let h = k as f32 / cells * 6.0;
            let c = |o: f32| (1.0 - ((h + o) % 6.0 - 3.0).abs().min(3.0) + 1.0).clamp(0.0, 1.0);
            [c(0.0), c(4.0), c(2.0)]
        };
        let short = id.rsplit('/').next().unwrap();
        for (label, distance) in [
            ("close", 0.9f32),
            ("mid", 4.0),
            ("far", 8.0),
            ("farther", 13.0),
            ("mid-turned", 4.0),
        ] {
            let turn = if label.ends_with("turned") { 60.0 } else { 0.0 };
            let shown = render(
                &mut renderer,
                &frame(&format!("lib:{id}"), &built, 0.3, &rainbow, distance, turn),
                &format!("{short}-{label}"),
            );
            assert!(
                shown.iter().any(|&c| luminance(c) > 0.05),
                "{id} {label} is lit"
            );
        }
    }
}
