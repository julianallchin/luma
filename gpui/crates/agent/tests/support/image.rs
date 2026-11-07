//! Reading the frames a pixel suite keeps. The diff arithmetic and the noise
//! floor are `gpui_agent::shots`', shared with `image.diff` in a script.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// A PNG on disk, decoded.
pub fn decode(path: impl AsRef<Path>) -> image::RgbaImage {
    let path = path.as_ref();
    image::open(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .to_rgba8()
}

/// Copy a screenshot out of the harness's own directory, which it deletes, to
/// `<shots>/<name>.png` and print where: a failing assertion has to name a
/// path that still exists when a person goes to look.
pub fn preserve(source: impl AsRef<Path>, name: &str) -> PathBuf {
    let kept = gpui_agent::shots::keep(source, name)
        .unwrap_or_else(|error| panic!("failed to keep {name}: {error}"));
    println!("kept {}", kept.display());
    kept
}

/// [`preserve`] a shot the script returned, and decode it.
pub fn keep(shot: &serde_json::Value, name: &str) -> (PathBuf, image::RgbaImage) {
    let kept = preserve(shot["path"].as_str().expect("a shot has a path"), name);
    let decoded = decode(&kept);
    (kept, decoded)
}

/// Mean horizontal high-frequency energy over columns `x` and rows `y`,
/// clipped to the image: the mean per-channel step between row neighbours.
/// Blur lowers it; sharp text and edges raise it.
pub fn edge_energy(image: &image::RgbaImage, x: Range<u32>, y: Range<u32>) -> f32 {
    let mut total = 0u64;
    let mut count = 0u64;
    for y in y.start..y.end.min(image.height()) {
        for x in (x.start + 1)..x.end.min(image.width()) {
            let left = image.get_pixel(x - 1, y);
            let here = image.get_pixel(x, y);
            total += (0..3)
                .map(|channel| u64::from(left[channel].abs_diff(here[channel])))
                .sum::<u64>();
            count += 3;
        }
    }
    total as f32 / count.max(1) as f32
}

/// [`edge_energy`] of the whole image but its outer eight pixels, where crop
/// and background antialiasing contribute edges of their own.
pub fn inner_edge_energy(image: &image::RgbaImage) -> f32 {
    let (width, height) = image.dimensions();
    edge_energy(
        image,
        8..width.saturating_sub(8),
        8..height.saturating_sub(8),
    )
}
