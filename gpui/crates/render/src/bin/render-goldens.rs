//! Render every golden frame with `luma-render`.
//!
//!     cargo run -p luma-render --release --bin render-goldens
//!     cargo run -p luma-render --release --bin render-goldens -- single-mover
//!     cargo run -p luma-render --bin render-goldens -- --describe-reference
//!
//! Output lands in `harness/goldens/scenes-wgpu/<scene>-<t>.png`, the same
//! names `harness/shot-visualizer.mjs` writes into `harness/goldens/scenes/`,
//! so the two directories compare frame for frame. Every PNG is accompanied by
//! a versioned JSON descriptor containing the complete deterministic input.

use std::path::{Path, PathBuf};

use luma_render::{build_frame, Catalogue, Renderer, DEFAULT_SUBFRAMES};

fn main() -> anyhow::Result<()> {
    let repo = repo_root();
    let catalogue = Catalogue::load(&repo.join("gpui/crates/render/goldens/scenes.json"))?;
    let mut requested: Vec<String> = std::env::args().skip(1).collect();
    let describe_reference = requested
        .first()
        .is_some_and(|argument| argument == "--describe-reference");
    if describe_reference {
        requested.remove(0);
    }
    let out_dir = repo.join(if describe_reference {
        "harness/goldens/scenes"
    } else {
        "harness/goldens/scenes-wgpu"
    });
    std::fs::create_dir_all(&out_dir)?;

    let (width, height) = catalogue.frame_size();
    let mut renderer = (!describe_reference).then(Renderer::new).transpose()?;
    let mut library = luma_render::assets::Library::new(repo.join("resources/meshes"));
    let descriptor_subframes = if describe_reference {
        catalogue.warmup_frames
    } else {
        DEFAULT_SUBFRAMES
    };

    for scene in &catalogue.scenes {
        if !requested.is_empty() && !requested.contains(&scene.id) {
            continue;
        }
        for &t in &scene.times {
            let path = out_dir.join(scene.frame_name(t));
            let stats = if let Some(renderer) = &mut renderer {
                let frame = build_frame(scene, &catalogue.definitions, t, &mut library)?;
                let pixels = renderer.render(&frame, width, height, DEFAULT_SUBFRAMES)?;
                write_png(&path, &pixels, width, height)?;
                format!(
                    "{} draws, {} cones",
                    frame.draws.len(),
                    frame.fixture_cones.len()
                )
            } else {
                anyhow::ensure!(
                    path.is_file(),
                    "refusing to write a descriptor without its reference image: {}",
                    path.display()
                );
                "reference inputs".into()
            };
            let descriptor = catalogue.frame_descriptor(scene, t, descriptor_subframes)?;
            write_json(&out_dir.join(scene.descriptor_name(t)), &descriptor)?;
            println!("{}  {stats}", path.display());
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

fn write_json(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    std::io::Write::write_all(&mut writer, b"\n")?;
    Ok(())
}
