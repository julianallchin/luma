//! The zero-copy presentation path and the readback path must be one picture.
//!
//! ```sh
//! cargo test -p luma-render --test shared_presentation
//! ```
//!
//! `docs/design/presentation-seam.md` claims a shared surface is a *transport*
//! choice and not a rendering mode. That claim is only worth making if
//! something checks it, and this is the check: the same descriptor, drawn both
//! ways, byte for byte.
//!
//! One test to a binary, deliberately. The fallback is selected by a process
//! environment variable, and a second test running in another thread of this
//! process would see it flip underneath its own renderer.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::frame::FixtureCone;
use luma_render::scene_desc::{CameraPose, DebugView, Environment, Piece, RenderSettings, Scene};
use luma_render::{build_frame_with, AsyncPresentation, AsyncViewport, Frame, Presented};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;

/// See `luma_render::share::WITHHOLD`, which is private because setting it is a
/// test's business and nothing else's.
const WITHHOLD: &str = "LUMA_WITHHOLD_SHARED_SURFACES";

fn frame() -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let mut render = RenderSettings::dark_stage(48.0, 0.5);
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.haze.enabled = true;
    render.haze.steps = 8;
    render.haze.density = 0.65;
    render.debug_view = DebugView::Pbr;
    let scene = Scene {
        id: "shared-presentation".into(),
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
        pieces: Vec::<Piece>::new(),
        state: BTreeMap::new(),
    };
    let mut frame =
        build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library).unwrap();
    frame.fixture_cones = vec![FixtureCone {
        position: Vec3::new(0.0, 0.0, 0.15),
        range: 8.0,
        direction: Vec3::Z,
        cos_beam: 0.975,
        color: Vec3::new(0.2, 0.55, 1.0),
        intensity: 0.08,
        cos_field: 0.93,
        wash: 0.0,
        gobo: 0,
        gobo_rotation: 0.31,
    }];
    frame.haze_density = 0.65;
    frame
}

/// Draw one deterministic frame on a viewport of its own.
///
/// A fresh viewport per call is what makes the comparison fair: the live path
/// accumulates blue-noise history across frames, so the first frame of a new
/// renderer is the only frame with no history behind it.
fn draw_one() -> AsyncPresentation {
    let mut viewport = AsyncViewport::new();
    viewport.set_subframes(1);
    viewport.submit(frame(), WIDTH, HEIGHT);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(result) = viewport.take_latest() {
            return result.expect("the frame rendered");
        }
        assert!(
            Instant::now() < deadline,
            "the renderer worker never completed a frame"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_shared_surface_holds_exactly_what_a_readback_would_have() {
    let shared = draw_one();

    // SAFETY-of-a-different-kind: this is the only test in this binary, so no
    // other thread is reading the environment while it changes.
    std::env::set_var(WITHHOLD, "1");
    let staged = draw_one();
    std::env::remove_var(WITHHOLD);

    assert!(
        matches!(staged.image, Presented::Pixels(_)),
        "withholding shared surfaces must fall back to a readback"
    );
    assert_eq!(
        (shared.width, shared.height),
        (staged.width, staged.height),
        "the two paths disagreed about the frame's size"
    );

    let shared_bytes = shared.image.to_bytes();
    let staged_bytes = staged.image.to_bytes();
    assert_eq!(
        shared_bytes.len(),
        (WIDTH * HEIGHT * 4) as usize,
        "a shared surface must read back unpadded"
    );

    if !matches!(shared.image, Presented::Pixels(_)) {
        // Only meaningful where sharing is available at all; elsewhere both
        // sides took the same path and the comparison above is vacuous.
        let differing = shared_bytes
            .iter()
            .zip(&staged_bytes)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            differing,
            0,
            "{differing} of {} bytes differ between the shared and readback paths",
            shared_bytes.len()
        );
    }
}
