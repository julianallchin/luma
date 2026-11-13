//! Light of one wavelength as an eye sees it: the CIE 1931 observer, and the
//! way from its XYZ to linear sRGB.
//!
//! A laser diode and a diffraction pattern are both light of known
//! wavelengths, so both come through here.

/// Lumens in a watt of light at the observer's peak, 555 nm.
pub const PEAK_LUMENS_PER_WATT: f64 = 683.0;

/// XYZ to linear sRGB (D65), row-major.
pub const XYZ_TO_SRGB: [[f64; 3]; 3] = [
    [3.2406, -1.5372, -0.4986],
    [-0.9689, 1.8758, 0.0415],
    [0.0557, -0.2040, 1.0570],
];

/// Linear sRGB (D65) to XYZ, row-major.
pub const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.4124, 0.3576, 0.1805],
    [0.2126, 0.7152, 0.0722],
    [0.0193, 0.1192, 0.9505],
];

/// The CIE 1931 colour-matching functions at `nm` nanometres: the multi-lobe
/// fit of Wyman, Sloan and Shirley (2013). Y is the eye's sensitivity, 1 at
/// its peak, so a watt at `nm` is `PEAK_LUMENS_PER_WATT × Y` lumens.
#[must_use]
pub fn observer(nm: f64) -> [f64; 3] {
    let lobe = |mu: f64, below: f64, above: f64| {
        let t = (nm - mu) / if nm < mu { below } else { above };
        (-0.5 * t * t).exp()
    };
    [
        1.056 * lobe(599.8, 37.9, 31.0) + 0.362 * lobe(442.0, 16.0, 26.7)
            - 0.065 * lobe(501.1, 20.4, 26.2),
        0.821 * lobe(568.8, 46.9, 40.5) + 0.286 * lobe(530.9, 16.3, 31.1),
        1.217 * lobe(437.0, 11.8, 36.0) + 0.681 * lobe(459.0, 26.0, 13.8),
    ]
}

/// `m × v`.
#[must_use]
pub fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_is_the_brightest_watt_and_blue_the_dimmest() {
        let [red, green, blue] = [638.0, 520.0, 450.0].map(|nm| observer(nm)[1]);
        assert!(green > red && red > blue, "{green} {red} {blue}");
        // The eye's peak is the unit.
        assert!((observer(555.0)[1] - 1.0).abs() < 0.02);
    }

    #[test]
    fn the_two_matrices_undo_each_other() {
        let colour = [0.2, 0.5, 0.9];
        let back = apply(XYZ_TO_SRGB, apply(SRGB_TO_XYZ, colour));
        assert!(colour.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-3));
    }
}
