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
