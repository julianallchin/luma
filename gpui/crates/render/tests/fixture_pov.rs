//! `render(view=pov:<fixture>)`: a camera in one fixture's head.
//!
//! Frames, not matrices. A camera assertion written against `eye` and `target`
//! passes for a camera that renders nothing — the interesting failures here
//! (a mirrored axis, an eye left inside its own housing, a lens taken from the
//! wrong fixture) all reach the pixels and none of them reach the numbers you
//! would have written down.
//!
//! Compared as **coarse luminance histograms**. The `fixture-pov` scene is
//! symmetric about the room's centre line, so the two wings see mirror images:
//! equal distributions of light, different pixels. A histogram is the statistic
//! that says exactly that and nothing more.
//!
//! Run `cargo test -p luma-render --test fixture_pov`. Needs a GPU.

use std::path::{Path, PathBuf};

use luma_render::assets::Library;
use luma_render::scene_desc::{Catalogue, Scene};
use luma_render::{build_frame, Renderer, DEFAULT_SUBFRAMES};

/// The golden scene this is all about: two movers on opposite wings, each on a
/// truss, each facing the house at its own deck, and a room that is symmetric
/// between them.
const SCENE: &str = "fixture-pov";

/// Buckets in a luminance histogram. Coarse on purpose: the claim is "these two
/// frames have the same light in them", and 16 buckets over a frame of half a
/// million pixels is loose enough to survive rasterization and tight enough
/// that a differently-aimed camera cannot pass.
const BUCKETS: usize = 16;

/// Small frames. The comparison is statistical, so resolution buys nothing and
/// costs a second per render.
const SIZE: (u32, u32) = (320, 200);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn catalogue() -> Catalogue {
    Catalogue::load(&repo_root().join("gpui/crates/render/goldens/scenes.json"))
        .expect("the golden catalogue loads")
}

fn scene_of(catalogue: &mut Catalogue) -> &mut Scene {
    catalogue
        .scenes
        .iter_mut()
        .find(|scene| scene.id == SCENE)
        .expect("the catalogue has the fixture-pov scene")
}

/// One frame through `fixture`'s head, as sRGB RGBA8.
fn shot(catalogue: &mut Catalogue, renderer: &mut Renderer, fixture: &str) -> Vec<u8> {
    scene_of(catalogue).camera.pov = Some(fixture.to_string());
    let mut library = Library::new(repo_root().join("resources/meshes"));
    let scene = catalogue
        .scenes
        .iter()
        .find(|scene| scene.id == SCENE)
        .expect("just set it");
    let frame =
        build_frame(scene, &catalogue.definitions, 0.0, &mut library).expect("the frame builds");
    renderer
        .render(&frame, SIZE.0, SIZE.1, DEFAULT_SUBFRAMES)
        .expect("the frame renders")
}

/// Fraction of pixels in each luminance bucket. Rec. 709 luma off the sRGB
/// bytes: the frames are compared as pictures, and a picture is what the
/// display transform already produced.
fn histogram(rgba: &[u8]) -> [f64; BUCKETS] {
    let mut counts = [0u64; BUCKETS];
    for px in rgba.chunks_exact(4) {
        let luma =
            0.2126 * f64::from(px[0]) + 0.7152 * f64::from(px[1]) + 0.0722 * f64::from(px[2]);
        let bucket = ((luma / 256.0) * BUCKETS as f64) as usize;
        counts[bucket.min(BUCKETS - 1)] += 1;
    }
    let total = counts.iter().sum::<u64>() as f64;
    counts.map(|c| c as f64 / total)
}

/// Total variation distance between two distributions: 0 identical, 1 disjoint.
fn distance(a: &[f64; BUCKETS], b: &[f64; BUCKETS]) -> f64 {
    0.5 * a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>()
}

/// Two heads on opposite wings, both facing the house across a symmetric room,
/// see mirror images: the same light, arranged the other way round.
///
/// Both halves are asserted. Matching histograms alone would also pass for a
/// camera that ignored the fixture entirely and put both eyes in one place, so
/// the frames must differ as pixels as well.
#[test]
fn opposite_wings_are_mirror_images() {
    let mut catalogue = catalogue();
    let mut renderer = Renderer::new().expect("a GPU device");
    let left = shot(&mut catalogue, &mut renderer, "wing-left");
    let right = shot(&mut catalogue, &mut renderer, "wing-right");

    let spread = distance(&histogram(&left), &histogram(&right));
    assert!(
        spread < 0.02,
        "the two wings should see the same light: total variation {spread:.4}"
    );
    assert_ne!(
        left, right,
        "the two wings should not see the *same picture* — the camera is not following the fixture"
    );
}

/// Changing where a head points changes what its POV shows.
///
/// The aim at rest is the mount frame, so this rotates the mount rather than
/// pushing pan/tilt through the state: `Scene::pov` reads the parked direction
/// on purpose, and a test that moved the head would be measuring the thing the
/// camera deliberately ignores.
#[test]
fn a_different_aim_is_a_different_frame() {
    let mut catalogue = catalogue();
    let mut renderer = Renderer::new().expect("a GPU device");
    let hung = shot(&mut catalogue, &mut renderer, "wing-left");

    // Swing the mount a quarter turn upstage, off everything it was pointing at.
    let scene = scene_of(&mut catalogue);
    let mover = scene
        .fixtures
        .iter_mut()
        .find(|f| f.id == "wing-left")
        .expect("the scene patches wing-left");
    mover.rot[0] -= std::f32::consts::FRAC_PI_2;
    let swung = shot(&mut catalogue, &mut renderer, "wing-left");

    let moved = distance(&histogram(&hung), &histogram(&swung));
    assert!(
        moved > 0.05,
        "re-aiming the head should change the frame: total variation {moved:.4}"
    );
}

/// A view through a fixture the venue does not have is refused by name, not
/// silently pointed somewhere.
#[test]
fn an_unpatched_fixture_is_not_a_camera() {
    let mut catalogue = catalogue();
    let mut library = Library::new(repo_root().join("resources/meshes"));
    scene_of(&mut catalogue).camera.pov = Some("no-such-head".into());
    let scene = catalogue
        .scenes
        .iter()
        .find(|scene| scene.id == SCENE)
        .expect("just set it");
    let error = match build_frame(scene, &catalogue.definitions, 0.0, &mut library) {
        Ok(_) => panic!("a frame was built for a fixture the scene does not patch"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("no-such-head"),
        "the refusal should name the fixture: {error}"
    );
}
