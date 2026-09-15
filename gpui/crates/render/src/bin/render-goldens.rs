//! Render every golden frame with `luma-render`.
//!
//!     cargo run -p luma-render --release --bin render-goldens
//!     cargo run -p luma-render --release --bin render-goldens -- single-mover
//!     cargo run -p luma-render --release --bin render-goldens -- --check
//!
//! Output lands in `harness/goldens/scenes-wgpu/<scene>-<t>.png`. Every PNG is
//! accompanied by a versioned JSON descriptor containing the complete
//! deterministic input.

use std::path::Path;

use luma_render::{build_frame, Catalogue, Renderer, DEFAULT_SUBFRAMES};

/// What a run does with the frames it renders.
enum Mode {
    /// Render and overwrite the tracked PNG and descriptor.
    Capture,
    /// Render and diff against the tracked PNG, writing nothing. Drift is a
    /// non-zero exit, so CI and a pre-commit sanity check can both use it.
    Check,
}

fn main() -> anyhow::Result<()> {
    let repo = luma_render::repo_root();
    let catalogue = Catalogue::load(&repo.join("gpui/crates/render/goldens/scenes.json"))?;
    let mut mode = Mode::Capture;
    let mut requested: Vec<String> = Vec::new();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--check" => mode = Mode::Check,
            unknown if unknown.starts_with("--") => anyhow::bail!("unknown flag: {unknown}"),
            scene => requested.push(scene.to_owned()),
        }
    }
    let out_dir = repo.join("harness/goldens/scenes-wgpu");
    if matches!(mode, Mode::Capture) {
        std::fs::create_dir_all(&out_dir)?;
    }

    let (width, height) = catalogue.frame_size();
    let mut renderer = Renderer::new()?;
    let mut library = luma_render::assets::Library::new(repo.join("resources/meshes"));

    let mut drifted = 0usize;
    for scene in &catalogue.scenes {
        if !requested.is_empty() && !requested.contains(&scene.id) {
            continue;
        }
        for &t in &scene.times {
            let path = out_dir.join(scene.frame_name(t));
            let frame = build_frame(scene, &catalogue.definitions, t, &mut library)?;
            let pixels = renderer.render(&frame, width, height, DEFAULT_SUBFRAMES)?;
            let geometry = format!(
                "{} draws, {} cones",
                frame.draws.len(),
                frame.fixture_cones.len()
            );
            let stats = match mode {
                Mode::Check => {
                    let verdict = compare_png(&path, &pixels)?;
                    if verdict.drifted() {
                        drifted += 1;
                    }
                    format!("{verdict}  ({geometry})")
                }
                Mode::Capture => {
                    luma_render::image_out::write(&path, &pixels, width, height)?;
                    let descriptor = catalogue.frame_descriptor(scene, t, DEFAULT_SUBFRAMES)?;
                    write_json(&out_dir.join(scene.descriptor_name(t)), &descriptor)?;
                    geometry
                }
            };
            println!("{}  {stats}", path.display());
        }
    }
    anyhow::ensure!(
        drifted == 0,
        "{drifted} golden frame(s) differ from the tracked capture"
    );
    Ok(())
}

/// One frame's verdict under `--check`.
enum Verdict {
    Unchanged,
    /// How many of the frame's pixels differ, and by how much in the worst
    /// channel — a one-count difference is a driver wobble, a large one is a
    /// real change.
    Changed {
        pixels: usize,
        max_delta: u8,
    },
    Missing,
}

impl Verdict {
    fn drifted(&self) -> bool {
        !matches!(self, Verdict::Unchanged)
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Unchanged => write!(f, "unchanged"),
            Verdict::Changed { pixels, max_delta } => {
                write!(f, "CHANGED: {pixels} px differ, max delta {max_delta}")
            }
            Verdict::Missing => write!(f, "MISSING: no tracked capture"),
        }
    }
}

/// Decode the tracked PNG at `path` and diff it against freshly rendered
/// `rgba`. A frame whose size changed counts as fully changed rather than an
/// error: the caller wants a verdict per frame, not an abort.
fn compare_png(path: &Path, rgba: &[u8]) -> anyhow::Result<Verdict> {
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(Verdict::Missing);
    };
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info()?;
    let mut tracked = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut tracked)?;
    tracked.truncate(info.buffer_size());
    anyhow::ensure!(
        info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
        "tracked golden is not 8-bit RGBA: {}",
        path.display()
    );
    if tracked.len() != rgba.len() {
        return Ok(Verdict::Changed {
            pixels: rgba.len() / 4,
            max_delta: u8::MAX,
        });
    }
    let mut pixels = 0usize;
    let mut max_delta = 0u8;
    for (was, now) in tracked.chunks_exact(4).zip(rgba.chunks_exact(4)) {
        let delta = was
            .iter()
            .zip(now)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        if delta > 0 {
            pixels += 1;
            max_delta = max_delta.max(delta);
        }
    }
    Ok(if pixels == 0 {
        Verdict::Unchanged
    } else {
        Verdict::Changed { pixels, max_delta }
    })
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    std::io::Write::write_all(&mut writer, b"\n")?;
    Ok(())
}
