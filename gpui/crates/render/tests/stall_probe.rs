//! Scratch probe, not a gate: reproduce the "GPU busy 4 ms, until_signalled
//! 42 ms" stall from a live lit stage, headless, and localize it.
//!
//! Pipelines frames the way the app does — a new submit every ~8 ms while
//! deliveries are taken as they land — and prints delivered interval,
//! draw_time (submit → observed completion) and until_signalled per light
//! count. Delete once the stall is understood.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::build_frame_with;
use luma_render::frame::{FixtureCone, Frame};
use luma_render::scene_desc::{CameraPose, DebugView, Environment, RenderSettings, Scene};
use luma_render::viewport::AsyncViewport;

const WIDTH: u32 = 2558;
const HEIGHT: u32 = 1357;

fn scene() -> Scene {
    let mut render = RenderSettings::dark_stage(48.0, 0.5);
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.haze.enabled = true;
    render.haze.steps = 8;
    render.haze.density = 0.65;
    render.debug_view = DebugView::Pbr;
    Scene {
        id: "stall-probe".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: Vec::new(),
        state: BTreeMap::new(),
    }
}

fn lit_frame(lights: usize, phase: f32, haze: bool) -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut scene = scene();
    scene.render.haze.enabled = haze;
    let mut frame = build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library)
        .expect("frame builds");
    frame.fixture_cones = (0..lights)
        .map(|i| {
            let angle = phase + i as f32 * 0.7;
            FixtureCone {
                position: Vec3::new((i % 8) as f32 - 4.0, 4.0, (i / 8) as f32 - 2.0),
                range: 8.0,
                direction: Vec3::new(angle.sin() * 0.3, -1.0, angle.cos() * 0.3).normalize(),
                cos_beam: 0.975,
                color: Vec3::new(1.0, 0.3, 0.2),
                intensity: 0.4,
                cos_field: 0.93,
                wash: 0.0,
                gobo: 0,
                gobo_rotation: 0.0,
            }
        })
        .collect();
    frame.haze_density = if haze { 0.65 } else { 0.0 };
    frame
}

#[test]
fn lone_lit_frames_report_their_latency() {
    // No pipelining: submit one frame, wait for it, measure. Separates a
    // genuine per-frame stall from queueing behind the previous frame.
    for (label, lights, haze) in [
        ("static120+haze", 120usize, true),
        ("static120-haze", 120, false),
    ] {
        let mut viewport = AsyncViewport::new();
        viewport.set_subframes(1);
        let mut draws = Vec::new();
        for serial in 0..120u64 {
            viewport.submit(lit_frame(lights, 0.0, haze), WIDTH, HEIGHT);
            let presented = loop {
                if let Some(result) = viewport.take_latest() {
                    break result.expect("frame renders");
                }
                std::thread::sleep(Duration::from_micros(200));
            };
            if serial >= 20 {
                draws.push(presented.draw_time.as_secs_f64() * 1000.0);
            }
        }
        draws.sort_by(f64::total_cmp);
        println!(
            "lone {label}: draw p50={:.2}ms p95={:.2}ms",
            draws[draws.len() / 2],
            draws[draws.len() * 95 / 100]
        );
    }
}

#[test]
fn pipelined_lit_frames_report_their_latency() {
    for (label, lights, moving, haze) in [
        ("unlit", 0usize, false, true),
        ("static+haze", 30, false, true),
        ("moving+haze", 30, true, true),
        ("moving-haze", 30, true, false),
        ("static-haze", 30, false, false),
    ] {
        let mut viewport = AsyncViewport::new();
        viewport.set_subframes(1);
        let start = Instant::now();
        let mut serial = 0u64;
        let mut delivered = Vec::new();
        let mut last_delivery: Option<Instant> = None;
        let mut intervals = Vec::new();
        let mut shadow_redraws = 0u64;
        // ~4 seconds of pipelined submission at an 8 ms cadence.
        while start.elapsed() < Duration::from_secs(4) {
            serial += 1;
            viewport.submit(
                lit_frame(
                    lights,
                    if moving { serial as f32 * 0.05 } else { 0.0 },
                    haze,
                ),
                WIDTH,
                HEIGHT,
            );
            std::thread::sleep(Duration::from_millis(8));
            while let Some(result) = viewport.take_latest() {
                let presented = result.expect("frame renders");
                let now = Instant::now();
                if let Some(previous) = last_delivery.replace(now) {
                    intervals.push(previous.elapsed().as_secs_f64() * 1000.0);
                }
                shadow_redraws += presented.shadows.redrawn_maps as u64;
                delivered.push((
                    presented.draw_time.as_secs_f64() * 1000.0,
                    presented
                        .until_signalled
                        .map(|d| d.as_secs_f64() * 1000.0)
                        .unwrap_or(-1.0),
                    presented.timings.map(|t| t.gpu_total_ms).unwrap_or(-1.0),
                ));
            }
        }
        let mut draws: Vec<f64> = delivered.iter().map(|d| d.0).collect();
        draws.sort_by(f64::total_cmp);
        let mut signalled: Vec<f64> = delivered.iter().map(|d| d.1).collect();
        signalled.sort_by(f64::total_cmp);
        intervals.sort_by(f64::total_cmp);
        let pct = |v: &[f64], q: f64| {
            v.get(((v.len() as f64 * q) as usize).min(v.len().saturating_sub(1)))
                .copied()
                .unwrap_or(-1.0)
        };
        let gpu: Vec<f64> = delivered
            .iter()
            .map(|d| d.2)
            .filter(|g| *g >= 0.0)
            .collect();
        let gpu_mean = gpu.iter().sum::<f64>() / gpu.len().max(1) as f64;
        println!(
            "{label} lights={lights}: delivered={} draw p50/p95={:.2}/{:.2}ms \
             until_signalled p50/p95={:.2}/{:.2}ms interval p50/p95={:.2}/{:.2}ms gpu_mean={gpu_mean:.2}ms shadow_redraws={shadow_redraws}",
            delivered.len(),
            pct(&draws, 0.5),
            pct(&draws, 0.95),
            pct(&signalled, 0.5),
            pct(&signalled, 0.95),
            pct(&intervals, 0.5),
            pct(&intervals, 0.95),
        );
    }
}
