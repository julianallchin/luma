//! Thumbnails for the add-element dialog.
//!
//! The dialog's preview pane owes the operator the *shape* of what a row
//! would put in their hand, and text was standing in for it. Each catalog
//! piece is rendered once, offscreen, on its own renderer and device — the
//! same [`luma_render::Renderer`] path the goldens use — into a PNG the
//! dialog then shows like any other image. A fixture is rendered the same
//! way the first time its row is highlighted: there are hundreds, and nobody
//! looks at most of them. Rendering happens on a plain thread because it owns
//! a whole wgpu device; the dialog repaints when the files land.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Mutex, OnceLock};

use luma_render::scene_desc::{
    CameraPose, Editor, Fixture, Geometry, Piece, RenderSettings, Scene,
};

/// Where a piece's thumbnail lands. Content-addressed by catalog ref only:
/// the palette renders defaults, and a default is stable per build.
pub(crate) fn thumbnail_path(catalog_ref: &str) -> PathBuf {
    std::env::temp_dir()
        // Versioned: the files are content-addressed by catalog ref alone, so
        // a style change (the matte, the framing) or a new geometry kind has
        // to move the whole
        // directory or every machine keeps its old opaque renders forever.
        .join("luma-piece-previews-v3")
        .join(format!("{}.png", catalog_ref.replace(['/', ' ', ':'], "_")))
}

/// Kick the render thread off once per process. Idempotent and cheap to call
/// from every dialog open.
pub(crate) fn warm() {
    worker();
}

/// The render thread's inbox: fixture paths to draw once the catalog is done.
fn worker() -> &'static mpsc::Sender<String> {
    static WORKER: OnceLock<mpsc::Sender<String>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (requests, inbox) = mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("element-previews".into())
            .spawn(move || {
                let Ok(mut renderer) = luma_render::Renderer::new() else {
                    return;
                };
                let fixtures_root = crate::library::fixtures_root().ok();
                let mut library = luma_render::assets::Library::new(
                    luma_lib::resources::meshes_root(fixtures_root.as_deref()),
                );
                render_all(&mut renderer, &mut library);
                for path in inbox {
                    if let Some(root) = &fixtures_root {
                        render_fixture(&mut renderer, &mut library, root, &path);
                    }
                    unavailable().lock().unwrap().insert(path);
                }
            })
            .ok();
        requests
    })
}

/// Fixtures the thread has finished with, drawn or not: asked for once.
fn unavailable() -> &'static Mutex<HashSet<String>> {
    static DONE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    DONE.get_or_init(Mutex::default)
}

/// A fixture's thumbnail, as far as it has got.
pub(crate) enum FixtureThumbnail {
    Ready(PathBuf),
    /// Asked for and not back yet: worth another look next frame.
    Rendering,
    /// It cannot be drawn on this machine.
    Unavailable,
}

/// The thumbnail of the fixture at `path`, asking for it on first sight.
pub(crate) fn fixture_thumbnail(path: &str) -> FixtureThumbnail {
    let file = thumbnail_path(path);
    if file.exists() {
        return FixtureThumbnail::Ready(file);
    }
    if unavailable().lock().unwrap().contains(path) {
        return FixtureThumbnail::Unavailable;
    }
    static ASKED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = ASKED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap()
        .insert(path.to_string());
    if first && worker().send(path.to_string()).is_err() {
        // The thread is gone: no device to draw on.
        unavailable().lock().unwrap().insert(path.to_string());
        return FixtureThumbnail::Unavailable;
    }
    FixtureThumbnail::Rendering
}

/// Whether every catalog piece already has its file — the dialog's "stop
/// polling" answer.
pub(crate) fn all_ready() -> bool {
    luma_scene::catalog::pieces()
        .iter()
        .all(|piece| thumbnail_path(piece.id).exists())
}

fn render_all(renderer: &mut luma_render::Renderer, library: &mut luma_render::assets::Library) {
    for piece in luma_scene::catalog::pieces() {
        let path = thumbnail_path(piece.id);
        if path.exists() {
            continue;
        }
        let geometry = match piece.geometry {
            luma_scene::catalog::Geometry::Mesh { path } => Geometry::mesh(path),
            luma_scene::catalog::Geometry::Procedural(family) => {
                Geometry::Procedural(luma_scene::catalog::default_params(family))
            }
            luma_scene::catalog::Geometry::Assembly(_) => Geometry::Assembly(piece.id.to_string()),
        };
        let subject = Subject {
            pieces: vec![Piece {
                id: "subject".into(),
                geometry,
                kind: String::new(),
                pos: [0.0; 3],
                rot: [0.0; 3],
                scale: 1.0,
            }],
            fixtures: Vec::new(),
            definitions: BTreeMap::new(),
            reach: PIECE_REACH,
        };
        shoot(renderer, library, &subject, &path);
    }
}

/// Draw one fixture, standing on its clamp, in the first mode it declares.
fn render_fixture(
    renderer: &mut luma_render::Renderer,
    library: &mut luma_render::assets::Library,
    fixtures_root: &std::path::Path,
    path: &str,
) {
    let Ok(definition) = luma_lib::fixtures::definition(fixtures_root, path) else {
        return;
    };
    let Some(mode) = definition.modes.first() else {
        return;
    };
    let definition = luma_scene::definition::Definition::from(&*definition);
    let subject = Subject {
        fixtures: vec![Fixture {
            id: "subject".into(),
            fixture_path: path.to_string(),
            mode_name: mode.name.clone(),
            pos: [0.0; 3],
            rot: [0.0; 3],
            aim: [0.0; 2],
        }],
        definitions: BTreeMap::from([(path.to_string(), definition)]),
        pieces: Vec::new(),
        reach: FIXTURE_REACH,
    };
    shoot(renderer, library, &subject, &thumbnail_path(path));
}

/// What one thumbnail is a picture of.
struct Subject {
    pieces: Vec<Piece>,
    fixtures: Vec<Fixture>,
    definitions: luma_render::frame::Definitions,
    /// How far back the camera stands, in radii of the subject's box.
    reach: f32,
}

/// Close enough to read a lattice: structure is long and thin, and fills
/// little of the sphere around it.
const PIECE_REACH: f32 = 1.9;
/// A housing is a block that nearly fills its sphere, so the camera stands
/// where the whole sphere is in view.
const FIXTURE_REACH: f32 = 2.5;

/// Render `subject`, framed on what it draws, to a transparent PNG.
fn shoot(
    renderer: &mut luma_render::Renderer,
    library: &mut luma_render::assets::Library,
    subject: &Subject,
    path: &std::path::Path,
) {
    if let Some(out_dir) = path.parent() {
        std::fs::create_dir_all(out_dir).ok();
    }
    let build = |scene: &Scene, library: &mut luma_render::assets::Library| {
        luma_render::build_frame_with(scene, &subject.definitions, &|_, _| None, 0.0, library).ok()
    };
    let scene_at = |eye: glam::Vec3, target: glam::Vec3, render: RenderSettings| Scene {
        id: "preview".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: eye.to_array(),
            target: target.to_array(),
        },
        // The editor key light is what lights unlit structure — a
        // thumbnail of a black piece on a black stage said nothing.
        editing: true,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Editor::default(),
        fixtures: subject.fixtures.clone(),
        pieces: subject.pieces.clone(),
        state: BTreeMap::new(),
    };
    // The subject's own extent, measured off what it draws, for a camera that
    // frames anything from a cable ramp to a moving head the same way —
    // wherever the thing sits about its own origin.
    let probe = scene_at(glam::Vec3::ONE, glam::Vec3::ZERO, bare([0.0; 3]));
    let Some((lo, hi)) = build(&probe, library).and_then(|frame| extent(&frame)) else {
        return;
    };
    let centre = (lo + hi) * 0.5;
    let radius = (hi - lo).length().max(0.4) * 0.5;
    let eye = centre + glam::Vec3::new(1.0, 0.72, 1.0).normalize() * radius * subject.reach;
    // The renderer has no alpha output, so alpha is recovered by difference
    // matting — the same frame over a black and over a grey background
    // diverges exactly where the background shows through, and the
    // divergence is the transparency.
    let mut shot = |background: [f32; 3]| -> Option<Vec<u8>> {
        let frame = build(&scene_at(eye, centre, bare(background)), library)?;
        renderer
            .render(&frame, THUMB_W, THUMB_H, THUMB_SUBFRAMES)
            .ok()
    };
    let (Some(dark), Some(lit)) = (shot([0.0; 3]), shot([MATTE_GREY_LINEAR; 3])) else {
        return;
    };
    let pixels = matte(&dark, &lit);
    luma_render::image_out::write(path, &pixels, THUMB_W, THUMB_H).ok();
}

/// The box around everything a frame draws, in the scene's own axes.
fn extent(frame: &luma_render::Frame) -> Option<(glam::Vec3, glam::Vec3)> {
    frame
        .draws
        .iter()
        .flat_map(|draw| {
            frame.meshes[draw.mesh].vertices.iter().map(|vertex| {
                luma_scene::coords::three_from_world(
                    draw.model
                        .transform_point3(glam::Vec3::from(vertex.position)),
                )
            })
        })
        .fold(None, |acc: Option<(glam::Vec3, glam::Vec3)>, point| {
            Some(acc.map_or((point, point), |(lo, hi)| (lo.min(point), hi.max(point))))
        })
}

/// The dialog wants the *object*, not a stage: no grid, no room, and a ground
/// the card's own surface shows through.
fn bare(background: [f32; 3]) -> RenderSettings {
    let mut render = RenderSettings::object_lit(50.0, 0.5);
    render.show_grid = false;
    // No medium: the composite pass attenuates the clear colour by
    // Beer-Lambert over the *far plane* — with haze on, any
    // background dies to fog and the matte has no step to measure.
    render.haze.enabled = false;
    render.haze.density = 0.0;
    // The subject alone: the venue's 200 m ground plane would read as
    // a horizon in a 300-pixel card.
    render.show_floor = false;
    // Nor is a card a room: a piece the palette shows at head height
    // is not flown, it is just posed for the picture.
    render.show_cables = false;
    // Nor the room's chrome: a compass says which way the house is,
    // and a card has no house.
    render.show_gizmos = false;
    render.environment.background = background;
    render
}

/// Recover alpha from the same frame rendered over black and over grey.
///
/// Where the two frames agree the background never showed through — opaque
/// piece, full alpha. Where they diverge, the divergence over the known
/// background step *is* the coverage: `lit = c + (1-a)·g`, `dark = c`, so
/// `a = 1 - (lit-dark)/g` per channel, worst channel wins. Colour comes from
/// the dark frame, which is already the piece over nothing. The step `g` is
/// *measured* off the top-left pixel — always bare background at this framing
/// — rather than predicted through the display transform.
fn matte(dark: &[u8], lit: &[u8]) -> Vec<u8> {
    let step = (0..3)
        .map(|c| f32::from(lit[c].saturating_sub(dark[c])))
        .fold(1.0f32, f32::max);
    let mut out = dark.to_vec();
    for (px, (d, l)) in out
        .chunks_exact_mut(4)
        .zip(dark.chunks_exact(4).zip(lit.chunks_exact(4)))
    {
        let leak = (0..3)
            .map(|c| f32::from(l[c].saturating_sub(d[c])) / step)
            .fold(0.0f32, f32::max);
        px[3] = ((1.0 - leak.min(1.0)) * 255.0).round() as u8;
    }
    out
}

/// The grey the second matte pass clears to, linear — far enough from black
/// to measure against noise, far enough from white not to clip a lit face.
const MATTE_GREY_LINEAR: f32 = 0.18;

/// Small enough to render the whole catalog in a couple of seconds, big
/// enough to read a lattice.
const THUMB_W: u32 = 384;
const THUMB_H: u32 = 288;
/// A still of unlit structure needs no converged haze.
const THUMB_SUBFRAMES: u32 = 4;

#[cfg(test)]
mod tests {
    /// Renders the whole catalog on a real device — seconds of GPU work, so
    /// opt-in: `cargo test -p luma-app --lib render_every -- --ignored`.
    /// The output is the actual dialog asset set, worth an eye after any
    /// change to the matte or the framing.
    #[test]
    #[ignore = "owns a wgpu device for several seconds"]
    fn render_every_catalog_thumbnail() {
        let mut renderer = luma_render::Renderer::new().unwrap();
        let mut library = luma_render::assets::Library::new(luma_lib::resources::meshes_root(None));
        super::render_all(&mut renderer, &mut library);
        assert!(super::all_ready());
    }

    /// One library fixture and the laser projector, the same way: worth an
    /// eye after any change to how a fixture is framed.
    #[test]
    #[ignore = "owns a wgpu device for several seconds"]
    fn render_fixture_thumbnails() {
        let mut renderer = luma_render::Renderer::new().unwrap();
        let root = luma_lib::resources::fixtures_root().unwrap();
        let mut library =
            luma_render::assets::Library::new(luma_lib::resources::meshes_root(Some(&root)));
        for path in [
            "lib:claypaky/sharpy",
            luma_lib::models::fixtures::LASER_PROJECTOR_PATH,
        ] {
            let file = super::thumbnail_path(path);
            std::fs::remove_file(&file).ok();
            super::render_fixture(&mut renderer, &mut library, &root, path);
            assert!(file.exists(), "{path}");
        }
    }
}
