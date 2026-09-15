//! Render every golden frame through the **live** path — [`Viewport`], at
//! [`luma_render::LIVE_SUBFRAMES`] — so the interactive image can be compared
//! against the export image of the same rig.
//!
//!     cargo run -p luma-render --release --bin live-goldens
//!     cargo run -p luma-render --release --bin live-goldens -- mover-fan
//!
//! Output lands in `gpui/target/live-goldens/`, which Git ignores, under the
//! same frame names as `harness/goldens/scenes-wgpu/`, so the two compare frame
//! for frame.
//! The bar this exists to police: a live frame must be *visually family-
//! identical* to its golden — same softness, falloff and pools — not merely
//! within a pixel tolerance.

use luma_render::{build_frame, Catalogue, Viewport};

fn main() -> anyhow::Result<()> {
    let repo = luma_render::repo_root();
    let mut catalogue = Catalogue::load(&repo.join("gpui/crates/render/goldens/scenes.json"))?;
    // The goldens pin `hazeResolution` at 1 because an export has the time. A
    // live frame does not, and a comparison that quietly kept the export's dial
    // would be measuring the wrong path.
    for scene in &mut catalogue.scenes {
        scene.render.haze.resolution = luma_render::LIVE_HAZE_RESOLUTION;
    }
    let mut library = luma_render::assets::Library::new(repo.join("resources/meshes"));
    let out_dir = repo.join("gpui/target/live-goldens");
    std::fs::create_dir_all(&out_dir)?;

    let requested: Vec<String> = std::env::args().skip(1).collect();
    let (width, height) = catalogue.frame_size();
    let mut viewport = Viewport::new()?;

    for scene in &catalogue.scenes {
        if !requested.is_empty() && !requested.contains(&scene.id) {
            continue;
        }
        for &t in &scene.times {
            let frame = build_frame(scene, &catalogue.definitions, t, &mut library)?;
            let started = std::time::Instant::now();
            let shot = viewport.draw(&frame, width, height)?;
            let elapsed = started.elapsed();
            // BGRA is what a compositor wants; a PNG wants RGBA.
            let rgba: Vec<u8> = shot
                .pixels
                .chunks_exact(4)
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect();
            let path = out_dir.join(scene.frame_name(t));
            luma_render::image_out::write(&path, &rgba, width, height)?;
            println!("{}  {:.1} ms", path.display(), elapsed.as_secs_f32() * 1e3);
        }
    }
    Ok(())
}
