//! Piece thumbnails for the add-element dialog.
//!
//! The dialog's preview pane owes the operator the *shape* of what a row
//! would put in their hand, and text was standing in for it. Each catalog
//! piece is rendered once, offscreen, on its own renderer and device — the
//! same [`luma_render::Renderer`] path the goldens use — into a PNG the
//! dialog then shows like any other image. Rendering happens on a plain
//! thread because it owns a whole wgpu device for a few seconds; the dialog
//! polls for the files and repaints when they land.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use luma_render::scene_desc::{CameraPose, Editor, Geometry, Piece, RenderSettings, Scene};

/// Where a piece's thumbnail lands. Content-addressed by catalog ref only:
/// the palette renders defaults, and a default is stable per build.
pub(crate) fn thumbnail_path(catalog_ref: &str) -> PathBuf {
    std::env::temp_dir()
        .join("luma-piece-previews")
        .join(format!("{}.png", catalog_ref.replace(['/', ' '], "_")))
}

/// Kick the render thread off once per process. Idempotent and cheap to call
/// from every dialog open.
pub(crate) fn warm() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("piece-previews".into())
        .spawn(render_all)
        .ok();
}

/// Whether every catalog piece already has its file — the dialog's "stop
/// polling" answer.
pub(crate) fn all_ready() -> bool {
    luma_scene::catalog::pieces()
        .iter()
        .all(|piece| thumbnail_path(piece.id).exists())
}

fn render_all() {
    let Ok(mut renderer) = luma_render::Renderer::new() else {
        return;
    };
    let mut library = luma_render::assets::Library::new(luma_lib::stage_render::meshes_root(None));
    let out_dir = std::env::temp_dir().join("luma-piece-previews");
    std::fs::create_dir_all(&out_dir).ok();
    for piece in luma_scene::catalog::pieces() {
        let path = thumbnail_path(piece.id);
        if path.exists() {
            continue;
        }
        let geometry = match piece.geometry {
            luma_scene::catalog::Geometry::Mesh { path } => Geometry::mesh(path),
            luma_scene::catalog::Geometry::Procedural(family) => {
                Geometry::Procedural(luma_render::catalog::default_params(family))
            }
        };
        // The piece's own extent, for a camera that frames anything from a
        // cable ramp to a three-metre stick the same way.
        let (lo, hi) = match &geometry {
            Geometry::MeshPath(mesh) => match library.get(mesh) {
                Ok(glb) => glb.bounds(),
                Err(_) => continue,
            },
            Geometry::Procedural(params) => {
                let bounds = luma_render::catalog::procedural_bounds(*params);
                (bounds.min.as_vec3(), bounds.max.as_vec3())
            }
        };
        let centre = (lo + hi) * 0.5;
        let radius = (hi - lo).length().max(0.4) * 0.5;
        let eye = centre + glam::Vec3::new(1.0, 0.72, 1.0).normalize() * radius * 2.7;
        let scene = Scene {
            id: format!("preview-{}", piece.id),
            times: vec![0.0],
            camera: CameraPose {
                position: eye.to_array(),
                target: centre.to_array(),
            },
            // The editor key light is what lights unlit structure — a
            // thumbnail of a black piece on a black stage said nothing.
            editing: true,
            aim_arrows: false,
            render: RenderSettings::editor_lit(50.0, 0.5),
            selected_fixture_ids: Vec::new(),
            editor: Editor::default(),
            fixtures: Vec::new(),
            pieces: vec![Piece {
                id: "subject".into(),
                geometry,
                kind: String::new(),
                pos: [0.0; 3],
                rot: [0.0; 3],
                scale: 1.0,
            }],
            state: BTreeMap::new(),
        };
        let Ok(frame) = luma_render::build_frame_with(
            &scene,
            &BTreeMap::new(),
            &|_, _| None,
            0.0,
            &mut library,
        ) else {
            continue;
        };
        let Ok(pixels) = renderer.render(&frame, THUMB_W, THUMB_H, THUMB_SUBFRAMES) else {
            continue;
        };
        luma_render::image_out::write(&path, &pixels, THUMB_W, THUMB_H).ok();
    }
}

/// Small enough to render the whole catalog in a couple of seconds, big
/// enough to read a lattice.
const THUMB_W: u32 = 384;
const THUMB_H: u32 = 288;
/// A still of unlit structure needs no converged haze.
const THUMB_SUBFRAMES: u32 = 4;
