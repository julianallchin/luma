//! What a *moving* viewport size costs, against a still one.
//!
//! ```sh
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo run -p luma-render --release \
//!     --example resize_probe
//! ```
//!
//! A measuring instrument, not a gate. The reported symptom is that ⌘B — which
//! slides the sidebar over `luma_ui::motion::SWEEP` and narrows the workspace
//! panel with it — drops the stage far below display rate for the length of
//! the slide. Every frame of that slide hands the renderer a width nobody has
//! asked for before, and `Renderer::targets` reallocates on any size change:
//! the fixed-size targets and the presentation surfaces.
//!
//! So the question this answers is narrow and answerable without a window:
//! holding scene, camera and cadence fixed, what does a per-frame size change
//! cost that a fixed size does not? Anything the app can do about ⌘B is bounded
//! above by that difference.
//!
//! It then drags: the stage widening two pixels a frame, in blocks between
//! still blocks, and fails when the median dragging frame costs more than
//! [`DRAG_BOUND`] times the median still one. The same symptom from the other
//! side: dragging the agent panel's edge or the window stuttered for the first
//! second. `LUMA_RESIZE_PASSES=1` instead prints, per pass and CPU span, what
//! a dragging frame spends that a still one does not.

use std::collections::BTreeMap;
use std::time::Duration;

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::build_frame_with;
use luma_render::frame::{FixtureCone, Frame};
use luma_render::scene_desc::{CameraPose, DebugView, Environment, RenderSettings, Scene};
use luma_render::viewport::AsyncViewport;
use luma_render::MetricSummary;

/// A workspace panel on a retina 16" window, with the sidebar shut.
/// `LUMA_RESIZE_WINDOW=WxH` runs the same sweep at another size: the cost is
/// pixel-linear, so one size alone would understate a full-screen stage.
const DEFAULT_SIZE: (u32, u32) = (1900, 1200);

fn size() -> (u32, u32) {
    let Ok(spec) = std::env::var("LUMA_RESIZE_WINDOW") else {
        return DEFAULT_SIZE;
    };
    let (w, h) = spec
        .split_once('x')
        .expect("LUMA_RESIZE_WINDOW is WIDTHxHEIGHT");
    (w.parse().expect("width"), h.parse().expect("height"))
}
/// How much of that width a ⌘B takes away. The sidebar is 256pt and the
/// thread/panel split is even, so the panel pays half of it, doubled for the
/// backing scale.
const DELTA: u32 = 256;
/// Frames a `SWEEP` slide covers at display rate — the count of *distinct*
/// widths one ⌘B produces.
const SLIDE_FRAMES: u32 = 32;

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
        id: "resize-probe".into(),
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
        pieces: Vec::new(),
        fixtures: Vec::new(),
        state: BTreeMap::new(),
    }
}

fn lit_frame(lights: usize) -> Frame {
    let meshes = luma_scene::meshes_root();
    let mut library = Library::new(meshes);
    let scene = scene();
    let mut frame = build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library)
        .expect("frame builds");
    frame.fixture_cones = (0..lights)
        .map(|i| {
            let angle = i as f32 * 0.7;
            FixtureCone {
                position: Vec3::new((i % 8) as f32 - 4.0, 4.0, (i / 8) as f32 - 2.0),
                range: 8.0,
                direction: Vec3::new(angle.sin() * 0.3, -1.0, angle.cos() * 0.3).normalize(),
                cos_beam: 0.975,
                color: Vec3::new(1.0, 0.3, 0.2),
                intensity: 0.4,
                cos_field: 0.93,
                gobo: 0,
                gobo_rotation: 0.0,
                haze_gain: 1.0,
                lens: luma_scene::luminaire::Lens::POINT,
            }
        })
        .collect();
    frame.haze_density = 0.65;
    frame
}

/// One unpipelined pass: submit, wait, measure. Pipelining would hide the
/// reallocation behind the previous frame's GPU work, which is exactly the
/// term being measured.
fn sweep(label: &str, lights: usize, widths: impl Fn(u32) -> u32) {
    let (_, height) = size();
    let mut viewport = AsyncViewport::new();
    viewport.set_subframes(1);
    let mut draws = Vec::new();
    let mut targets = Vec::new();
    let mut totals = Vec::new();
    for serial in 0..SLIDE_FRAMES + 8 {
        viewport.submit(lit_frame(lights), widths(serial), height);
        let presented = loop {
            if let Some(result) = viewport.take_latest() {
                break result.expect("frame renders");
            }
            std::thread::sleep(Duration::from_micros(200));
        };
        // The first eight are the warm-up the pipelined probes also discard:
        // first-touch allocation, pipeline compilation, atlas upload.
        if serial >= 8 {
            draws.push(presented.draw_time.as_secs_f64() * 1000.0);
            targets.push(presented.cpu.targets.as_secs_f64() * 1000.0);
            totals.push(presented.cpu.total.as_secs_f64() * 1000.0);
        }
    }
    let [draws, totals, targets] =
        [draws, totals, targets].map(|v| MetricSummary::of(v).expect("frames measured"));
    println!(
        "{label:<22} lights={lights:<4} draw p50/p95 {:>7.2}/{:>7.2}ms  \
         cpu_total p50/p95 {:>6.2}/{:>6.2}ms  targets p50/p95 {:>6.2}/{:>6.2}ms",
        draws.p50_ms, draws.p95_ms, totals.p50_ms, totals.p95_ms, targets.p50_ms, targets.p95_ms,
    );
}

fn a_moving_viewport_size_reports_what_it_costs() {
    for lights in [0usize, 30] {
        let (wide, _) = size();
        sweep("still", lights, move |_| wide);
        // What ⌘B actually hands the renderer: a fresh width every frame,
        // eased, so consecutive frames differ by 1..12 px and never repeat.
        sweep("sliding", lights, move |serial| {
            let t = (f64::from(serial.min(SLIDE_FRAMES)) / f64::from(SLIDE_FRAMES)).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            wide - (eased * f64::from(DELTA)) as u32
        });
        // The fix's shape, measured as an upper bound before it is written:
        // one reallocation at the destination, then the whole slide rendered
        // at a size the renderer has already seen.
        sweep("snapped", lights, move |_| wide - DELTA);
    }
}

/// Frames in one still or one dragging block of [`drag`].
const BLOCK: usize = 8;
/// Still/dragging block pairs [`drag`] measures.
const BLOCKS: usize = 10;
/// How much slower than a still frame the median dragging frame may be.
const DRAG_BOUND: f64 = 1.3;

/// A drag as a user makes one, against standing still, in the same run.
///
/// The stage widens two pixels a frame, the way dragging the agent panel's
/// edge or the window's hands it a new size every frame. Blocks of dragging
/// frames alternate with blocks of still frames at the size the drag has
/// reached, so both see the same machine load and every dragging block is
/// the *start* of a drag — where a cold reallocation used to show worst.
/// Each frame is submitted and waited for alone, as in [`sweep`].
fn drag(lights: usize) -> [MetricSummary; 2] {
    let (mut width, height) = size();
    let mut viewport = AsyncViewport::new();
    viewport.set_subframes(1);
    let mut draw = |width: u32| {
        viewport.submit(lit_frame(lights), width, height);
        let presented = loop {
            if let Some(result) = viewport.take_latest() {
                break result.expect("frame renders");
            }
            std::thread::sleep(Duration::from_micros(200));
        };
        presented.draw_time.as_secs_f64() * 1000.0
    };
    for _ in 0..8 {
        draw(width);
    }
    let (mut still, mut dragging) = (Vec::new(), Vec::new());
    for _ in 0..BLOCKS {
        for _ in 0..BLOCK {
            still.push(draw(width));
        }
        for _ in 0..BLOCK {
            width += 2;
            dragging.push(draw(width));
        }
    }
    [still, dragging].map(|v| MetricSummary::of(v).expect("frames measured"))
}

/// Per pass and per CPU span, what a dragging frame spends that a still one
/// does not: the instrument for finding what a resize still reallocates.
/// `LUMA_PROFILE_DETAIL=1` adds the pass brackets.
fn drag_passes(lights: usize) {
    let (wide, height) = size();
    let mut renderer = luma_render::Renderer::new_profiled().expect("renderer");
    let frame = lit_frame(lights);
    let mut spans: [BTreeMap<&'static str, Vec<f64>>; 2] = Default::default();
    for serial in 0..200u32 {
        let width = if serial < 100 {
            wide
        } else {
            wide + 2 * (serial - 99)
        };
        let started = std::time::Instant::now();
        let t = renderer
            .profile_live_surface(&frame, width, height, 1)
            .expect("frame");
        let wall = started.elapsed().as_secs_f64() * 1000.0;
        if serial % 100 < 20 {
            continue;
        }
        let into = &mut spans[usize::from(serial >= 100)];
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        for (name, value) in [
            ("wall", wall),
            ("gpu_total", t.gpu_total_ms),
            ("cpu_total", ms(t.cpu.total)),
            ("cpu_targets", ms(t.cpu.targets)),
            ("cpu_upload", ms(t.cpu.upload)),
            ("cpu_encode", ms(t.cpu.encode)),
            ("cpu_submit", ms(t.cpu.submission.submit)),
        ] {
            into.entry(name).or_default().push(value);
        }
        for pass in &t.passes {
            into.entry(pass.name)
                .or_default()
                .push(pass.end_ms - pass.start_ms);
        }
    }
    let median = |v: &Vec<f64>| MetricSummary::of(v.iter().copied()).map_or(f64::NAN, |s| s.p50_ms);
    println!("passes lights={lights}  median still -> dragging (ms)");
    for (name, values) in &spans[1] {
        let still = spans[0].get(name).map_or(f64::NAN, median);
        println!("  {name:<32} {still:>7.2} -> {:>7.2}", median(values));
    }
}

fn main() {
    if std::env::var_os("LUMA_RESIZE_PASSES").is_some() {
        for lights in [0usize, 30] {
            drag_passes(lights);
        }
        return;
    }
    a_moving_viewport_size_reports_what_it_costs();
    let mut slow = false;
    for lights in [0usize, 30] {
        let [still, dragging] = drag(lights);
        let ratio = dragging.p50_ms / still.p50_ms;
        println!(
            "drag {lights:>2} lights: still p50/p95/max {:>6.2}/{:>6.2}/{:>6.2}ms  \
             dragging {:>6.2}/{:>6.2}/{:>6.2}ms  median x{ratio:.2} (bound x{DRAG_BOUND})",
            still.p50_ms,
            still.p95_ms,
            still.max_ms,
            dragging.p50_ms,
            dragging.p95_ms,
            dragging.max_ms,
        );
        slow |= ratio > DRAG_BOUND;
    }
    if slow {
        eprintln!("a dragging frame costs more than x{DRAG_BOUND} a still one");
        std::process::exit(1);
    }
}
