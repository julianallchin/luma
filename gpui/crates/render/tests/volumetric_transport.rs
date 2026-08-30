use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::frame::FixtureCone;
use luma_render::scene_desc::{
    CameraPose, DebugView, Environment, Geometry, Piece, RenderSettings, Scene,
};
use luma_render::{
    build_frame_with, AsyncPresentation, AsyncViewport, Frame, Renderer, SubmitOutcome, Viewport,
};
use serde::Deserialize;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;

fn scene(pieces: Vec<Piece>) -> Scene {
    let mut render = RenderSettings::dark_stage(48.0, 0.5);
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.haze.enabled = true;
    render.haze.steps = 8;
    render.haze.density = 0.65;
    render.debug_view = DebugView::VolumetricAccumulation;
    Scene {
        id: "volumetric-transport-proof".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces,
        state: BTreeMap::new(),
    }
}

fn light(gobo: u32) -> FixtureCone {
    FixtureCone {
        position: Vec3::new(0.0, 0.0, 0.15),
        range: 8.0,
        direction: Vec3::Z,
        cos_beam: 0.975,
        color: Vec3::new(0.2, 0.55, 1.0),
        intensity: 0.08,
        cos_field: 0.93,
        wash: 0.0,
        gobo,
        gobo_rotation: 0.31,
    }
}

fn frame(pieces: Vec<Piece>, lights: Vec<FixtureCone>) -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut frame = build_frame_with(
        &scene(pieces),
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut library,
    )
    .unwrap();
    frame.fixture_cones = lights;
    frame.haze_density = 0.65;
    frame
}

fn blocker() -> Piece {
    Piece {
        id: "occluder".into(),
        geometry: Geometry::mesh("stage_lab/speaker_dbr15.glb"),
        kind: "speaker".into(),
        pos: [0.0, 0.0, 0.8],
        rot: [0.0; 3],
        scale: 4.0,
    }
}

fn mean_rgb(pixels: &[u8]) -> f64 {
    pixels
        .chunks_exact(4)
        .map(|pixel| f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2]))
        .sum::<f64>()
        / (pixels.len() / 4 * 3) as f64
}

fn hash(pixels: &[u8]) -> u64 {
    pixels.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[derive(Deserialize)]
struct StressDescriptor {
    width: u32,
    height: u32,
    subframes: u32,
    camera: StressCamera,
    render: StressRender,
    light: StressLight,
    lattice: StressLattice,
    cases: Vec<StressCase>,
}

#[derive(Deserialize)]
struct StressCamera {
    position: [f32; 3],
    target: [f32; 3],
}

#[derive(Deserialize)]
struct StressRender {
    stage_size: f32,
    haze_density: f32,
    haze_resolution: f32,
    haze_steps: u32,
    debug_view: String,
}

#[derive(Deserialize)]
struct StressLight {
    origin: [f32; 3],
    direction: [f32; 3],
    range: f32,
    cos_beam: f32,
    cos_field: f32,
    color: [f32; 3],
    intensity: f32,
    gobo_modulus: u32,
    gobo_rotation: f32,
}

#[derive(Deserialize)]
struct StressLattice {
    columns: usize,
    center_column: f32,
    center_row: f32,
    spacing: [f32; 2],
}

#[derive(Deserialize)]
struct StressCase {
    cones: usize,
    image: String,
    expected_fnv64: String,
}

fn stress_frame(descriptor: &StressDescriptor, count: usize) -> Frame {
    let debug_view = match descriptor.render.debug_view.as_str() {
        "volumetric_accumulation" => DebugView::VolumetricAccumulation,
        other => panic!("unknown stress debug view {other}"),
    };
    let mut render = RenderSettings::dark_stage(
        descriptor.render.stage_size,
        descriptor.render.haze_resolution,
    );
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.haze.enabled = true;
    render.haze.steps = descriptor.render.haze_steps;
    render.haze.density = descriptor.render.haze_density;
    render.debug_view = debug_view;
    let scene = Scene {
        id: "volumetric-stress-golden".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: descriptor.camera.position,
            target: descriptor.camera.target,
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
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut frame =
        build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap();
    frame.fixture_cones.reserve(count);
    for index in 0..count {
        let column = (index % descriptor.lattice.columns) as f32;
        let row = (index / descriptor.lattice.columns) as f32;
        frame.fixture_cones.push(FixtureCone {
            position: Vec3::from_array(descriptor.light.origin)
                + Vec3::new(
                    (column - descriptor.lattice.center_column) * descriptor.lattice.spacing[0],
                    (row - descriptor.lattice.center_row) * descriptor.lattice.spacing[1],
                    0.0,
                ),
            range: descriptor.light.range,
            direction: Vec3::from_array(descriptor.light.direction),
            cos_beam: descriptor.light.cos_beam,
            color: Vec3::from_array(descriptor.light.color),
            intensity: descriptor.light.intensity,
            cos_field: descriptor.light.cos_field,
            wash: 0.0,
            gobo: (index as u32) % descriptor.light.gobo_modulus,
            gobo_rotation: descriptor.light.gobo_rotation,
        });
    }
    frame
}

fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(File::open(path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    assert_eq!(reader.info().color_type, png::ColorType::Rgba);
    assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut bytes).unwrap();
    bytes.truncate(info.buffer_size());
    (info.width, info.height, bytes)
}

fn write_png(path: &Path, width: u32, height: u32, pixels: &[u8]) {
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path).unwrap()), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(pixels)
        .unwrap();
}

/// How long [`wait_for_serial`] waits before calling the renderer worker stuck.
///
/// **A liveness guard, not a frame budget.** A viewport's *first* frame pays
/// for device creation and shader compilation — measured at ~24.5s on an idle
/// M3 Max — while every frame after it lands in 3-13ms. Any bound tight enough
/// to say something interesting about the second frame therefore fails the
/// first one the moment the machine is busy, which is exactly how this flaked
/// at 30s: a correct result went red because the box was loaded, and the
/// margin was 0-14% even when it was not.
///
/// So this is deliberately several times the measured warmup. It exists only
/// so a genuinely wedged worker fails the run instead of hanging it. What a
/// frame *costs* is asserted where it can be measured meaningfully — the
/// budget tests and `profile-volumetrics` — and deliberately not here, where
/// the number would be dominated by shader compilation.
const WORKER_LIVENESS_TIMEOUT: Duration = Duration::from_secs(180);

fn wait_for_serial(viewport: &AsyncViewport, serial: u64) -> AsyncPresentation {
    let started = Instant::now();
    let deadline = started + WORKER_LIVENESS_TIMEOUT;
    loop {
        if let Some(result) = viewport.take_latest() {
            let presentation = result.unwrap();
            if presentation.serial >= serial {
                assert_eq!(
                    presentation.serial, serial,
                    "wait skipped the target serial"
                );
                return presentation;
            }
        }
        assert!(
            Instant::now() < deadline,
            "renderer worker did not complete serial {serial} within {:?} \
             (waited {:?}) — the worker is wedged, not merely slow: this bound \
             is several times the first-frame shader-compilation cost",
            WORKER_LIVENESS_TIMEOUT,
            started.elapsed()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn draw_async(
    viewport: &mut AsyncViewport,
    frame: Frame,
    width: u32,
    height: u32,
    serial: u64,
) -> AsyncPresentation {
    viewport.submit(frame, width, height);
    wait_for_serial(viewport, serial)
}

#[test]
fn one_overlap_and_gobo_transport_are_deterministic_and_energy_monotonic() {
    let mut renderer = Renderer::new().unwrap();
    let one = renderer
        .render(&frame(Vec::new(), vec![light(0)]), WIDTH, HEIGHT, 4)
        .unwrap();
    let overlap = renderer
        .render(
            &frame(Vec::new(), vec![light(0), light(0)]),
            WIDTH,
            HEIGHT,
            4,
        )
        .unwrap();
    let gobo = renderer
        .render(&frame(Vec::new(), vec![light(1)]), WIDTH, HEIGHT, 4)
        .unwrap();
    // Provisional 2026-08-25: re-baselined for the baked haze density field
    // (`haze_field.rs`), which is a different realization of the same noise
    // process, not a different picture. Accepted from stills; the temporal
    // delta distribution was measured to match the previous field to within
    // 8% and stay flat across texel boundaries. If the look is later rejected,
    // these three and the two below are the revert candidates — the invariants
    // asserted after this point never moved.
    assert_eq!(
        (hash(&one), hash(&overlap), hash(&gobo)),
        (
            0x2f6f_fe9f_972f_c988,
            0x6f91_c14f_777d_8cca,
            0xb064_ab17_5c43_29ac,
        ),
        "one/overlap/gobo transport golden drifted"
    );

    assert!(mean_rgb(&overlap) > mean_rgb(&one));
    assert!(mean_rgb(&gobo) < mean_rgb(&one));
    assert_ne!(hash(&gobo), hash(&one));
    assert_eq!(
        one,
        renderer
            .render(&frame(Vec::new(), vec![light(0)]), WIDTH, HEIGHT, 4)
            .unwrap(),
        "fixed capture seeds must be byte deterministic"
    );
}

#[test]
fn scene_depth_occludes_beams_and_invalid_inputs_stay_bounded() {
    let mut renderer = Renderer::new().unwrap();
    let open = renderer
        .render(&frame(Vec::new(), vec![light(0)]), WIDTH, HEIGHT, 2)
        .unwrap();
    let blocked = renderer
        .render(&frame(vec![blocker()], vec![light(0)]), WIDTH, HEIGHT, 2)
        .unwrap();
    // Provisional 2026-08-25, same re-baseline as above.
    assert_eq!(
        (hash(&open), hash(&blocked)),
        (0xec44_6591_4338_0f1e, 0xbb65_661d_35cd_8d5e),
        "depth-occlusion transport golden drifted"
    );
    assert!(mean_rgb(&blocked) < mean_rgb(&open));

    let mut invalid = light(7);
    invalid.position.x = f32::NAN;
    invalid.direction = Vec3::splat(f32::NAN);
    invalid.range = f32::INFINITY;
    invalid.intensity = f32::NAN;
    invalid.cos_field = f32::NAN;
    let mut invalid_frame = frame(Vec::new(), vec![invalid]);
    invalid_frame.haze_density = f32::NAN;
    invalid_frame.time = f32::NAN;
    let safe = renderer.render(&invalid_frame, WIDTH, HEIGHT, 1).unwrap();
    assert_eq!(safe.len(), (WIDTH * HEIGHT * 4) as usize);
    assert!(safe.chunks_exact(4).all(|pixel| pixel[3] == 255));
}

#[test]
fn tiled_transport_accepts_32_128_and_512_cones() {
    let golden_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens");
    let descriptor: StressDescriptor = serde_json::from_reader(
        File::open(golden_dir.join("volumetric-stress-scenes.json")).unwrap(),
    )
    .unwrap();
    let update = std::env::var_os("LUMA_UPDATE_GOLDENS").is_some();
    let mut renderer = Renderer::new().unwrap();
    assert_eq!(
        descriptor
            .cases
            .iter()
            .map(|case| case.cones)
            .collect::<Vec<_>>(),
        [32, 128, 512],
        "the checked-in stress contract must retain all acceptance tiers"
    );
    for case in &descriptor.cases {
        let pixels = renderer
            .render(
                &stress_frame(&descriptor, case.cones),
                descriptor.width,
                descriptor.height,
                descriptor.subframes,
            )
            .unwrap();
        let actual_hash = hash(&pixels);
        let image_path = golden_dir.join(&case.image);
        if update {
            write_png(&image_path, descriptor.width, descriptor.height, &pixels);
            eprintln!("{} cones: 0x{actual_hash:016x}", case.cones);
            continue;
        }
        let expected_hash = u64::from_str_radix(
            case.expected_fnv64
                .strip_prefix("0x")
                .unwrap_or(&case.expected_fnv64),
            16,
        )
        .unwrap();
        assert_eq!(
            actual_hash, expected_hash,
            "{}-cone hash drifted",
            case.cones
        );
        let (width, height, expected_pixels) = read_png(&image_path);
        assert_eq!((width, height), (descriptor.width, descriptor.height));
        assert_eq!(pixels, expected_pixels, "{}-cone image drifted", case.cones);
    }
}

#[test]
fn live_history_resets_on_camera_and_time_discontinuity() {
    let mut live = Viewport::new().unwrap();
    live.set_subframes(1);
    let mut input = frame(Vec::new(), vec![light(0)]);
    let first = live.draw(&input, WIDTH, HEIGHT).unwrap().pixels.to_vec();
    let stabilized = live.draw(&input, WIDTH, HEIGHT).unwrap().pixels.to_vec();
    assert_ne!(
        first, stabilized,
        "stable frames must advance blue-noise history"
    );

    input.camera.eye.x += 0.25;
    let camera_reset = live.draw(&input, WIDTH, HEIGHT).unwrap().pixels.to_vec();
    let mut fresh = Viewport::new().unwrap();
    fresh.set_subframes(1);
    assert_eq!(
        camera_reset,
        fresh.draw(&input, WIDTH, HEIGHT).unwrap().pixels,
        "camera motion must reject all prior volumetric history"
    );

    live.draw(&input, WIDTH, HEIGHT).unwrap();
    input.time += 1.0;
    let time_reset = live.draw(&input, WIDTH, HEIGHT).unwrap().pixels.to_vec();
    let mut fresh = Viewport::new().unwrap();
    fresh.set_subframes(1);
    assert_eq!(
        time_reset,
        fresh.draw(&input, WIDTH, HEIGHT).unwrap().pixels,
        "track-time discontinuity must reject all prior volumetric history"
    );
}

#[test]
fn live_history_rejects_a_moving_occluder_with_a_fixed_camera() {
    let mut live = Viewport::new().unwrap();
    live.set_subframes(1);
    let open_input = frame(Vec::new(), vec![light(0)]);
    live.draw(&open_input, WIDTH, HEIGHT).unwrap();
    let open = live
        .draw(&open_input, WIDTH, HEIGHT)
        .unwrap()
        .pixels
        .to_vec();

    let blocked_input = frame(vec![blocker()], vec![light(0)]);
    let blocked_after_history = live
        .draw(&blocked_input, WIDTH, HEIGHT)
        .unwrap()
        .pixels
        .to_vec();
    let mut fresh = Viewport::new().unwrap();
    fresh.set_subframes(1);
    let blocked_fresh = fresh
        .draw(&blocked_input, WIDTH, HEIGHT)
        .unwrap()
        .pixels
        .to_vec();

    let mut changed_surface_pixels = 0;
    let mut unrejected_error = 0_u64;
    let mut rejected_error = 0_u64;
    for ((before, after), fresh) in open
        .chunks_exact(4)
        .zip(blocked_after_history.chunks_exact(4))
        .zip(blocked_fresh.chunks_exact(4))
    {
        let surface_changed = before[..3]
            .iter()
            .zip(&fresh[..3])
            .map(|(a, b)| a.abs_diff(*b) as u32)
            .sum::<u32>()
            > 30;
        if surface_changed {
            changed_surface_pixels += 1;
            unrejected_error += before[..3]
                .iter()
                .zip(&fresh[..3])
                .map(|(a, b)| u64::from(a.abs_diff(*b)))
                .sum::<u64>();
            rejected_error += after[..3]
                .iter()
                .zip(&fresh[..3])
                .map(|(a, b)| u64::from(a.abs_diff(*b)))
                .sum::<u64>();
        }
    }
    assert!(
        changed_surface_pixels > 50,
        "fixture must cover enough pixels for a meaningful depth-rejection probe"
    );
    assert!(
        rejected_error * 4 < unrejected_error,
        "depth rejection must remove at least 75% of stale-surface RGB error: \
         rejected={rejected_error}, stale={unrejected_error}"
    );
}

#[test]
fn async_viewport_advances_and_resets_production_temporal_history() {
    let mut live = AsyncViewport::new();
    live.set_subframes(1);
    let first = draw_async(
        &mut live,
        frame(Vec::new(), vec![light(0)]),
        WIDTH,
        HEIGHT,
        1,
    );
    if let Some(timings) = first.timings {
        assert!(timings.cpu_encode_submit_ms >= 0.0);
        assert!(timings.gpu_total_ms >= timings.gpu_volumetric_ms);
    }
    // The submit-span phases are only worth having if a long total is
    // *attributable*, and that requires them to account for the whole span
    // rather than sample points inside it. Pinned here because the failure is
    // silent: a phase added to the encode path and not to `CpuSpans` would
    // still produce plausible-looking numbers that quietly lose time.
    let cpu = first.cpu;
    let phases = cpu.prepare + cpu.clusters + cpu.upload + cpu.targets + cpu.encode;
    let drift = phases.abs_diff(cpu.total);
    assert!(
        drift < std::time::Duration::from_micros(50),
        "submit phases must exhaust the submit span: \
         phases={phases:?} total={:?} drift={drift:?}",
        cpu.total
    );
    assert!(
        cpu.total > std::time::Duration::ZERO,
        "the submit span is a wall bracket and is never absent"
    );
    let stabilized = draw_async(
        &mut live,
        frame(Vec::new(), vec![light(0)]),
        WIDTH,
        HEIGHT,
        2,
    );
    assert_ne!(
        first.image.to_bytes(),
        stabilized.image.to_bytes(),
        "async history must advance"
    );

    let mut moved = frame(Vec::new(), vec![light(0)]);
    moved.camera.eye.x += 0.25;
    let camera_reset = draw_async(&mut live, moved, WIDTH, HEIGHT, 3);
    let mut fresh = AsyncViewport::new();
    fresh.set_subframes(1);
    let mut fresh_moved = frame(Vec::new(), vec![light(0)]);
    fresh_moved.camera.eye.x += 0.25;
    let expected_camera = draw_async(&mut fresh, fresh_moved, WIDTH, HEIGHT, 1);
    assert_eq!(
        camera_reset.image.to_bytes(),
        expected_camera.image.to_bytes(),
        "the renderer-thread path must reject history on camera motion"
    );

    let resized = draw_async(
        &mut live,
        {
            let mut input = frame(Vec::new(), vec![light(0)]);
            input.camera.eye.x += 0.25;
            input
        },
        WIDTH + 17,
        HEIGHT + 11,
        4,
    );
    let mut fresh_resize = AsyncViewport::new();
    fresh_resize.set_subframes(1);
    let expected_resize = draw_async(
        &mut fresh_resize,
        {
            let mut input = frame(Vec::new(), vec![light(0)]);
            input.camera.eye.x += 0.25;
            input
        },
        WIDTH + 17,
        HEIGHT + 11,
        1,
    );
    assert_eq!(
        resized.image.to_bytes(),
        expected_resize.image.to_bytes(),
        "resize must discard history and rebuild renderer-thread targets"
    );
}

#[test]
fn async_viewport_coalesces_in_flight_work_and_presents_the_newest_descriptor() {
    let mut live = AsyncViewport::new();
    live.set_subframes(1);
    let mut saw_replacement = false;
    for serial in 1..=16 {
        let mut input = frame(Vec::new(), vec![light(0)]);
        input.camera.eye.x += serial as f32 * 0.02;
        saw_replacement |= matches!(
            live.submit(input, WIDTH, HEIGHT),
            SubmitOutcome::Replaced { .. }
        );
    }
    assert!(
        saw_replacement,
        "a saturated producer must exercise coalescing"
    );
    let newest = wait_for_serial(&live, 16);

    let mut fresh = AsyncViewport::new();
    fresh.set_subframes(1);
    let mut newest_input = frame(Vec::new(), vec![light(0)]);
    newest_input.camera.eye.x += 16.0 * 0.02;
    let expected = draw_async(&mut fresh, newest_input, WIDTH, HEIGHT, 1);
    assert_eq!(
        newest.image.to_bytes(),
        expected.image.to_bytes(),
        "newest-wins coalescing must render the last submitted descriptor"
    );
}
