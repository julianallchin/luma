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
        // Versioned: the files are content-addressed by catalog ref alone, so
        // a style change (the matte, the framing) has to move the whole
        // directory or every machine keeps its old opaque renders forever.
        .join("luma-piece-previews-v2")
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
    // Derived from `thumbnail_path` so the two cannot name different
    // directories.
    if let Some(out_dir) = thumbnail_path("probe").parent() {
        std::fs::create_dir_all(out_dir).ok();
    }
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
        let eye = centre + glam::Vec3::new(1.0, 0.72, 1.0).normalize() * radius * 1.9;
        // The dialog wants the *object*, not a stage: no grid, no room, and a
        // transparent ground the card's own surface shows through. The
        // renderer has no alpha output, so alpha is recovered by difference
        // matting — the same frame over a black and over a grey background
        // diverges exactly where the background shows through, and the
        // divergence is the transparency.
        let scene_with = |background: [f32; 3]| {
            let mut render = RenderSettings::editor_lit(50.0, 0.5);
            render.show_grid = false;
            // No medium: the composite pass attenuates the clear colour by
            // Beer-Lambert over the *far plane* — with haze on, any
            // background dies to fog and the matte has no step to measure.
            render.haze.enabled = false;
            render.haze.density = 0.0;
            // The subject alone: the venue's 200 m ground plane would read as
            // a horizon in a 300-pixel card.
            render.show_floor = false;
            render.environment.background = background;
            Scene {
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
                render,
                selected_fixture_ids: Vec::new(),
                editor: Editor::default(),
                fixtures: Vec::new(),
                pieces: vec![Piece {
                    id: "subject".into(),
                    geometry: geometry.clone(),
                    kind: String::new(),
                    pos: [0.0; 3],
                    rot: [0.0; 3],
                    scale: 1.0,
                }],
                state: BTreeMap::new(),
            }
        };
        let mut shot = |background: [f32; 3]| -> Option<Vec<u8>> {
            let scene = scene_with(background);
            let frame = luma_render::build_frame_with(
                &scene,
                &BTreeMap::new(),
                &|_, _| None,
                0.0,
                &mut library,
            )
            .ok()?;
            renderer
                .render(&frame, THUMB_W, THUMB_H, THUMB_SUBFRAMES)
                .ok()
        };
        let (Some(dark), Some(lit)) = (shot([0.0; 3]), shot([MATTE_GREY_LINEAR; 3])) else {
            continue;
        };
        let pixels = matte(&dark, &lit);
        luma_render::image_out::write(&path, &pixels, THUMB_W, THUMB_H).ok();
    }
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
        super::render_all();
        assert!(super::all_ready());
    }
}
