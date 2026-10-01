//! Light colors. The working space is **linear Rec. 2020**, 0–1 per channel:
//! every stored light color, every gradient stop and keyframe, and every
//! color the engine computes. A light's brightness is its peak channel.
//!
//! Three things convert out of it, and each does so here, by one rule:
//!
//! - OKLab, for perceptual blending ([`interpolate`]).
//! - An emitter set: a fixture's LEDs or the display ([`Gamut`]). A color
//!   the emitters cannot make is mapped to the nearest one they can, in
//!   OKLab, by reducing chroma at constant lightness and hue.
//! - sRGB hex, for people ([`from_srgb`], [`to_display_srgb`]).
//!
//! # Where the constants come from
//!
//! The RGB → XYZ matrices are derived below from the primaries' CIE 1931 xy
//! chromaticities and the D65 white point (SMPTE RP 177 derivation):
//! Rec. 709 / sRGB from ITU-R BT.709-6, Rec. 2020 from ITU-R BT.2020-2, D65
//! as (0.3127, 0.3290). The XYZ → LMS matrix and the LMS′ → Lab matrix are
//! Björn Ottosson's published OKLab matrices
//! (<https://bottosson.github.io/posts/oklab/>). The Rec. 2020 → LMS matrix
//! is their product, with each row scaled to sum to one so that D65 white is
//! LMS (1, 1, 1) and has zero chroma exactly — the normalization Ottosson's
//! own published linear-sRGB matrix has. The tests check the product
//! against his published sRGB matrix and the derived sRGB ↔ Rec. 2020
//! matrices against ITU-R BT.2087.
// The published matrix coefficients keep their source precision.
#![allow(clippy::excessive_precision)]

type Mat3 = [[f64; 3]; 3];

const fn mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut out = [[0.; 3]; 3];
    let mut i = 0;
    while i < 3 {
        let mut j = 0;
        while j < 3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
            j += 1;
        }
        i += 1;
    }
    out
}

const fn inverse(m: Mat3) -> Mat3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = m;
    let (x, y, z) = (e * i - f * h, f * g - d * i, d * h - e * g);
    let det = a * x + b * y + c * z;
    [
        [x / det, (c * h - b * i) / det, (b * f - c * e) / det],
        [y / det, (a * i - c * g) / det, (c * d - a * f) / det],
        [z / det, (b * g - a * h) / det, (a * e - b * d) / det],
    ]
}

fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
}

/// CIE 1931 xy of D65.
const D65: [f64; 2] = [0.3127, 0.3290];

/// Linear RGB → XYZ for primaries at `xy`, scaled so RGB (1, 1, 1) is the
/// white point at Y = 1.
const fn rgb_to_xyz(xy: [[f64; 2]; 3], white: [f64; 2]) -> Mat3 {
    const fn column([x, y]: [f64; 2]) -> [f64; 3] {
        [x / y, 1., (1. - x - y) / y]
    }
    let (r, g, b, w) = (column(xy[0]), column(xy[1]), column(xy[2]), column(white));
    let primaries = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let inv = inverse(primaries);
    let s = [
        inv[0][0] * w[0] + inv[0][1] * w[1] + inv[0][2] * w[2],
        inv[1][0] * w[0] + inv[1][1] * w[1] + inv[1][2] * w[2],
        inv[2][0] * w[0] + inv[2][1] * w[1] + inv[2][2] * w[2],
    ];
    [
        [r[0] * s[0], g[0] * s[1], b[0] * s[2]],
        [r[1] * s[0], g[1] * s[1], b[1] * s[2]],
        [r[2] * s[0], g[2] * s[1], b[2] * s[2]],
    ]
}

/// Rec. 709 / sRGB primaries (ITU-R BT.709-6).
pub const SRGB_PRIMARIES: [[f64; 2]; 3] = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]];
/// Rec. 2020 primaries (ITU-R BT.2020-2).
pub const REC2020_PRIMARIES: [[f64; 2]; 3] = [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]];

const REC2020_TO_XYZ: Mat3 = rgb_to_xyz(REC2020_PRIMARIES, D65);
const SRGB_TO_XYZ: Mat3 = rgb_to_xyz(SRGB_PRIMARIES, D65);
const SRGB_TO_REC2020: Mat3 = mul(inverse(REC2020_TO_XYZ), SRGB_TO_XYZ);

/// Ottosson's XYZ (D65) → LMS.
const XYZ_TO_LMS: Mat3 = [
    [0.8189330101, 0.3618667424, -0.1288597137],
    [0.0329845436, 0.9293118715, 0.0361456387],
    [0.0482003018, 0.2643662691, 0.6338517070],
];
const REC2020_TO_LMS: Mat3 = unit_rows(mul(XYZ_TO_LMS, REC2020_TO_XYZ));
const LMS_TO_REC2020: Mat3 = inverse(REC2020_TO_LMS);
/// Ottosson's cube-rooted LMS → Lab. Its inverse is computed, not his
/// published one, which is exact only to about 1e-7 and so would not round
/// trip.
const LMS_TO_LAB: Mat3 = [
    [0.2104542553, 0.7936177850, -0.0040720468],
    [1.9779984951, -2.4285922050, 0.4505937099],
    [0.0259040371, 0.7827717662, -0.8086757660],
];
const LAB_TO_LMS: Mat3 = inverse(LMS_TO_LAB);

const fn unit_rows(m: Mat3) -> Mat3 {
    let mut out = m;
    let mut i = 0;
    while i < 3 {
        let sum = m[i][0] + m[i][1] + m[i][2];
        out[i] = [m[i][0] / sum, m[i][1] / sum, m[i][2] / sum];
        i += 1;
    }
    out
}

/// The sRGB transfer curve: a linear level to its gamma-encoded value.
pub fn srgb_encode(linear: f64) -> f64 {
    if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1. / 2.4) - 0.055
    }
}

/// The inverse of [`srgb_encode`].
pub fn srgb_decode(encoded: f64) -> f64 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Gamma-encoded sRGB (what a hex code or a CSS color says) to linear
/// Rec. 2020. An sRGB color in 0–1 lands in 0–1.
pub fn from_srgb(srgb: [f64; 3]) -> [f64; 3] {
    apply(&SRGB_TO_REC2020, srgb.map(srgb_decode)).map(|v| v.clamp(0., 1.))
}

/// Linear sRGB (sRGB primaries, no transfer curve) to linear Rec. 2020: a
/// change of primaries only. The same light in 0–1 lands in 0–1.
pub fn from_linear_srgb(linear: [f64; 3]) -> [f64; 3] {
    apply(&SRGB_TO_REC2020, linear).map(|v| v.clamp(0., 1.))
}

/// `#rrggbb` (the `#` optional, sRGB) to linear Rec. 2020.
pub fn from_hex(hex: &str) -> Option<[f64; 3]> {
    let digits = hex.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(from_srgb(
        [byte(0)?, byte(2)?, byte(4)?].map(|v| f64::from(v) / 255.),
    ))
}

/// Whether sRGB can show `color` (linear Rec. 2020) exactly.
pub fn in_srgb(color: [f64; 3]) -> bool {
    Gamut::SRGB.contains(color)
}

/// `color` (linear Rec. 2020) as gamma-encoded sRGB 0–1 for a screen,
/// gamut-mapped by [`Gamut::levels`] when sRGB cannot show it.
pub fn to_display_srgb(color: [f64; 3]) -> [f64; 3] {
    Gamut::SRGB.levels(color).map(srgb_encode)
}

/// Linear Rec. 2020 to OKLab `[L, a, b]`.
pub fn to_oklab(color: [f64; 3]) -> [f64; 3] {
    apply(&LMS_TO_LAB, apply(&REC2020_TO_LMS, color).map(f64::cbrt))
}

/// OKLab `[L, a, b]` to linear Rec. 2020, unclamped.
pub fn from_oklab(lab: [f64; 3]) -> [f64; 3] {
    apply(&LMS_TO_REC2020, apply(&LAB_TO_LMS, lab).map(|v| v * v * v))
}

/// Blend two colors (linear Rec. 2020) perceptually, in OKLab. The editor
/// and the engine share it. The ends are the exact colors; a blend is
/// clamped to 0–1.
pub fn interpolate(from: [f64; 3], to: [f64; 3], amount: f64) -> [f64; 3] {
    if amount <= 0. {
        return from;
    }
    if amount >= 1. {
        return to;
    }
    let (a, b) = (to_oklab(from), to_oklab(to));
    from_oklab(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * amount)).map(|v| v.clamp(0., 1.))
}

/// The colors a set of three emitters can make: the matrix from linear
/// Rec. 2020 into their linear levels, each 0–1. The display is one
/// ([`Gamut::SRGB`]); an RGB fixture is another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gamut {
    from_rec2020: Mat3,
}

/// How far outside 0–1 an emitter level may be and still count as makeable:
/// float noise, and a color stored to six places, at the gamut's edge. Far
/// below what anyone can see, even in the dark.
const EDGE: f64 = 1e-6;

impl Gamut {
    /// sRGB primaries. Also the gamut assumed for a fixture's red, green and
    /// blue emitters, since fixture definitions do not say where they are.
    pub const SRGB: Self = Self::from_primaries(SRGB_PRIMARIES);
    /// Rec. 2020 itself: every stored color is inside it.
    pub const REC2020: Self = Self::from_primaries(REC2020_PRIMARIES);

    /// Emitters with CIE 1931 xy chromaticities `xy`, balanced to D65.
    pub const fn from_primaries(xy: [[f64; 2]; 3]) -> Self {
        Self {
            from_rec2020: mul(inverse(rgb_to_xyz(xy, D65)), REC2020_TO_XYZ),
        }
    }

    /// Whether the emitters make `light` (linear Rec. 2020) exactly, with no
    /// level below 0 or above 1.
    pub fn contains(&self, light: [f64; 3]) -> bool {
        apply(&self.from_rec2020, light)
            .iter()
            .all(|v| (-EDGE..=1. + EDGE).contains(v))
    }

    /// The emitter levels, each at least 0, that make `light`'s color: its
    /// chromaticity, at any brightness. A color the emitters cannot make is
    /// moved toward the gray of the same OKLab lightness, keeping its hue,
    /// until they can: the nearest makeable color. Levels may exceed 1 —
    /// how bright the result can be is [`Self::levels`]' and
    /// [`Self::split`]'s question. Scaling `light` scales the result.
    pub fn fit(&self, light: [f64; 3]) -> [f64; 3] {
        let levels = apply(&self.from_rec2020, light);
        if levels.iter().all(|v| *v >= -EDGE) {
            return levels.map(|v| v.max(0.));
        }
        let [l, a, b] = to_oklab(light);
        let at = |chroma: f64| apply(&self.from_rec2020, from_oklab([l, a * chroma, b * chroma]));
        // The gray at `l` is makeable: every gamut here is balanced to D65.
        let (mut inside, mut outside) = (0., 1.);
        for _ in 0..40 {
            let mid = (inside + outside) / 2.;
            if at(mid).iter().all(|v| *v >= 0.) {
                inside = mid;
            } else {
                outside = mid;
            }
        }
        at(inside).map(|v| v.max(0.))
    }

    /// The emitter levels, each 0–1, for `light` (linear Rec. 2020): its
    /// color by [`Self::fit`]; a light brighter than the emitters can make
    /// keeps that color and loses brightness.
    pub fn levels(&self, light: [f64; 3]) -> [f64; 3] {
        let levels = self.fit(light);
        let peak = levels.iter().copied().fold(1., f64::max);
        levels.map(|v| v / peak)
    }

    /// A normalized color and a brightness (a [`crate::FixtureOutput`]'s
    /// color and dimmer) as emitter terms: the emitters' levels normalized to
    /// a peak of 1, and the brightness they then need, at most 1. A dark
    /// light keeps its color. A black color keeps the brightness: the
    /// emitters are dark either way, and a lamp with none still dims.
    pub fn split(&self, color: [f64; 3], dimmer: f64) -> ([f64; 3], f64) {
        let levels = self.fit(color);
        let peak = levels.iter().copied().fold(0., f64::max);
        if peak <= 1e-12 {
            return ([0.; 3], dimmer.clamp(0., 1.));
        }
        (levels.map(|v| v / peak), (dimmer * peak).clamp(0., 1.))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3], tolerance: f64) -> bool {
        a.iter().zip(&b).all(|(a, b)| (a - b).abs() <= tolerance)
    }

    /// A small deterministic spread of colors over the unit cube.
    fn cube(steps: usize) -> Vec<[f64; 3]> {
        let at = |i: usize| i as f64 / (steps - 1) as f64;
        (0..steps.pow(3))
            .map(|n| [at(n % steps), at(n / steps % steps), at(n / steps / steps)])
            .collect()
    }

    #[test]
    fn derived_matrices_match_the_published_ones() {
        // ITU-R BT.2087-0, linear BT.709 to linear BT.2020, to its 4 places.
        let bt2087 = [
            [0.6274, 0.3293, 0.0433],
            [0.0691, 0.9195, 0.0114],
            [0.0164, 0.0880, 0.8956],
        ];
        for (row, published) in SRGB_TO_REC2020.iter().zip(bt2087) {
            assert!(close(*row, published, 1e-4), "{row:?} vs {published:?}");
        }
        // Ottosson's published linear-sRGB → LMS.
        let ottosson = [
            [0.4122214708, 0.5363325363, 0.0514459929],
            [0.2119034982, 0.6806995451, 0.1073969566],
            [0.0883024619, 0.2817188376, 0.6299787005],
        ];
        for (row, published) in mul(REC2020_TO_LMS, SRGB_TO_REC2020).iter().zip(ottosson) {
            assert!(close(*row, published, 1e-4), "{row:?} vs {published:?}");
        }
    }

    #[test]
    fn white_is_white_and_gray() {
        assert!(close(from_srgb([1.; 3]), [1.; 3], 1e-12));
        // Ottosson's published Lab matrix gives white a chroma of 4e-8.
        let [l, a, b] = to_oklab([1.; 3]);
        assert!((l - 1.).abs() < 1e-7 && a.hypot(b) < 1e-7);
    }

    #[test]
    fn srgb_round_trips_through_rec2020() {
        for srgb in cube(9) {
            let color = from_srgb(srgb);
            assert!(color.iter().all(|v| (0. ..=1.).contains(v)));
            assert!(in_srgb(color), "{srgb:?}");
            let back = to_display_srgb(color);
            assert!(close(back, srgb, 1e-9), "{srgb:?} came back as {back:?}");
        }
        assert_eq!(from_hex("#ff8000"), Some(from_srgb([1., 128. / 255., 0.])));
        assert_eq!(from_hex("ff80"), None);
    }

    #[test]
    fn oklab_round_trips() {
        for color in cube(7) {
            assert!(close(from_oklab(to_oklab(color)), color, 1e-9), "{color:?}");
        }
    }

    /// The blend before the working space changed: OKLab between
    /// gamma-encoded sRGB colors, clamped in linear sRGB, with Ottosson's
    /// published sRGB matrices.
    fn old_interpolate(from: [f64; 3], to: [f64; 3], amount: f64) -> [f64; 3] {
        let to_lab = |c: [f64; 3]| {
            let [r, g, b] = c.map(srgb_decode);
            let lms = [
                0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b,
                0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b,
                0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b,
            ]
            .map(f64::cbrt);
            apply(&LMS_TO_LAB, lms)
        };
        let (a, b) = (to_lab(from), to_lab(to));
        let [l, m, s] = apply(
            &LAB_TO_LMS,
            std::array::from_fn(|i| a[i] + (b[i] - a[i]) * amount),
        )
        .map(|v| v * v * v);
        [
            4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
            -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
            -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
        ]
        .map(|v| srgb_encode(v.clamp(0., 1.)))
    }

    /// A gradient of sRGB colors, such as hex codes, blends as the editor
    /// showed it before: OKLab does not depend on the space it is read from.
    /// (Stored gradients were linear light, which the old engine blended as
    /// if gamma-encoded; their midpoints did change.)
    #[test]
    fn oklab_blending_matches_the_old_srgb_blending() {
        let colors = cube(5);
        for (i, from) in colors.iter().enumerate() {
            let to = colors[(i * 37 + 11) % colors.len()];
            for amount in [0.25, 0.5, 0.75] {
                let new = interpolate(from_srgb(*from), from_srgb(to), amount);
                let old = old_interpolate(*from, to, amount);
                // Compare where the old blend did not clamp.
                if !in_srgb(new) {
                    continue;
                }
                let shown = to_display_srgb(new);
                assert!(
                    close(shown, old, 2e-3),
                    "{from:?} → {to:?} at {amount}: {shown:?} vs {old:?}"
                );
            }
        }
    }

    fn hue_and_lightness(color: [f64; 3]) -> (f64, f64, f64) {
        let [l, a, b] = to_oklab(color);
        (l, b.atan2(a), a.hypot(b))
    }

    #[test]
    fn gamut_mapping_keeps_hue_and_lightness_and_stays_in_range() {
        let srgb_in_rec2020 = |levels: [f64; 3]| apply(&inverse(Gamut::SRGB.from_rec2020), levels);
        let mut outside = 0;
        for color in cube(11) {
            let fitted = Gamut::SRGB.fit(color);
            assert!(fitted.iter().all(|v| *v >= 0.), "{color:?} → {fitted:?}");
            let levels = Gamut::SRGB.levels(color);
            assert!(levels.iter().all(|v| (0. ..=1.).contains(v)));
            if in_srgb(color) {
                continue;
            }
            outside += 1;
            let (l0, h0, c0) = hue_and_lightness(color);
            let (l1, h1, c1) = hue_and_lightness(srgb_in_rec2020(fitted));
            assert!((l0 - l1).abs() < 1e-6, "{color:?}: lightness {l0} → {l1}");
            assert!(c1 <= c0 + 1e-9, "{color:?}: chroma grew");
            if c1 > 1e-3 {
                let turn = (h0 - h1 + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
                    - std::f64::consts::PI;
                assert!(turn.abs() < 1e-3, "{color:?}: hue {h0} → {h1}");
            }
        }
        assert!(outside > 100, "the cube must reach outside sRGB");
    }

    #[test]
    fn an_srgb_color_keeps_its_levels_and_brightness() {
        // Full sRGB red, stored as Rec. 2020: the emitters' red at full.
        let red = from_srgb([1., 0., 0.]);
        let peak = red.iter().copied().fold(0., f64::max);
        let (levels, dimmer) = Gamut::SRGB.split(red.map(|v| v / peak), peak);
        assert!(close(levels, [1., 0., 0.], 1e-9) && (dimmer - 1.).abs() < 1e-9);
        // Rec. 2020's own red is deeper than sRGB can show: the nearest red
        // at full, with no green or blue creeping in past the edge.
        let (levels, dimmer) = Gamut::SRGB.split([1., 0., 0.], 1.);
        assert!(levels[0] == 1. && dimmer == 1.);
        assert!(levels[1].min(levels[2]) < 1e-6);
        assert_eq!(Gamut::SRGB.split([0.; 3], 0.5), ([0.; 3], 0.5));
    }
}
