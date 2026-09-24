//! Glare point-spread functions: the aperture patterns baked once at startup,
//! and the whole-frame kernel `post.rs` convolves the frame's hot pixels with.
//!
//! **Aperture patterns.** Light through an aperture spreads into the
//! aperture's Fraunhofer pattern, the squared magnitude of the Fourier
//! transform of its pupil function `A · exp(iφ)`. A perfect aperture gives a
//! perfectly symmetric pattern, which reads as computer graphics. Real
//! streaks and haze come mostly from dust and scratches on the lens: Wu et
//! al., "How to Train Neural Networks for Flare Removal" (ICCV 2021), model a
//! lens as a clean aperture with about N(30, 5²) dust dots of varied radius
//! and N(30, 5²) scratch polylines of U(1, 16) segments, each of opacity
//! U(0, 1), plus a small defocus phase `W(r) ∝ r²`. Dust gives haze and
//! rainbow fringes; each scratch gives an irregular streak across its own
//! direction. Three apertures are baked here, each with the same seeded
//! defects every launch:
//!
//! - **Iris**: nine slightly rounded blades and the lens's defects.
//! - **Star**: six straight blades (a hexagon) and the same defects. Straight
//!   edges make six strong rays: a camera stopped down.
//! - **Eye**: a round pupil with the lens's ciliary fibres and particles,
//!   after Ritschel et al., "Temporal Glare" (2009), without the time
//!   variation, and a lighter amount of dust and scratches.
//!
//! Each is diffracted at 32 wavelengths from 395 to 705 nm: eight FFTs, one
//! per 40 nm band with that band's defocus phase, each resampled at four
//! wavelengths (a pattern scales with its wavelength). The wavelengths are
//! weighted into linear sRGB by the CIE 1931 observer, with negative lobes
//! clipped as a sensor's response has none.
//!
//! **Kernel.** The glare kernel for a frame is the veil plus, for every style
//! but bloom, its aperture's pattern. The veil is the human eye's glare spread
//! function (Vos 1984, as used by Spencer et al. 1995 and Yoshida et al.
//! 2008), with θ in degrees:
//!
//! ```text
//! PSF(θ) = 0.384·2.61e6·exp(−(θ/0.02)²) + 0.478·20.91/(θ+0.02)³ + 0.138·72.37/(θ+0.02)²
//! ```
//!
//! Its 1/θ² term reaches across the whole frame. Bloom is this veil alone and
//! the eye is this veil with the eye's pattern. A camera's veiling glare has
//! the same wide, additive shape (Qian et al., CVPR 2026) but is weaker, so
//! the iris and the star take a tenth of it, and their lens's dust and
//! scratches show. Both parts are energy per unit solid angle normalised to
//! the source's energy, and are converted into kernel texels by the
//! camera's field of view. The kernel's centre texel is the source's own
//! texel, which the frame already draws; it takes its neighbours' mean, so
//! the glare neither doubles the source nor dips under it. What is left sums
//! to the physical fraction of the source's light that lands outside its own
//! texel. There is no other shaping: a steep fade or a cut would draw a
//! bright ring (Yoshida et al. 2008). The kernel fades to zero only near the
//! grid's padding, far out on the tail.

use std::f32::consts::PI;

use glam::Vec2;

use crate::scene_desc::GlareStyle;

/// Side of the FFT grid an aperture is diffracted on, texels.
const BAKE: usize = 512;
/// Side of a stored pattern: the bake, tent-filtered by two.
pub(crate) const SIZE: usize = BAKE / 2;
/// Pupil radius in bake texels: nearly the whole grid, so the Airy core is
/// as small as the grid can draw it, about one stored texel. A real lens's
/// core is far smaller than a glare texel; one larger than that would add a
/// second, blurred lens around every source. The DFT samples |F|² coarsely
/// at this size, but its energy is exact (Parseval), and the spectral
/// average smooths what the sampling misses.
const PUPIL: f32 = BAKE as f32 / 2.0 - 16.0;
/// Angle of one stored texel at the reference wavelength, degrees. This
/// sets the size of the lens's defects against its aperture: a scratch one
/// texel wide spreads over the whole pattern, ±25°, as a real hairline
/// scratch spreads over a frame.
pub(crate) const TEXEL_DEG: f32 = 0.2;
/// The wavelength the FFT grid's angles are at, nanometres: the shortest
/// sampled, so every other wavelength's pattern, spread wider, is read from
/// inside the grid rather than past its edge.
const REFERENCE_NM: f32 = 395.0;
/// Wavelength bands: each is one FFT, resampled at `SUB` wavelengths.
const BANDS: usize = 8;
const SUB: usize = 4;
const FIRST_NM: f32 = 390.0;
const LAST_NM: f32 = 710.0;
/// Defocus path difference at the pupil's rim, in waves at 550 nm. Small: it breaks the Airy rings' symmetry without moving
/// much light out of the core (a Strehl ratio of about 0.9).
const DEFOCUS_WAVES: f32 = 0.15;

/// Which aperture a pattern comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Aperture {
    /// Nine rounded iris blades with dust and scratches.
    Iris,
    /// Six straight blades with the same dust and scratches.
    Star,
    /// A pupil with ciliary fibres, lens particles, and a little dust.
    Eye,
}

impl Aperture {
    pub(crate) const ALL: [Self; 3] = [Self::Iris, Self::Star, Self::Eye];

    fn index(self) -> usize {
        self as usize
    }
}

/// A baked pattern: `SIZE`² texels, row-major, each the fraction of the
/// source's light (per channel) that lands in that texel, without the
/// unresolved core (see [`power_spectrum`]). The centre is at
/// texel `SIZE / 2`.
pub(crate) struct Pattern {
    pub texels: Vec<[f32; 3]>,
}

/// The three patterns, in [`Aperture::ALL`] order.
pub(crate) struct Patterns([Pattern; 3]);

impl Patterns {
    /// Bake every aperture, in parallel.
    pub(crate) fn bake() -> Self {
        Self(std::thread::scope(|scope| {
            Aperture::ALL
                .map(|aperture| scope.spawn(move || bake(aperture)))
                .map(|handle| handle.join().expect("pattern bake"))
        }))
    }

    fn get(&self, aperture: Aperture) -> &Pattern {
        &self.0[aperture.index()]
    }
}

/// Bake one aperture's pattern.
pub(crate) fn bake(aperture: Aperture) -> Pattern {
    let mask = mask(aperture);
    let bands: Vec<(f32, Vec<f32>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..BANDS)
            .map(|band| {
                let mask = &mask;
                scope.spawn(move || {
                    let width = (LAST_NM - FIRST_NM) / BANDS as f32;
                    let centre = FIRST_NM + width * (band as f32 + 0.5);
                    (centre, power_spectrum(mask, centre))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("band"))
            .collect()
    });
    let weights = spectral_weights();
    // Each wavelength's pattern at bake resolution, in angle: a longer
    // wavelength spreads the same spectrum wider, so its value at an angle is
    // its band's spectrum nearer the centre, the scale's square keeping each
    // wavelength's energy.
    let centre = (BAKE / 2) as f32;
    let mut fine = vec![[0.0f32; 3]; BAKE * BAKE];
    let rows_per = BAKE.div_ceil(threads());
    std::thread::scope(|scope| {
        for (chunk, rows) in fine.chunks_mut(rows_per * BAKE).enumerate() {
            let (bands, weights) = (&bands, &weights);
            scope.spawn(move || {
                for (i, texel) in rows.iter_mut().enumerate() {
                    let index = chunk * rows_per * BAKE + i;
                    let d = Vec2::new(
                        (index % BAKE) as f32 - centre,
                        (index / BAKE) as f32 - centre,
                    );
                    for (band, (_, power)) in bands.iter().enumerate() {
                        for sub in 0..SUB {
                            let k = band * SUB + sub;
                            let scale = REFERENCE_NM / wavelength(k);
                            let p = sample(power, BAKE, d * scale + centre) * scale * scale;
                            for c in 0..3 {
                                texel[c] += weights[k][c] * p;
                            }
                        }
                    }
                }
            });
        }
    });
    // Tent-filter by two: stored texel X is bake texel 2X with half of
    // each neighbour, which keeps the centre on a texel and every bake
    // texel's energy.
    let mut texels = vec![[0.0f32; 3]; SIZE * SIZE];
    let tent = [0.5f32, 1.0, 0.5];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut sum = [0.0f32; 3];
            for (j, wy) in tent.iter().enumerate() {
                for (i, wx) in tent.iter().enumerate() {
                    let (bx, by) = ((2 * x + i) as isize - 1, (2 * y + j) as isize - 1);
                    if bx < 0 || by < 0 || bx >= BAKE as isize || by >= BAKE as isize {
                        continue;
                    }
                    let t = fine[by as usize * BAKE + bx as usize];
                    for c in 0..3 {
                        sum[c] += wx * wy * t[c];
                    }
                }
            }
            // Fade to nothing toward the edge, so the square border of the
            // pattern does not show in the kernel.
            let r = Vec2::new(x as f32, y as f32).distance(Vec2::splat((SIZE / 2) as f32));
            let fade = 1.0 - smoothstep(0.8, 1.0, r / (SIZE / 2) as f32);
            texels[y * SIZE + x] = sum.map(|v| v * fade);
        }
    }
    Pattern { texels }
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

/// The `k`th of the `BANDS × SUB` wavelengths, nanometres.
fn wavelength(k: usize) -> f32 {
    FIRST_NM + (LAST_NM - FIRST_NM) * (k as f32 + 0.5) / (BANDS * SUB) as f32
}

/// Per-wavelength weights into linear sRGB, each channel's summing to one:
/// the CIE 1931 observer (the multi-lobe fit of Wyman, Sloan and Shirley
/// 2013) through the XYZ-to-sRGB matrix, negative lobes clipped.
fn spectral_weights() -> Vec<[f32; 3]> {
    let lobe = |l: f32, mu: f32, below: f32, above: f32| {
        let t = (l - mu) / if l < mu { below } else { above };
        (-0.5 * t * t).exp()
    };
    let mut weights: Vec<[f32; 3]> = (0..BANDS * SUB)
        .map(|k| {
            let l = wavelength(k);
            let x = 1.056 * lobe(l, 599.8, 37.9, 31.0) + 0.362 * lobe(l, 442.0, 16.0, 26.7)
                - 0.065 * lobe(l, 501.1, 20.4, 26.2);
            let y = 0.821 * lobe(l, 568.8, 46.9, 40.5) + 0.286 * lobe(l, 530.9, 16.3, 31.1);
            let z = 1.217 * lobe(l, 437.0, 11.8, 36.0) + 0.681 * lobe(l, 459.0, 26.0, 13.8);
            [
                (3.2406 * x - 1.5372 * y - 0.4986 * z).max(0.0),
                (-0.9689 * x + 1.8758 * y + 0.0415 * z).max(0.0),
                (0.0557 * x - 0.2040 * y + 1.0570 * z).max(0.0),
            ]
        })
        .collect();
    for c in 0..3 {
        let sum: f32 = weights.iter().map(|w| w[c]).sum();
        for w in &mut weights {
            w[c] /= sum;
        }
    }
    weights
}

/// Bilinear read of a `side`² field at texel coordinates (texel centres at
/// integers); zero outside.
fn sample(field: &[f32], side: usize, at: Vec2) -> f32 {
    let (x0, y0) = (at.x.floor(), at.y.floor());
    let (fx, fy) = (at.x - x0, at.y - y0);
    let get = |x: f32, y: f32| {
        if x < 0.0 || y < 0.0 || x >= side as f32 || y >= side as f32 {
            0.0
        } else {
            field[y as usize * side + x as usize]
        }
    };
    let top = get(x0, y0) * (1.0 - fx) + get(x0 + 1.0, y0) * fx;
    let bottom = get(x0, y0 + 1.0) * (1.0 - fx) + get(x0 + 1.0, y0 + 1.0) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Deterministic noise: the defects are part of the look and must not change
/// between launches.
struct Rng(u64);

impl Rng {
    /// Uniform in [0, 1).
    fn next(&mut self) -> f32 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }

    /// Normal, by Box–Muller.
    fn normal(&mut self, mean: f32, sd: f32) -> f32 {
        let u = self.next().max(1e-7);
        let v = self.next();
        mean + sd * (-2.0 * u.ln()).sqrt() * (2.0 * PI * v).cos()
    }

    /// A count drawn from N(mean, sd²), never negative.
    fn count(&mut self, mean: f32, sd: f32) -> usize {
        self.normal(mean, sd).round().max(0.0) as usize
    }

    /// A point uniform in the pupil, bake texels from its centre.
    fn in_pupil(&mut self) -> Vec2 {
        let a = self.next() * 2.0 * PI;
        Vec2::new(a.cos(), a.sin()) * PUPIL * self.next().sqrt()
    }
}

/// A dust dot on the lens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Dot {
    pub centre: Vec2,
    pub radius: f32,
    pub opacity: f32,
}

/// A scratch: a polyline, its half-width and opacity.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scratch {
    pub points: Vec<Vec2>,
    pub half_width: f32,
    pub opacity: f32,
}

/// Dust and scratches, in bake texels from the pupil's centre.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Defects {
    pub dust: Vec<Dot>,
    pub scratches: Vec<Scratch>,
}

/// Wu et al.'s distributions, scaled to the pupil: dust radius 0.4 %–2.5 %
/// of the pupil's radius, log-uniform; scratch segments 3 %–12 % of it,
/// turning a little at each joint, 0.2 %–0.6 % wide; opacity U(0, 1) for
/// both. `amount` scales the
/// mean counts (1 for a camera lens).
pub(crate) fn defects(seed: u64, amount: f32) -> Defects {
    let mut rng = Rng(seed);
    let dust = (0..rng.count(30.0 * amount, 5.0 * amount))
        .map(|_| Dot {
            centre: rng.in_pupil(),
            radius: (rng.range(1.0f32.ln(), 6.0f32.ln())).exp(),
            opacity: rng.next(),
        })
        .collect();
    let scratches = (0..rng.count(30.0 * amount, 5.0 * amount))
        .map(|_| {
            let mut at = rng.in_pupil();
            let mut heading = rng.next() * 2.0 * PI;
            let segments = 1 + (rng.next() * 16.0) as usize;
            let mut points = vec![at];
            for _ in 0..segments.min(16) {
                heading += rng.normal(0.0, 0.25);
                at += Vec2::new(heading.cos(), heading.sin()) * rng.range(8.0, 30.0);
                points.push(at);
            }
            Scratch {
                points,
                half_width: rng.range(0.25, 0.75),
                opacity: rng.next(),
            }
        })
        .collect();
    Defects { dust, scratches }
}

/// The camera lens's defects, shared by the iris and the star.
const LENS_SEED: u64 = 0x1e25_d057;
/// The eye's: a lighter amount.
const EYE_SEED: u64 = 0x5eed_1a55;
const EYE_DEFECTS: f32 = 0.25;

/// Transmission of the aperture at each bake texel.
pub(crate) fn mask(aperture: Aperture) -> Vec<f32> {
    let (mut mask, defects) = match aperture {
        Aperture::Iris => (blades(9.0, 0.25), defects(LENS_SEED, 1.0)),
        Aperture::Star => (blades(6.0, 0.0), defects(LENS_SEED, 1.0)),
        Aperture::Eye => (eye(), defects(EYE_SEED, EYE_DEFECTS)),
    };
    let mut stamp = Stamp::default();
    for dot in &defects.dust {
        stamp.disc(dot.centre, dot.radius);
        stamp.apply(&mut mask, dot.opacity);
    }
    for scratch in &defects.scratches {
        for pair in scratch.points.windows(2) {
            stamp.segment(pair[0], pair[1], scratch.half_width);
        }
        stamp.apply(&mut mask, scratch.opacity);
    }
    mask
}

/// One defect's coverage of the bake grid, sparse. Each texel keeps its
/// largest coverage, so a polyline's joints are not darkened twice.
#[derive(Default)]
struct Stamp {
    coverage: std::collections::BTreeMap<usize, f32>,
}

impl Stamp {
    fn touch(&mut self, x: isize, y: isize, coverage: f32) {
        if coverage <= 0.0 || x < 0 || y < 0 || x >= BAKE as isize || y >= BAKE as isize {
            return;
        }
        let entry = self
            .coverage
            .entry(y as usize * BAKE + x as usize)
            .or_insert(0.0);
        *entry = entry.max(coverage.min(1.0));
    }

    /// Texels around `lo..hi` (pupil coordinates), with their centres.
    fn texels(lo: Vec2, hi: Vec2) -> impl Iterator<Item = (isize, isize, Vec2)> {
        let c = (BAKE / 2) as f32;
        let (x0, x1) = ((lo.x + c).floor() as isize, (hi.x + c).ceil() as isize);
        let (y0, y1) = ((lo.y + c).floor() as isize, (hi.y + c).ceil() as isize);
        (y0..=y1).flat_map(move |y| {
            (x0..=x1).map(move |x| (x, y, Vec2::new(x as f32 + 0.5 - c, y as f32 + 0.5 - c)))
        })
    }

    fn disc(&mut self, centre: Vec2, radius: f32) {
        let reach = Vec2::splat(radius + 1.0);
        for (x, y, p) in Self::texels(centre - reach, centre + reach) {
            // A texel's coverage by an edge at this distance, box-filtered.
            self.touch(x, y, (radius + 0.5 - p.distance(centre)).clamp(0.0, 1.0));
        }
    }

    fn segment(&mut self, a: Vec2, b: Vec2, half_width: f32) {
        let reach = Vec2::splat(half_width + 1.0);
        let ab = b - a;
        for (x, y, p) in Self::texels(a.min(b) - reach, a.max(b) + reach) {
            let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
            let d = p.distance(a + ab * t);
            // The line's cross-section against the texel's, both boxes.
            let coverage = (d + half_width).min(0.5) - (d - half_width).max(-0.5);
            self.touch(x, y, coverage.clamp(0.0, 1.0));
        }
    }

    /// Darken `mask` by this defect at `opacity`, and clear the stamp.
    fn apply(&mut self, mask: &mut [f32], opacity: f32) {
        for (index, coverage) in std::mem::take(&mut self.coverage) {
            mask[index] *= 1.0 - opacity * coverage;
        }
    }
}

/// Transmission of a pupil-sized area at each bake texel, supersampled 4×4
/// so its edge is antialiased.
fn rasterize(inside: impl Fn(Vec2) -> f32) -> Vec<f32> {
    let centre = (BAKE / 2) as f32;
    let mut mask = vec![0.0; BAKE * BAKE];
    for y in 0..BAKE {
        for x in 0..BAKE {
            let dx = x as f32 + 0.5 - centre;
            let dy = y as f32 + 0.5 - centre;
            if dx.abs() > PUPIL + 2.0 || dy.abs() > PUPIL + 2.0 {
                continue;
            }
            let mut t = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let p = Vec2::new(
                        dx - 0.5 + (sx as f32 + 0.5) / 4.0,
                        dy - 0.5 + (sy as f32 + 0.5) / 4.0,
                    );
                    t += inside(p);
                }
            }
            mask[y * BAKE + x] = t / 16.0;
        }
    }
    mask
}

/// `count` blades, their straight edges blended `rounding` of the way toward
/// a circle.
fn blades(count: f32, rounding: f32) -> Vec<f32> {
    rasterize(|p| {
        let r = p.length();
        let angle = p.y.atan2(p.x) + 0.2;
        let sector = 2.0 * PI / count;
        let local = (angle.rem_euclid(sector)) - sector / 2.0;
        let polygon = PUPIL * (sector / 2.0).cos() / local.cos();
        let edge = polygon + rounding * (PUPIL - polygon);
        f32::from(u8::from(r <= edge))
    })
}

/// A round pupil, crossed by radial ciliary fibres and dotted with lens
/// particles, both partly opaque.
fn eye() -> Vec<f32> {
    let mut rng = Rng(EYE_SEED ^ 0xf1b2e);
    let mut mask = rasterize(|p| f32::from(u8::from(p.length() <= PUPIL)));
    let mut stamp = Stamp::default();
    for _ in 0..160 {
        let a = rng.next() * 2.0 * PI;
        let start = 0.35 + 0.45 * rng.next();
        let opacity = 0.3 + 0.5 * rng.next();
        // Fibres run in from the rim to `start` of the radius, about a
        // hundredth of the pupil wide.
        let dir = Vec2::new(a.cos(), a.sin());
        stamp.segment(dir * start * PUPIL, dir * PUPIL, 1.0);
        stamp.apply(&mut mask, opacity);
    }
    for _ in 0..240 {
        let centre = rng.in_pupil();
        stamp.disc(centre, rng.range(1.7, 6.8));
        stamp.apply(&mut mask, 0.6);
    }
    mask
}

/// |FFT(mask · exp(iφ))|² at `wavelength_nm`, normalised to unit sum and
/// shifted so frequency zero is at texel `BAKE / 2`. φ is the defocus phase.
///
/// The 3 × 3 samples around frequency zero are then cleared: the Airy core,
/// out to about its first dark ring. A real lens's core is a thousandth of a
/// degree, far inside the source's own pixel, where the frame already draws
/// it; here the grid would make it a tenth of a degree, and resampled and
/// filtered it would become a second, blurred lens around every source. Its
/// light stays with the source: the pattern keeps only what the aperture,
/// its blades and its defects send outside it.
fn power_spectrum(mask: &[f32], wavelength_nm: f32) -> Vec<f32> {
    let centre = (BAKE / 2) as f32;
    let waves = DEFOCUS_WAVES * 550.0 / wavelength_nm;
    let mut re = vec![0.0f32; BAKE * BAKE];
    let mut im = vec![0.0f32; BAKE * BAKE];
    for (i, &t) in mask.iter().enumerate() {
        if t == 0.0 {
            continue;
        }
        let dx = (i % BAKE) as f32 + 0.5 - centre;
        let dy = (i / BAKE) as f32 + 0.5 - centre;
        let phase = 2.0 * PI * waves * (dx * dx + dy * dy) / (PUPIL * PUPIL);
        let (s, c) = phase.sin_cos();
        re[i] = t * c;
        im[i] = t * s;
    }
    let twiddles = Twiddles::new(BAKE);
    fft_2d(&mut re, &mut im, BAKE, &twiddles);
    let half = BAKE / 2;
    let mut power = vec![0.0; BAKE * BAKE];
    let mut total = 0.0f64;
    for y in 0..BAKE {
        for x in 0..BAKE {
            let i = y * BAKE + x;
            let p = re[i] * re[i] + im[i] * im[i];
            total += f64::from(p);
            power[((y + half) % BAKE) * BAKE + (x + half) % BAKE] = p;
        }
    }
    let scale = (1.0 / total.max(1e-30)) as f32;
    for p in &mut power {
        *p *= scale;
    }
    for y in half - 1..=half + 1 {
        for x in half - 1..=half + 1 {
            power[y * BAKE + x] = 0.0;
        }
    }
    power
}

/// Forward FFT twiddle factors for one size.
struct Twiddles {
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Twiddles {
    fn new(n: usize) -> Self {
        let (sin, cos) = (0..n / 2)
            .map(|k| (-2.0 * std::f64::consts::PI * k as f64 / n as f64).sin_cos())
            .map(|(s, c)| (s as f32, c as f32))
            .unzip();
        Self { cos, sin }
    }
}

/// In-place 2D FFT of a `n`² field: rows, then columns through a transpose.
fn fft_2d(re: &mut [f32], im: &mut [f32], n: usize, twiddles: &Twiddles) {
    for _ in 0..2 {
        for (row_re, row_im) in re.chunks_mut(n).zip(im.chunks_mut(n)) {
            fft(row_re, row_im, twiddles);
        }
        transpose(re, n);
        transpose(im, n);
    }
}

fn transpose(field: &mut [f32], n: usize) {
    for y in 0..n {
        for x in y + 1..n {
            field.swap(y * n + x, x * n + y);
        }
    }
}

/// In-place iterative radix-2 FFT.
fn fft(re: &mut [f32], im: &mut [f32], twiddles: &Twiddles) {
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
        let stride = n / len;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (c, s) = (twiddles.cos[k * stride], twiddles.sin[k * stride]);
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

// --- the veil -------------------------------------------------------------

/// Vos's glare spread function, per square degree, unnormalised.
fn vos(theta_deg: f32) -> f32 {
    let t = theta_deg + 0.02;
    0.384 * 2.61e6 * (-(theta_deg / 0.02).powi(2)).exp()
        + 0.478 * 20.91 / (t * t * t)
        + 0.138 * 72.37 / (t * t)
}

/// Widest angle the veil is normalised over, degrees: the range the CIE
/// disability-glare formula is fitted to.
const VOS_EXTENT_DEG: f64 = 30.0;

/// The integral of [`vos`] over the disc out to [`VOS_EXTENT_DEG`], in
/// closed form: `∫ PSF(θ) 2πθ dθ`.
fn vos_total() -> f64 {
    let a = 0.02f64;
    let t = VOS_EXTENT_DEG;
    let core = 0.384 * 2.61e6 * std::f64::consts::PI * a * a;
    let cubic = 1.0 / (2.0 * a) - 1.0 / (t + a) + a / (2.0 * (t + a).powi(2));
    let square = ((t + a) / a).ln() + a / (t + a) - 1.0;
    core + 2.0 * std::f64::consts::PI * (0.478 * 20.91 * cubic + 0.138 * 72.37 * square)
}

// --- the kernel -----------------------------------------------------------

/// The FFT grid a frame's glare is convolved on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grid {
    /// Texels across and down, powers of two.
    pub width: usize,
    pub height: usize,
    /// Focal length in grid texels: the frame's height in texels over twice
    /// the tangent of half its vertical field of view.
    pub focal: f32,
    /// How far the kernel may reach each way, in texels, before the cyclic
    /// convolution would fold it back onto the frame: the zero padding.
    pub reach: [f32; 2],
}

/// A camera lens's veil against the eye's: veiling glare indices of good
/// lenses are a few percent (ISO 9358), where Vos's veil sends about a
/// quarter of the light more than a glare texel from the source.
const CAMERA_VEIL: f32 = 0.1;

/// Which pattern each style adds to its veil, and the veil's weight.
/// Aperture diameters, millimetres: a 25 mm video lens near f/5, and a
/// pupil in a lit room.
const CAMERA_APERTURE_MM: f32 = 5.0;
const EYE_PUPIL_MM: f32 = 4.0;

/// How much wider the stored pattern is than the aperture's real one.
///
/// A pattern's angles scale as λ/D. The bake's λ/D at 550 nm is
/// `BAKE / (4·PUPIL)` stored texels, about 0.15°; a 5 mm aperture's is
/// about 0.006°. Far from the core the pattern's energy per solid angle
/// falls as 1/θ³, so a pattern stretched `s` times, read at the same angle,
/// holds `s` times the real light there: its weight is divided by `s`.
/// Undivided, every hot pixel threw rays and a halo tens of times too
/// strong, and the sun's rays crossed the whole frame.
fn stretch(aperture: Aperture) -> f32 {
    let stored_deg = BAKE as f32 / (4.0 * PUPIL) * (550.0 / REFERENCE_NM) * TEXEL_DEG;
    let diameter_mm = match aperture {
        Aperture::Iris | Aperture::Star => CAMERA_APERTURE_MM,
        Aperture::Eye => EYE_PUPIL_MM,
    };
    let real_deg = (550e-9 / (diameter_mm * 1e-3)).to_degrees();
    stored_deg / real_deg
}

/// The veil's energy per square degree at angle θ is `vos(θ)` times this:
/// what the tonemap needs to draw the veil of a source off the frame.
pub(crate) fn veil_scale(style: GlareStyle) -> f32 {
    parts(style).1 / vos_total() as f32
}

fn parts(style: GlareStyle) -> (Option<Aperture>, f32) {
    match style {
        GlareStyle::Bloom => (None, 1.0),
        GlareStyle::Aperture => (Some(Aperture::Iris), CAMERA_VEIL),
        GlareStyle::Eye => (Some(Aperture::Eye), 1.0),
        GlareStyle::Star => (Some(Aperture::Star), CAMERA_VEIL),
    }
}

/// The glare kernel: `grid.width × grid.height` texels, row-major, offset
/// zero at texel 0 and negative offsets wrapped, as a cyclic convolution
/// wants. RGB is the fraction of the source's light (per channel) that lands
/// in the texel; alpha is unused. `diffraction` scales the pattern: 1 is
/// physical.
pub(crate) fn kernel(
    style: GlareStyle,
    diffraction: f32,
    grid: &Grid,
    patterns: &Patterns,
) -> Vec<[f32; 4]> {
    let (aperture, veil_weight) = parts(style);
    let pattern = aperture.map(|a| (patterns.get(a), diffraction.max(0.0) / stretch(a)));
    let norm = veil_weight / vos_total() as f32;
    let (w, h) = (grid.width, grid.height);
    let texel_deg = (1.0 / grid.focal).atan().to_degrees();
    // Subsamples per axis: enough near the centre for the veil's steep
    // near field, and enough elsewhere not to skip over the pattern's texels.
    let far_samples = (texel_deg / TEXEL_DEG).ceil().clamp(1.0, 4.0) as usize;
    let mut out = vec![[0.0f32; 4]; w * h];
    let rows_per = h.div_ceil(threads());
    std::thread::scope(|scope| {
        for (chunk, rows) in out.chunks_mut(rows_per * w).enumerate() {
            scope.spawn(move || {
                for (i, texel) in rows.iter_mut().enumerate() {
                    let index = chunk * rows_per * w + i;
                    let (x, y) = (index % w, index / w);
                    let d = Vec2::new(
                        if x < w / 2 {
                            x as f32
                        } else {
                            x as f32 - w as f32
                        },
                        if y < h / 2 {
                            y as f32
                        } else {
                            y as f32 - h as f32
                        },
                    );
                    let window = {
                        let e = (d / Vec2::from(grid.reach)).length();
                        1.0 - smoothstep(0.6, 1.0, e)
                    };
                    if window <= 0.0 || d == Vec2::ZERO {
                        continue;
                    }
                    let n = if d.length() < 6.0 { 4 } else { far_samples };
                    let mut sum = [0.0f32; 3];
                    for sy in 0..n {
                        for sx in 0..n {
                            let p = d + (Vec2::new(sx as f32, sy as f32) + 0.5) / n as f32 - 0.5;
                            let r = p.length();
                            let theta = (r / grid.focal).atan().to_degrees();
                            let veil = vos(theta) * norm;
                            let mut density = [veil; 3];
                            if let Some((pattern, weight)) = pattern {
                                // The pattern holds energy per stored texel.
                                let at = p / r.max(1e-6) * theta / TEXEL_DEG;
                                let c = (SIZE / 2) as f32;
                                let texel = sample3(&pattern.texels, at + c);
                                for ch in 0..3 {
                                    density[ch] += weight * texel[ch] / (TEXEL_DEG * TEXEL_DEG);
                                }
                            }
                            for ch in 0..3 {
                                sum[ch] += density[ch];
                            }
                        }
                    }
                    // Square degrees this texel subtends, seen from the lens.
                    let r2 = d.length_squared();
                    let solid = grid.focal / (grid.focal * grid.focal + r2).powf(1.5)
                        * (180.0 / PI).powi(2);
                    let scale = solid * window / (n * n) as f32;
                    *texel = [sum[0] * scale, sum[1] * scale, sum[2] * scale, 0.0];
                }
            });
        }
    });
    // The centre texel is the source's own: its neighbours' mean, so the
    // glare neither adds a second core nor leaves a dip under the source.
    let neighbours = [1, w - 1, w, (h - 1) * w];
    let fill: [f32; 3] =
        std::array::from_fn(|c| neighbours.iter().map(|&i| out[i][c]).sum::<f32>() / 4.0);
    out[0] = [fill[0], fill[1], fill[2], 0.0];
    out
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Bilinear read of a stored pattern at texel coordinates; zero outside.
fn sample3(field: &[[f32; 3]], at: Vec2) -> [f32; 3] {
    let (x0, y0) = (at.x.floor(), at.y.floor());
    let (fx, fy) = (at.x - x0, at.y - y0);
    let mut out = [0.0; 3];
    for (dx, dy, weight) in [
        (0.0, 0.0, (1.0 - fx) * (1.0 - fy)),
        (1.0, 0.0, fx * (1.0 - fy)),
        (0.0, 1.0, (1.0 - fx) * fy),
        (1.0, 1.0, fx * fy),
    ] {
        let (x, y) = (x0 + dx, y0 + dy);
        if x < 0.0 || y < 0.0 || x >= SIZE as f32 || y >= SIZE as f32 {
            continue;
        }
        let t = field[y as usize * SIZE + x as usize];
        for c in 0..3 {
            out[c] += weight * t[c];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sum of `field` (`channel` of `stride`) over radii `[r0, r1)` and
    /// angles `[a0, a1)` around the centre of a `side`² field.
    #[allow(clippy::too_many_arguments)]
    fn region(
        field: &[f32],
        side: usize,
        stride: usize,
        channel: usize,
        r0: f32,
        r1: f32,
        a0: f32,
        a1: f32,
    ) -> f32 {
        let c = (side / 2) as f32;
        let mut sum = 0.0;
        for y in 0..side {
            for x in 0..side {
                let d = Vec2::new(x as f32 - c, y as f32 - c);
                let a = d.y.atan2(d.x).rem_euclid(2.0 * PI);
                if (r0..r1).contains(&d.length()) && (a0..a1).contains(&a) {
                    sum += field[(y * side + x) * stride + channel];
                }
            }
        }
        sum
    }

    fn flat(pattern: &Pattern) -> Vec<f32> {
        pattern
            .texels
            .iter()
            .flat_map(|t| [t[0], t[1], t[2]])
            .collect()
    }

    /// A clean disc's pattern is round: its energy does not depend on
    /// direction.
    #[test]
    fn a_round_pupil_spreads_evenly() {
        let power = power_spectrum(
            &rasterize(|p| f32::from(u8::from(p.length() <= PUPIL))),
            REFERENCE_NM,
        );
        let a = region(&power, BAKE, 1, 0, 10.0, 60.0, 0.1, 0.6);
        let b = region(&power, BAKE, 1, 0, 10.0, 60.0, 1.1, 1.6);
        assert!((a - b).abs() < 0.1 * a.max(b), "{a} against {b}");
    }

    /// The defects are seeded: the same every launch, and drawn from Wu et
    /// al.'s distributions. The pinned values change only when the look is
    /// meant to.
    #[test]
    fn the_defects_are_the_same_every_launch() {
        let lens = defects(LENS_SEED, 1.0);
        assert_eq!(lens, defects(LENS_SEED, 1.0));
        assert_eq!(mask(Aperture::Iris), mask(Aperture::Iris));
        assert_eq!((lens.dust.len(), lens.scratches.len()), (28, 30));
        let first = lens.dust[0];
        assert_eq!(
            (
                (first.centre.x * 100.0).round(),
                (first.centre.y * 100.0).round()
            ),
            (-3718.0, -4928.0)
        );
        for dot in &lens.dust {
            assert!(dot.centre.length() <= PUPIL && (1.0..=6.0).contains(&dot.radius));
            assert!((0.0..1.0).contains(&dot.opacity));
        }
        for scratch in &lens.scratches {
            assert!((2..=17).contains(&scratch.points.len()));
        }
        let eye = defects(EYE_SEED, EYE_DEFECTS);
        assert!(eye.dust.len() < lens.dust.len() / 2);
    }

    /// Scratches and dust break the iris's symmetry: its pattern is not the
    /// same turned half a blade.
    #[test]
    fn the_defects_break_the_symmetry() {
        let baked = flat(&bake(Aperture::Iris));
        let sector = 2.0 * PI / 9.0;
        let parts: Vec<f32> = (0..9)
            .map(|k| {
                let a = k as f32 * sector;
                region(&baked, SIZE, 3, 1, 20.0, 120.0, a, a + sector)
            })
            .collect();
        let max = parts.iter().copied().fold(0.0, f32::max);
        let min = parts.iter().copied().fold(f32::MAX, f32::min);
        assert!(max > 1.15 * min, "{parts:?}");
    }

    /// Red spreads wider than blue: more of its light lies far out.
    #[test]
    fn longer_wavelengths_spread_wider() {
        let baked = flat(&bake(Aperture::Iris));
        let far = |c| region(&baked, SIZE, 3, c, 30.0, 120.0, 0.0, 2.0 * PI);
        assert!(far(0) > 1.1 * far(2), "red {} blue {}", far(0), far(2));
    }

    /// Without its core, a pattern holds the fraction of the source's light
    /// that the aperture's edges and defects send elsewhere: some, not most.
    #[test]
    fn a_pattern_is_what_leaves_the_core() {
        let total = |aperture| -> [f32; 3] {
            let pattern = bake(aperture);
            std::array::from_fn(|c| pattern.texels.iter().map(|t| t[c]).sum())
        };
        for aperture in Aperture::ALL {
            let sums = total(aperture);
            for sum in sums {
                assert!((0.02..0.5).contains(&sum), "{aperture:?}: {sums:?}");
            }
        }
    }

    /// The veil's closed-form total is its numeric integral.
    #[test]
    fn the_veil_total_is_its_integral() {
        let steps = 2_000_000u32;
        let mut sum = 0.0f64;
        for i in 0..steps {
            let theta = (f64::from(i) + 0.5) / f64::from(steps) * VOS_EXTENT_DEG;
            let dt = VOS_EXTENT_DEG / f64::from(steps);
            sum += f64::from(vos(theta as f32)) * 2.0 * std::f64::consts::PI * theta * dt;
        }
        let total = vos_total();
        assert!((sum - total).abs() < 0.01 * total, "{sum} against {total}");
    }

    /// A grid like the High one at 1080p with a 50° lens.
    fn grid() -> Grid {
        Grid {
            width: 1024,
            height: 512,
            focal: 256.0 / 2.0 / (25.0f32.to_radians()).tan(),
            reach: [569.0, 256.0],
        }
    }

    /// The bloom kernel is the veil outside the source's texel: with that
    /// texel's own share, integrated finely, it is the whole veil. What it
    /// holds is the physical fraction, about a quarter at this resolution.
    #[test]
    fn the_veil_kernel_is_energy_normalised() {
        let wide = Grid {
            // Wide enough that the fade starts beyond the veil's 30°.
            width: 2048,
            height: 2048,
            focal: 2048.0,
            reach: [1024.0; 2],
        };
        let patterns = Patterns::bake();
        let k = kernel(GlareStyle::Bloom, 1.0, &wide, &patterns);
        let without_centre: f64 = k.iter().skip(1).map(|t| f64::from(t[1])).sum();
        // The centre texel's share, by a fine grid of samples.
        let n = 400;
        let mut centre = 0.0f64;
        for j in 0..n {
            for i in 0..n {
                let p = Vec2::new(
                    (i as f32 + 0.5) / n as f32 - 0.5,
                    (j as f32 + 0.5) / n as f32 - 0.5,
                );
                let theta = (p.length() / wide.focal).atan().to_degrees();
                centre += f64::from(vos(theta));
            }
        }
        let texel_deg2 = f64::from((1.0 / wide.focal).atan().to_degrees()).powi(2);
        let centre = centre / f64::from(n * n) * texel_deg2 / vos_total();
        let total = without_centre + centre;
        assert!((total - 1.0).abs() < 0.03, "whole veil {total}");

        let bloom = kernel(GlareStyle::Bloom, 1.0, &grid(), &patterns);
        let fraction: f32 = bloom.iter().map(|t| t[1]).sum();
        assert!((0.15..0.4).contains(&fraction), "glare fraction {fraction}");
        // Every channel is the same veil.
        let red: f32 = bloom.iter().map(|t| t[0]).sum();
        assert!((red - fraction).abs() < 1e-4);
    }

    /// The pattern adds its own light outside the core, in proportion to
    /// `diffraction`, and the kernel falls smoothly: no texel beyond the
    /// first ring outshines one nearer the source along the same row, so
    /// there is no ring.
    #[test]
    fn diffraction_adds_to_the_veil_without_a_ring() {
        let patterns = Patterns::bake();
        let sum = |style, diffraction| -> f32 {
            kernel(style, diffraction, &grid(), &patterns)
                .iter()
                .map(|t| t[1])
                .sum()
        };
        let veil = sum(GlareStyle::Aperture, 0.0);
        let one = sum(GlareStyle::Aperture, 1.0) - veil;
        let two = sum(GlareStyle::Aperture, 2.0) - veil;
        // Scaled to a real aperture, the pattern sends well under a hundredth
        // of the light beyond a glare texel.
        assert!(one > 1e-4 && one < 0.01, "diffraction adds {one}");
        assert!((two - 2.0 * one).abs() < 1e-3 * two.max(1.0));
        // A camera's veil is a tenth of the eye's.
        let eye = sum(GlareStyle::Bloom, 1.0);
        assert!(
            (veil - CAMERA_VEIL * eye).abs() < 1e-3 * eye,
            "{veil} {eye}"
        );

        let bloom = kernel(GlareStyle::Bloom, 1.0, &grid(), &patterns);
        let row: Vec<f32> = (0..400).map(|x| bloom[x][1]).collect();
        assert!(
            row[0] <= row[1] * 1.5 && row[0] >= row[1],
            "{:?}",
            &row[..3]
        );
        for x in 1..399 {
            assert!(
                row[x + 1] <= row[x],
                "rises at {x}: {:?}",
                &row[x - 1..x + 2]
            );
        }
    }
}
