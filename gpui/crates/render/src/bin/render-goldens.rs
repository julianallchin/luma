//! Render every golden frame with `luma-render`.
//!
//!     cargo run -p luma-render --release --bin render-goldens
//!     cargo run -p luma-render --release --bin render-goldens -- single-mover
//!
//! Output lands in `harness/goldens/scenes-wgpu/<scene>-<t>.png`, the same
//! names `harness/shot-visualizer.mjs` writes into `harness/goldens/scenes/`,
//! so the two directories compare frame for frame.

use std::path::{Path, PathBuf};

use luma_render::{build_frame, Catalogue, Renderer, DEFAULT_SUBFRAMES};

fn main() -> anyhow::Result<()> {
    let repo = repo_root();
    let catalogue = Catalogue::load(&repo.join("gpui/crates/render/goldens/scenes.json"))?;
    let mut library = luma_render::assets::Library::new(repo.join("resources/meshes"));
    let out_dir = repo.join("harness/goldens/scenes-wgpu");
    std::fs::create_dir_all(&out_dir)?;

    let requested: Vec<String> = std::env::args().skip(1).collect();
    let (width, height) = catalogue.frame_size();
    let mut renderer = Renderer::new()?;

    for scene in &catalogue.scenes {
        if !requested.is_empty() && !requested.contains(&scene.id) {
            continue;
        }
        for &t in &scene.times {
            let frame = build_frame(scene, &catalogue.definitions, t, &mut library)?;
            let pixels = renderer.render(&frame, width, height, DEFAULT_SUBFRAMES)?;
            let path = out_dir.join(scene.frame_name(t));
            write_png(&path, &pixels, width, height)?;
            println!(
                "{}  {} draws, {} cones",
                path.display(),
                frame.draws.len(),
                frame.haze_lights.len()
            );
        }
    }
    Ok(())
}

/// The crate sits at `<repo>/gpui/crates/render`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate is three levels below the repo root")
        .to_path_buf()
}

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}
