//! Thumbnails for the add-element dialog.
//!
//! The dialog's preview pane owes the operator the *shape* of what a row
//! would put in their hand, and text was standing in for it. A subject is
//! rendered offscreen, on its own renderer and device — the same
//! [`luma_render::Renderer`] path the goldens use — into a PNG the dialog
//! then shows like any other image, one per angle it has been turned to.
//! The catalog's pieces are rendered ahead at the angle the dialog opens on;
//! everything else is rendered when it is first looked at: there are
//! hundreds of fixtures, and nobody looks at most of them. Rendering happens
//! on a plain thread because it owns a whole wgpu device; the dialog
//! repaints when the files land.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Mutex, OnceLock};

use luma_render::scene_desc::{
    CameraPose, Editor, Fixture, Geometry, Piece, RenderSettings, Scene,
};

/// What a thumbnail is a picture of, and from where.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct View {
    pub(crate) subject: Subject,
    /// How far the subject has been turned, in [`TURN_STEP_DEG`] steps.
    pub(crate) turn: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Subject {
    /// A catalog piece, by its catalog ref.
    Piece(String),
    /// A fixture, by its definition's path.
    Fixture(String),
}

/// One step of turning a subject. Coarse enough that a full turn is a few
/// dozen files, fine enough to read as turning.
pub(crate) const TURN_STEP_DEG: i32 = 10;
const TURN_STEPS: i32 = 360 / TURN_STEP_DEG;

impl View {
    pub(crate) fn of(subject: Subject, turn: i32) -> Self {
        Self {
            subject,
            turn: turn.rem_euclid(TURN_STEPS),
        }
    }

    /// Where this view's file lands. Content-addressed by subject and angle:
    /// a default is stable per build.
    fn file(&self) -> PathBuf {
        let (Subject::Piece(name) | Subject::Fixture(name)) = &self.subject;
        std::env::temp_dir()
            // Versioned: a style change (the matte, the framing) or a new
            // geometry kind has to move the whole directory or every machine
            // keeps its old renders forever.
            .join("luma-element-previews-v1")
            .join(format!(
                "{}@{}.png",
                name.replace(['/', ' ', ':'], "_"),
                self.turn * TURN_STEP_DEG
            ))
    }
}

/// Kick the render thread off once per process. Idempotent and cheap to call
/// from every dialog open.
pub(crate) fn warm() {
    worker();
}

/// The render thread's inbox: views to draw once the catalog is done.
fn worker() -> &'static mpsc::Sender<View> {
    static WORKER: OnceLock<mpsc::Sender<View>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (requests, inbox) = mpsc::channel::<View>();
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
                for piece in luma_scene::catalog::pieces() {
                    let view = View::of(Subject::Piece(piece.id.to_string()), 0);
                    render(&mut renderer, &mut library, fixtures_root.as_deref(), &view);
                }
                while let Ok(mut view) = inbox.recv() {
                    // A subject being turned asks for every angle it passes
                    // through; only where it is now is worth drawing.
                    while let Ok(later) = inbox.try_recv() {
                        view = later;
                    }
                    render(&mut renderer, &mut library, fixtures_root.as_deref(), &view);
                    if !view.file().exists() {
                        undrawable().lock().unwrap().insert(view);
                    }
                }
            })
            .ok();
        requests
    })
}

/// Views the thread tried and could not draw: not worth asking for again.
fn undrawable() -> &'static Mutex<HashSet<View>> {
    static FAILED: OnceLock<Mutex<HashSet<View>>> = OnceLock::new();
    FAILED.get_or_init(Mutex::default)
}

/// A thumbnail, as far as it has got.
pub(crate) enum Thumbnail {
    Ready(PathBuf),
    /// Asked for and not back yet: worth another look next frame.
    Rendering,
    /// It cannot be drawn on this machine.
    Unavailable,
}

/// The thumbnail of `view`, asking for it while it is missing.
pub(crate) fn thumbnail(view: &View) -> Thumbnail {
    let file = view.file();
    if file.exists() {
        return Thumbnail::Ready(file);
    }
    // A send that fails means the thread is gone: no device to draw on.
    if undrawable().lock().unwrap().contains(view) || worker().send(view.clone()).is_err() {
        return Thumbnail::Unavailable;
    }
    Thumbnail::Rendering
}

/// Whether every catalog piece already has its file — the dialog's "stop
/// polling" answer.
pub(crate) fn all_ready() -> bool {
    luma_scene::catalog::pieces().iter().all(|piece| {
        View::of(Subject::Piece(piece.id.to_string()), 0)
            .file()
            .exists()
    })
}

/// Draw `view` to its file, unless it is already there.
fn render(
    renderer: &mut luma_render::Renderer,
    library: &mut luma_render::assets::Library,
    fixtures_root: Option<&std::path::Path>,
    view: &View,
) {
    let file = view.file();
    if file.exists() {
        return;
    }
    let posed = match &view.subject {
        Subject::Piece(catalog_ref) => piece(catalog_ref),
        Subject::Fixture(path) => fixtures_root.and_then(|root| fixture(root, path)),
    };
    if let Some(posed) = posed {
        shoot(renderer, library, &posed, view.turn, &file);
    }
}

/// A catalog piece at its defaults.
fn piece(catalog_ref: &str) -> Option<Posed> {
    let piece = luma_scene::catalog::piece(catalog_ref)?;
    let geometry = match piece.geometry {
        luma_scene::catalog::Geometry::Mesh { path } => Geometry::mesh(path),
        luma_scene::catalog::Geometry::Procedural(family) => {
            Geometry::Procedural(luma_scene::catalog::default_params(family))
        }
        luma_scene::catalog::Geometry::Assembly(_) => Geometry::Assembly(piece.id.to_string()),
    };
    Some(Posed {
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
    })
}

/// One fixture, hanging by its clamp, in the first mode it declares.
fn fixture(fixtures_root: &std::path::Path, path: &str) -> Option<Posed> {
    let definition = luma_lib::fixtures::definition(fixtures_root, path).ok()?;
    let mode = definition.modes.first()?;
    Some(Posed {
        fixtures: vec![Fixture {
            id: "subject".into(),
            fixture_path: path.to_string(),
            mode_name: mode.name.clone(),
            pos: [0.0; 3],
            rot: [0.0; 3],
            aim: [0.0; 2],
        }],
        definitions: BTreeMap::from([(
            path.to_string(),
            luma_scene::definition::Definition::from(&*definition),
        )]),
        pieces: Vec::new(),
        reach: FIXTURE_REACH,
    })
}

/// A subject as the renderer is handed it.
struct Posed {
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

/// Render `subject`, framed on what it draws and turned `turn` steps about
/// the vertical, to a transparent PNG.
fn shoot(
    renderer: &mut luma_render::Renderer,
    library: &mut luma_render::assets::Library,
    subject: &Posed,
    turn: i32,
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
    let turned = glam::Quat::from_rotation_y(((turn * TURN_STEP_DEG) as f32).to_radians());
    let eye =
        centre + turned * glam::Vec3::new(1.0, 0.72, 1.0).normalize() * radius * subject.reach;
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
    use super::{Subject, View};

    /// Renders a piece, a library fixture and the laser projector on a real
    /// device, turned and not — seconds of GPU work, so opt-in:
    /// `cargo test -p luma-app --lib render_previews -- --ignored`. The
    /// output is what the dialog shows, worth an eye after any change to the
    /// matte or the framing.
    #[test]
    #[ignore = "owns a wgpu device for several seconds"]
    fn render_previews() {
        let mut renderer = luma_render::Renderer::new().unwrap();
        let root = luma_lib::resources::fixtures_root().unwrap();
        let mut library =
            luma_render::assets::Library::new(luma_lib::resources::meshes_root(Some(&root)));
        for subject in [
            Subject::Piece(crate::stage::hand::TRUSS_STRAIGHT.into()),
            Subject::Fixture("lib:claypaky/sharpy".into()),
            Subject::Fixture(luma_lib::models::fixtures::LASER_PROJECTOR_PATH.into()),
        ] {
            for turn in [0, 9] {
                let view = View::of(subject.clone(), turn);
                std::fs::remove_file(view.file()).ok();
                super::render(&mut renderer, &mut library, Some(&root), &view);
                assert!(view.file().exists(), "{view:?}");
            }
        }
    }
}
