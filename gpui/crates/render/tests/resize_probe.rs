//! What a *moving* viewport size costs, against a still one.
//!
//! ```sh
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p luma-render --release \
//!     --test resize_probe -- --nocapture
//! ```
//!
//! Scratch instrument, not a gate. The reported symptom is that ⌘B — which
//! slides the sidebar over `luma_ui::motion::SWEEP` and narrows the workspace
//! panel with it — drops the stage far below display rate for the length of
//! the slide. Every frame of that slide hands the renderer a width nobody has
//! asked for before, and `Renderer::targets` reallocates on any size change:
//! eight textures, five presentation surfaces, and the temporal haze history
//! reset to invalid.
//!
//! So the question this answers is narrow and answerable without a window:
//! holding scene, camera and cadence fixed, what does a per-frame size change
//! cost that a fixed size does not? Anything the app can do about ⌘B is bounded
//! above by that difference.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::build_frame_with;
use luma_render::frame::{FixtureCone, Frame};
use luma_render::scene_desc::{CameraPose, DebugView, Environment, RenderSettings, Scene};
use luma_render::viewport::AsyncViewport;

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
            pov: None,
        },
        editing: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        pieces: Vec::new(),
        fixtures: Vec::new(),
        state: BTreeMap::new(),
    }
}

fn lit_frame(lights: usize) -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
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
                wash: 0.0,
                gobo: 0,
                gobo_rotation: 0.0,
            }
        })
        .collect();
    frame.haze_density = 0.65;
    frame
}

fn pct(sorted: &[f64], q: f64) -> f64 {
    sorted
        .get(((sorted.len() as f64 * q) as usize).min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(f64::NAN)
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
    draws.sort_by(f64::total_cmp);
    targets.sort_by(f64::total_cmp);
    totals.sort_by(f64::total_cmp);
    println!(
        "{label:<22} lights={lights:<4} draw p50/p95 {:>7.2}/{:>7.2}ms  \
         cpu_total p50/p95 {:>6.2}/{:>6.2}ms  targets p50/p95 {:>6.2}/{:>6.2}ms",
        pct(&draws, 0.5),
        pct(&draws, 0.95),
        pct(&totals, 0.5),
        pct(&totals, 0.95),
        pct(&targets, 0.5),
        pct(&targets, 0.95),
    );
}

#[test]
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
