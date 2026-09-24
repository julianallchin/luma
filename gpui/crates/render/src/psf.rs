//! Point-spread functions for glare, computed once from an aperture.
//!
//! A point light seen through an aperture spreads into the aperture's
//! Fraunhofer pattern: the squared magnitude of its Fourier transform. The
//! pattern scales with wavelength, which is what colours its fringes. Two
//! apertures are baked here, each into a 256² RGBA half-float texture
//! (`post.rs` draws it as a sprite at every visible lens):
//!
//! - **Aperture**: a camera iris, nine slightly rounded blades. Its edges
//!   make eighteen thin rays; the rounding keeps them faint.
//! - **Eye**: a round pupil with the lens's ciliary fibres and particles in
//!   it, after Ritschel et al., "Temporal Glare" (2009), without the time
//!   variation. The fibres make a corona of many fine radial streaks.
//!
//! The central Airy core is faded out and the rest lifted by its radius (see
//! `bake`): the lens disc and the bloom already draw the core. What is
//! left is normalised as a fraction of the whole pattern's energy, so the
//! glare a lens throws is physical relative to the lens's own light.

use std::f32::consts::PI;

/// Texture side, texels. Also the FFT size.
pub(crate) const SIZE: usize = 256;
/// Pupil radius in the FFT grid, texels. A larger pupil makes a smaller core
/// and a finer pattern.
const PUPIL: f32 = 56.0;
/// Wavelength bands for red, green and blue, nanometres. Each channel
/// averages the pattern over its band: a single wavelength keeps the Airy
/// rings crisp, which no broadband light shows, and the average turns them
/// into a smooth falloff while the rays, which do not scale away, remain.
const BANDS: [(f32, f32); 3] = [(575.0, 680.0), (495.0, 600.0), (420.0, 520.0)];
/// Samples per band.
const BAND_SAMPLES: usize = 12;
/// The wavelength the FFT grid is computed at, nanometres.
const REFERENCE: f32 = 550.0;
/// Radius of the flattened core, texels.
const CORE: f32 = 8.0;

/// Which aperture a pattern comes from.
#[derive(Clone, Copy)]
pub(crate) enum Aperture {
    /// Nine rounded iris blades.
    Iris,
    /// A pupil with ciliary fibres and lens particles.
    Eye,
}

/// Bake `aperture`'s pattern: `SIZE`² RGBA texels, row-major, as f16 bits.
pub(crate) fn bake(aperture: Aperture) -> Vec<half::f16> {
    let mask = match aperture {
        Aperture::Iris => iris(),
        Aperture::Eye => eye(),
    };
    let power = power_spectrum(&mask);
    let centre = SIZE as f32 / 2.0;
    let mut out = vec![half::f16::ZERO; SIZE * SIZE * 4];
    // Each channel's whole pattern, core included, is the source's energy.
    let total: f64 = power.iter().map(|p| f64::from(*p)).sum();
    let sums = [total; 3];
    let mut texels = vec![[0.0f32; 3]; SIZE * SIZE];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = glam::Vec2::new(x as f32 + 0.5 - centre, y as f32 + 0.5 - centre);
            let r = d.length();
            // Take the core out smoothly, over twice its radius.
            // Inside the core the pattern takes its value at the core's
            // edge along the same direction, faded to nothing at the centre:
            // the lens disc and the bloom already draw the core, and a hole
            // cut instead would leave a rim that reads as a ring. Outside,
            // the pattern is lifted by r / CORE: this is a look, not optics.
            // A pattern falling as 1/r³ has to be made so bright to show its
            // rays that its near field becomes a second, larger lens.
            let shape = if r < CORE {
                (r / CORE).powi(2)
            } else {
                r / CORE
            };
            let d = d * (CORE / r.max(1e-3)).max(1.0);
            // The fade to the texture's edge keeps the sprite's square border
            // from showing.
            let edge = 1.0 - smoothstep(0.4 * centre, centre, r);
            for (c, (lo, hi)) in BANDS.iter().enumerate() {
                // A longer wavelength spreads the same pattern wider: its
                // value here is the reference pattern nearer the centre,
                // with the scale's square keeping each sample's energy.
                let mut p = 0.0;
                for i in 0..BAND_SAMPLES {
                    let lambda = lo + (hi - lo) * (i as f32 + 0.5) / BAND_SAMPLES as f32;
                    let scale = REFERENCE / lambda;
                    p += sample(&power, d * scale + centre) * scale * scale;
                }
                p /= BAND_SAMPLES as f32;
                texels[y * SIZE + x][c] = p * shape * edge;
            }
        }
    }
    // Normalise each channel by its whole pattern (core included) and to a
    // unit texel area, so the texture is energy per texel as a fraction of
    // the source's.
    for (i, texel) in texels.iter().enumerate() {
        for c in 0..3 {
            out[i * 4 + c] = half::f16::from_f32((f64::from(texel[c]) / sums[c].max(1e-30)) as f32);
        }
        out[i * 4 + 3] = half::f16::ONE;
    }
    out
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Bilinear read of a `SIZE`² field at texel coordinates; zero outside.
fn sample(field: &[f32], at: glam::Vec2) -> f32 {
    let p = at - 0.5;
    let (x0, y0) = (p.x.floor(), p.y.floor());
    let (fx, fy) = (p.x - x0, p.y - y0);
    let get = |x: f32, y: f32| {
        if x < 0.0 || y < 0.0 || x >= SIZE as f32 || y >= SIZE as f32 {
            0.0
        } else {
            field[y as usize * SIZE + x as usize]
        }
    };
    let top = get(x0, y0) * (1.0 - fx) + get(x0 + 1.0, y0) * fx;
    let bottom = get(x0, y0 + 1.0) * (1.0 - fx) + get(x0 + 1.0, y0 + 1.0) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Deterministic noise for the eye's fibres and particles: the pattern is
/// part of the look, and must not change between launches.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
}

/// Transmission of a pupil-sized area at each grid texel, supersampled 4×4
/// so its edge is antialiased.
fn rasterize(inside: impl Fn(glam::Vec2) -> f32) -> Vec<f32> {
    let centre = SIZE as f32 / 2.0;
    let mut mask = vec![0.0; SIZE * SIZE];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - centre;
            let dy = y as f32 + 0.5 - centre;
            if dx.abs() > PUPIL + 2.0 || dy.abs() > PUPIL + 2.0 {
                continue;
            }
            let mut t = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let p = glam::Vec2::new(
                        dx - 0.5 + (sx as f32 + 0.5) / 4.0,
                        dy - 0.5 + (sy as f32 + 0.5) / 4.0,
                    );
                    t += inside(p);
                }
            }
            mask[y * SIZE + x] = t / 16.0;
        }
    }
    mask
}

/// Nine blades, their straight edges blended a quarter of the way toward a
/// circle.
fn iris() -> Vec<f32> {
    const BLADES: f32 = 9.0;
    rasterize(|p| {
        let r = p.length();
        let angle = p.y.atan2(p.x) + 0.2;
        let sector = 2.0 * PI / BLADES;
        let local = (angle.rem_euclid(sector)) - sector / 2.0;
        let polygon = PUPIL * (sector / 2.0).cos() / local.cos();
        let edge = polygon + 0.25 * (PUPIL - polygon);
        f32::from(u8::from(r <= edge))
    })
}

/// A round pupil, crossed by radial ciliary fibres and dotted with lens
/// particles, both partly opaque.
fn eye() -> Vec<f32> {
    let mut rng = Lcg(0x5eed_1a55);
    let fibres: Vec<(f32, f32, f32)> = (0..160)
        .map(|_| {
            (
                rng.next() * 2.0 * PI,
                0.35 + 0.45 * rng.next(),
                0.3 + 0.5 * rng.next(),
            )
        })
        .collect();
    let particles: Vec<(glam::Vec2, f32)> = (0..240)
        .map(|_| {
            let a = rng.next() * 2.0 * PI;
            let r = PUPIL * rng.next().sqrt();
            (
                glam::Vec2::new(a.cos(), a.sin()) * r,
                0.4 + 1.2 * rng.next(),
            )
        })
        .collect();
    rasterize(|p| {
        let r = p.length();
        if r > PUPIL {
            return 0.0;
        }
        let mut t = 1.0f32;
        let angle = p.y.atan2(p.x);
        for &(a, start, opacity) in &fibres {
            // Fibres run in from the rim to `start` of the radius, a
            // quarter of a texel wide.
            if r < start * PUPIL {
                continue;
            }
            let off = (angle - a + PI).rem_euclid(2.0 * PI) - PI;
            if (off * r).abs() < 0.25 {
                t *= 1.0 - opacity;
            }
        }
        for &(c, radius) in &particles {
            if (p - c).length() < radius {
                t *= 0.4;
            }
        }
        t
    })
}

/// |FFT(mask)|², shifted so frequency zero is at the centre.
fn power_spectrum(mask: &[f32]) -> Vec<f32> {
    let mut re: Vec<f32> = mask.to_vec();
    let mut im = vec![0.0f32; SIZE * SIZE];
    // Rows, then columns.
    let mut row_re = vec![0.0; SIZE];
    let mut row_im = vec![0.0; SIZE];
    for y in 0..SIZE {
        row_re.copy_from_slice(&re[y * SIZE..(y + 1) * SIZE]);
        row_im.copy_from_slice(&im[y * SIZE..(y + 1) * SIZE]);
        fft(&mut row_re, &mut row_im);
        re[y * SIZE..(y + 1) * SIZE].copy_from_slice(&row_re);
        im[y * SIZE..(y + 1) * SIZE].copy_from_slice(&row_im);
    }
    for x in 0..SIZE {
        for y in 0..SIZE {
            row_re[y] = re[y * SIZE + x];
            row_im[y] = im[y * SIZE + x];
        }
        fft(&mut row_re, &mut row_im);
        for y in 0..SIZE {
            re[y * SIZE + x] = row_re[y];
            im[y * SIZE + x] = row_im[y];
        }
    }
    let half = SIZE / 2;
    let mut power = vec![0.0; SIZE * SIZE];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let i = y * SIZE + x;
            let shifted = ((y + half) % SIZE) * SIZE + (x + half) % SIZE;
            power[shifted] = re[i] * re[i] + im[i] * im[i];
        }
    }
    power
}

/// In-place iterative radix-2 FFT.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (angle * k as f32).sin_cos();
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sum of `field` (`channel` of `stride`) over radii `[r0, r1)` and
    /// angles `[a0, a1)` around the centre.
    fn region(
        field: &[f32],
        stride: usize,
        channel: usize,
        r0: f32,
        r1: f32,
        a0: f32,
        a1: f32,
    ) -> f32 {
        let c = SIZE as f32 / 2.0;
        let mut sum = 0.0;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let d = glam::Vec2::new(x as f32 + 0.5 - c, y as f32 + 0.5 - c);
                let a = d.y.atan2(d.x).rem_euclid(2.0 * PI);
                if (r0..r1).contains(&d.length()) && (a0..a1).contains(&a) {
                    sum += field[(y * SIZE + x) * stride + channel];
                }
            }
        }
        sum
    }

    /// A disc's pattern is round: its energy does not depend on direction.
    #[test]
    fn a_round_pupil_spreads_evenly() {
        let power = power_spectrum(&rasterize(|p| f32::from(u8::from(p.length() <= PUPIL))));
        let a = region(&power, 1, 0, 10.0, 60.0, 0.1, 0.6);
        let b = region(&power, 1, 0, 10.0, 60.0, 1.1, 1.6);
        assert!((a - b).abs() < 0.1 * a.max(b), "{a} against {b}");
    }

    /// The core is faded out, not a peak.
    #[test]
    fn the_core_is_flattened() {
        let baked: Vec<f32> = bake(Aperture::Iris).iter().map(|v| v.to_f32()).collect();
        let core = region(&baked, 4, 1, 0.0, CORE, 0.0, 2.0 * PI) / (PI * CORE * CORE);
        let rim = region(&baked, 4, 1, CORE, CORE + 2.0, 0.0, 2.0 * PI)
            / (PI * ((CORE + 2.0).powi(2) - CORE * CORE));
        assert!(core < rim, "core {core} against rim {rim}");
    }

    /// Red spreads wider than blue: more of its light lies far out.
    #[test]
    fn longer_wavelengths_spread_wider() {
        let baked: Vec<f32> = bake(Aperture::Iris).iter().map(|v| v.to_f32()).collect();
        let far = |c| region(&baked, 4, c, 30.0, 100.0, 0.0, 2.0 * PI);
        assert!(far(0) > 1.1 * far(2), "red {} blue {}", far(0), far(2));
    }
}
