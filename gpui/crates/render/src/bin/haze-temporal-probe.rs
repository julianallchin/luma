//! Temporal artefact probe for the baked density field — a diagnostic, not a gate.
//!
//! Every still-image check is blind to swimming: the field drifts with
//! `elapsed`, so a reconstruction artefact can be invisible in one frame and
//! obvious in motion. This renders a short sequence at 60 Hz steps and reports
//! the frame-to-frame delta distribution, which is the thing that has to match
//! between the old field and the new one.
//!
//! The regime matters. Drift is `elapsed * (0.4, 0.25, 0.15)` in units of `q`,
//! so at 1/60 s a frame advects the field by 0.0067 lattice units — against a
//! texel spacing of 1/`TEXELS_PER_CELL` = 0.25. Twenty-four frames move it less
//! than one texel, which is exactly where piecewise-linear reconstruction would
//! stair-step if it were going to.
//!
//!     cargo run -p luma-render --release --bin haze-temporal-probe -- <out-dir>

use std::path::PathBuf;

use luma_render::{assets, build_frame, Catalogue, Renderer, DEFAULT_SUBFRAMES};

const FRAMES: usize = 60;
const DT: f32 = 1.0 / 60.0;
const START: f32 = 1.37;

fn main() -> anyhow::Result<()> {
    let repo = repo_root();
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: haze-temporal-probe <out-dir>"))?,
    );
    std::fs::create_dir_all(&out)?;
    let catalogue = Catalogue::load(&repo.join("gpui/crates/render/goldens/scenes.json"))?;
    let mut renderer = Renderer::new()?;
    let mut library = assets::Library::new(repo.join("resources/meshes"));
    let (width, height) = catalogue.frame_size();

    // `single-mover` is the source `one-beam` is derived from, so this covers
    // the single-beam case the review set names.
    for id in ["single-mover", "dense-venue"] {
        let scene = catalogue
            .scenes
            .iter()
            .find(|scene| scene.id == id)
            .ok_or_else(|| anyhow::anyhow!("missing scene {id}"))?;
        let mut previous: Option<Vec<u8>> = None;
        let mut deltas = Vec::new();
        for step in 0..FRAMES {
            let time = START + step as f32 * DT;
            let frame = build_frame(scene, &catalogue.definitions, time, &mut library)?;
            let pixels = renderer.render(&frame, width, height, DEFAULT_SUBFRAMES)?;
            write_png(
                &out.join(format!("{id}-{step:02}.png")),
                &pixels,
                width,
                height,
            )?;
            if let Some(previous) = &previous {
                let n = pixels.len();
                let mut sum = 0u64;
                let mut max = 0u8;
                let mut changed = 0u64;
                for i in 0..n {
                    let d = pixels[i].abs_diff(previous[i]);
                    sum += u64::from(d);
                    max = max.max(d);
                    changed += u64::from(d != 0);
                }
                deltas.push((
                    sum as f64 / n as f64,
                    max,
                    100.0 * changed as f64 / n as f64,
                ));
            }
            previous = Some(pixels.to_vec());
        }
        let mean: f64 = deltas.iter().map(|d| d.0).sum::<f64>() / deltas.len() as f64;
        let peak = deltas.iter().map(|d| d.1).max().unwrap_or(0);
        let spread: f64 = {
            let var =
                deltas.iter().map(|d| (d.0 - mean).powi(2)).sum::<f64>() / deltas.len() as f64;
            var.sqrt()
        };
        let touched: f64 = deltas.iter().map(|d| d.2).sum::<f64>() / deltas.len() as f64;
        println!(
            "{id}: {} steps  frame-to-frame mean|d| {mean:.4}/255 (sd {spread:.4})  \
             peak {peak}  touched {touched:.1}%",
            deltas.len()
        );
        let lo = deltas.iter().map(|d| d.0).fold(f64::INFINITY, f64::min);
        let hi = deltas.iter().map(|d| d.0).fold(0.0_f64, f64::max);
        // A stair-step at texel boundaries would show as a periodic swing in
        // this series; a flat min/max band is the absence of one.
        println!("    per-step mean|d| range {lo:.4}..{hi:.4}");
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root")
}

fn write_png(path: &std::path::Path, rgba: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}
