//! Colour transfer functions, and the re-export of the space bridge.
//!
//! The Z-up/Y-up conversion moved to [`luma_scene::coords`] when the venue
//! resolver — which is GPU-free and cannot depend on this crate — had to speak
//! it too. Every name is re-exported here, so `crate::coords::world_from_data`
//! still resolves.

pub use luma_scene::coords::*;

use glam::Vec3;

/// sRGB transfer to linear, three's `SRGBToLinear`. CSS colour literals in the
/// three.js scene (`#030303`, `#191919`) arrive through this; `setRGB` values
/// are already linear and must not.
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c < 0.04045 {
        c * 0.077_399_38
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The inverse of [`srgb_to_linear`]. Anything that averages, blends or
/// resamples a readback frame has to come back through here: the renderer hands
/// out sRGB-encoded bytes, and arithmetic on those is arithmetic on a gamma
/// curve.
#[must_use]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c < 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// `#rrggbb` in sRGB to a linear working-space colour.
#[must_use]
pub fn hex_srgb(hex: u32) -> Vec3 {
    let ch = |shift: u32| srgb_to_linear(((hex >> shift) & 0xff) as f32 / 255.0);
    Vec3::new(ch(16), ch(8), ch(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trips() {
        for c in [0.0_f32, 0.002, 0.05, 0.5, 1.0] {
            assert!((linear_to_srgb(srgb_to_linear(c)) - c).abs() < 1e-5);
        }
    }

    #[test]
    fn hex_is_channel_ordered() {
        let red = hex_srgb(0xff_00_00);
        assert!(red.x > 0.99 && red.y == 0.0 && red.z == 0.0);
    }
}
