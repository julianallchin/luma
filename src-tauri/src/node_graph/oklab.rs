// Björn Ottosson's OKLab. sRGB values are in [0,1] (gamma-encoded);
// linear and OKLab use the same float scale, no clamping inside conversion.

#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

pub fn srgb_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let lr = srgb_to_linear(r);
    let lg = srgb_to_linear(g);
    let lb = srgb_to_linear(b);

    let l = 0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb;
    let m = 0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb;
    let s = 0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    (
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )
}

pub fn oklab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;

    let l3 = l_ * l_ * l_;
    let m3 = m_ * m_ * m_;
    let s3 = s_ * s_ * s_;

    let lr = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
    let lg = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
    let lb = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3;

    (
        linear_to_srgb(lr.clamp(0.0, 1.0)),
        linear_to_srgb(lg.clamp(0.0, 1.0)),
        linear_to_srgb(lb.clamp(0.0, 1.0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn roundtrip() {
        for &(r, g, b) in &[
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (1.0, 0.0, 0.0),
            (0.2, 0.5, 0.9),
        ] {
            let (l, a, b2) = srgb_to_oklab(r, g, b);
            let (r2, g2, b3) = oklab_to_srgb(l, a, b2);
            assert!(approx(r, r2), "r {} -> {}", r, r2);
            assert!(approx(g, g2), "g {} -> {}", g, g2);
            assert!(approx(b, b3), "b {} -> {}", b, b3);
        }
    }
}
