//! Pixel-diff arithmetic shared by the pixel suites.
//!
//! One definition, so two suites cannot disagree about what "these frames
//! differ" means — five per-file copies had already split into two silently
//! different per-channel thresholds (`>= 3` vs `> 8`), and an assertion tuned
//! against one was wrong when read against the other.

/// Per-channel delta below which a pixel is noise, not change: antialiasing,
/// compositor rounding, GPU dithering. The one documented threshold; a caller
/// that genuinely needs a different sensitivity passes its own and says why.
pub const CHANNEL_NOISE: u8 = 3;

/// Fraction of pixels whose RGB differs by at least `threshold` on any
/// channel. Alpha is ignored — screenshots are opaque.
pub fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage, threshold: u8) -> f32 {
    assert_eq!(
        left.dimensions(),
        right.dimensions(),
        "frames must be the same size to be differenced"
    );
    let changed = left
        .pixels()
        .zip(right.pixels())
        .filter(|(left, right)| (0..3).any(|c| left[c].abs_diff(right[c]) >= threshold))
        .count();
    changed as f32 / (left.width() * left.height()) as f32
}

/// Copy a screenshot out of the harness's own directory — which it deletes —
/// into `<temp>/luma-shots/<drawer>/<name>.png`, and decode it.
///
/// Two things at once, because they are always wanted together: a failing
/// assertion has to name a path that still exists when a person goes to look,
/// and the same assertion has to compare the pixels. `drawer` keeps one
/// suite's shots from overwriting another's when both like the same name, and
/// `LUMA_SHOTS` moves the whole tree — one knob for every suite that keeps a
/// shot this way.
pub fn keep_in(
    drawer: &str,
    shot: &serde_json::Value,
    name: &str,
) -> (std::path::PathBuf, image::RgbaImage) {
    let source = shot["path"].as_str().expect("a shot has a path");
    let root = std::env::var("LUMA_SHOTS").map_or_else(
        |_| std::env::temp_dir().join("luma-shots"),
        std::path::PathBuf::from,
    );
    let directory = root.join(drawer);
    std::fs::create_dir_all(&directory).expect("failed to create the shot directory");
    let kept = directory.join(format!("{name}.png"));
    std::fs::copy(source, &kept).expect("failed to keep the shot");
    let decoded = image::open(&kept)
        .expect("the harness wrote a shot that is not an image")
        .to_rgba8();
    (kept, decoded)
}
