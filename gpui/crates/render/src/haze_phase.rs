//! How haze droplets scatter light by angle (`shaders/haze_phase.wgsl`).
//!
//! Theatrical haze and fog are droplets of glycol, glycerine or mineral oil,
//! nearly all under a micrometre: in glycol fog the DustTrak found over 90%
//! of the aerosol mass below 1 µm (Aura Health and Safety for IATSE,
//! "Calibration Factors for Haze and Fog in the Film Industry", 2018), and in
//! oil-based fog most airborne particles were under 0.77 µm (NIOSH, cited in
//! Colden/Phylmar's theatrical fog review for CSATF). Droplets that size are
//! Mie scatterers: most of the light goes on nearly straight, a little goes
//! sideways, and a weak lobe comes back. A beam you look down blazes; the same
//! beam seen from the side is faint.
//!
//! The phase function has the form of Jendersie and d'Eon's fit to Mie
//! scattering ("An Approximate Mie Scattering Function for Fog and Cloud
//! Rendering", SIGGRAPH 2023 Talks): a Henyey-Greenstein peak blended with a
//! Draine lobe. Their parameters are for water droplets of one size. Haze
//! fluid bends light more than water (glycol and glycerine about 1.43,
//! mineral oil 1.47, water 1.33) and its droplets come in a spread of sizes,
//! and both send more of the light sideways and back: fitted to water at
//! 0.8 µm, a beam seen from the side or from behind was half as bright as
//! the exact scattering of haze makes it, and one looked straight down three
//! times too bright. So the four parameters here are fitted to the exact
//! Mie scattering of [`DROPLETS`], averaged over visible light, and the
//! tests compute that scattering and hold the fit to it.
//!
//! Every user of the phase reads it relative to isotropic (isotropic is 1,
//! not 1/4π) except the outdoor sun term, which divides by 4π itself.

/// The haze the phase function is fitted to: droplets of refractive index
/// `index`, their diameters lognormal about a count median of `median_um`
/// micrometres with geometric standard deviation `spread`. By mass that is a
/// median of about 0.6 µm with nine tenths under a micrometre, as measured.
#[cfg(test)]
const DROPLETS: Droplets = Droplets {
    index: 1.43,
    median_um: 0.3,
    spread: 1.6,
};

#[cfg(test)]
struct Droplets {
    index: f64,
    median_um: f64,
    spread: f64,
}

/// A phase function `(1 − w)·HG(g_hg) + w·Draine(g_d, α)`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lobes {
    g_hg: f32,
    g_d: f32,
    alpha: f32,
    w_d: f32,
}

/// The haze's: the least-squares fit, in log intensity over the sphere, to
/// the exact scattering of [`DROPLETS`].
const HAZE: Lobes = Lobes {
    g_hg: 0.713,
    g_d: 0.642,
    alpha: 166.0,
    w_d: 0.344,
};

impl Lobes {
    /// The phase function relative to isotropic, as the shader evaluates it.
    #[cfg(test)]
    fn eval(self, cos_t: f32) -> f32 {
        let hg = |g: f32| (1.0 - g * g) / (1.0 + g * g - 2.0 * g * cos_t).powf(1.5);
        let draine = hg(self.g_d) * (1.0 + self.alpha * cos_t * cos_t)
            / (1.0 + self.alpha * (1.0 + 2.0 * self.g_d * self.g_d) / 3.0);
        (1.0 - self.w_d) * hg(self.g_hg) + self.w_d * draine
    }
}

/// `haze_phase_spread` in `shaders/haze_phase.wgsl`: how far, as a share,
/// the forward peak for light from a spread of directions is from the peak
/// at their middle. `cos_t` is the middle's cosine and `spread` the
/// directions' variance about it, radians squared; this is the second term
/// of the mean, so it holds while it is small.
pub(crate) fn spread(cos_t: f32, spread: f32) -> f32 {
    let g = HAZE.g_hg;
    let d = 1.0 + g * g - 2.0 * g * cos_t;
    0.5 * spread * (15.0 * g * g * (1.0 - cos_t * cos_t) - 3.0 * g * cos_t * d).abs() / (d * d)
}

/// The phase constants and `haze_phase`, for any module that scatters light
/// in the haze.
pub(crate) fn prelude() -> String {
    let lobes = HAZE;
    format!(
        "const HAZE_PHASE_G_HG: f32 = {:?};\nconst HAZE_PHASE_G_D: f32 = {:?};\n\
         const HAZE_PHASE_ALPHA: f32 = {:?};\nconst HAZE_PHASE_W_D: f32 = {:?};\n{}",
        lobes.g_hg,
        lobes.g_d,
        lobes.alpha,
        lobes.w_d,
        include_str!("shaders/haze_phase.wgsl")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn haze() -> Lobes {
        HAZE
    }

    /// `a·b` and `a/b` of complex numbers as `(re, im)`.
    fn mul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
        (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
    }
    fn div(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
        let d = b.0 * b.0 + b.1 * b.1;
        ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
    }

    /// The light one sphere scatters at each of `cosines`, unpolarised:
    /// `|S1|² + |S2|²` of Mie theory for size parameter `x` and real
    /// refractive index `m` (Bohren and Huffman, "Absorption and Scattering
    /// of Light by Small Particles", appendix A).
    fn mie(x: f64, m: f64, cosines: &[f64]) -> Vec<f64> {
        let stop = (x + 4.0 * x.cbrt() + 2.0) as usize;
        let mx = m * x;
        // The logarithmic derivative of ψ at `mx`, by downward recurrence.
        let mut d = vec![0.0; stop.max(mx as usize) + 16];
        for n in (1..d.len()).rev() {
            d[n - 1] = n as f64 / mx - 1.0 / (d[n] + n as f64 / mx);
        }
        let (mut psi0, mut psi1) = (x.cos(), x.sin());
        let (mut chi0, mut chi1) = (-x.sin(), x.cos());
        let mut s1 = vec![(0.0, 0.0); cosines.len()];
        let mut s2 = s1.clone();
        let mut pi0 = vec![0.0; cosines.len()];
        let mut pi1 = vec![1.0; cosines.len()];
        for n in 1..=stop {
            let nf = n as f64;
            let psi = (2.0 * nf - 1.0) / x * psi1 - psi0;
            let chi = (2.0 * nf - 1.0) / x * chi1 - chi0;
            let (xi, xi1) = ((psi, -chi), (psi1, -chi1));
            let term = |k: f64| {
                let k = k + nf / x;
                div((k * psi - psi1, 0.0), (k * xi.0 - xi1.0, k * xi.1 - xi1.1))
            };
            let (a, b) = (term(d[n] / m), term(d[n] * m));
            let weight = (2.0 * nf + 1.0) / (nf * (nf + 1.0));
            for (j, &mu) in cosines.iter().enumerate() {
                let pi = pi1[j];
                let tau = nf * mu * pi - (nf + 1.0) * pi0[j];
                let add = |sum: &mut (f64, f64), p: (f64, f64), q: (f64, f64), u: f64, v: f64| {
                    let (p, q) = (mul(p, (u, 0.0)), mul(q, (v, 0.0)));
                    sum.0 += weight * (p.0 + q.0);
                    sum.1 += weight * (p.1 + q.1);
                };
                add(&mut s1[j], a, b, pi, tau);
                add(&mut s2[j], a, b, tau, pi);
                pi1[j] = ((2.0 * nf + 1.0) * mu * pi - (nf + 1.0) * pi0[j]) / nf;
                pi0[j] = pi;
            }
            (psi0, psi1) = (psi1, psi);
            (chi0, chi1) = (chi1, chi);
        }
        s1.iter()
            .zip(&s2)
            .map(|(a, b)| a.0 * a.0 + a.1 * a.1 + b.0 * b.0 + b.1 * b.1)
            .collect()
    }

    /// The exact phase function of [`DROPLETS`] relative to isotropic, at
    /// every half degree: over its sizes and over visible light.
    fn exact() -> Vec<(f64, f64)> {
        const STEPS: usize = 360;
        let cosines: Vec<f64> = (0..STEPS)
            .map(|i| ((i as f64 + 0.5) / STEPS as f64 * std::f64::consts::PI).cos())
            .collect();
        let mut light = vec![0.0; STEPS];
        for wavelength in [0.45, 0.52, 0.55, 0.60, 0.638] {
            for size in 0..30 {
                let z = -3.0 + 6.0 * (f64::from(size) + 0.5) / 30.0;
                let diameter = DROPLETS.median_um * (z * DROPLETS.spread.ln()).exp();
                let x = std::f64::consts::PI * diameter / wavelength;
                let scattered = mie(x, DROPLETS.index, &cosines);
                let count = (-z * z / 2.0).exp();
                for (sum, s) in light.iter_mut().zip(scattered) {
                    *sum += count * s * wavelength * wavelength;
                }
            }
        }
        let sine = |c: f64| (1.0 - c * c).sqrt();
        let mean = cosines
            .iter()
            .zip(&light)
            .map(|(c, l)| l * sine(*c))
            .sum::<f64>()
            * std::f64::consts::PI
            / STEPS as f64
            / 2.0;
        cosines
            .into_iter()
            .zip(light)
            .map(|(c, l)| (c, l / mean))
            .collect()
    }

    /// The lobes are the scattering of haze: within a quarter of the exact
    /// Mie answer from 15° off a beam's axis round to straight behind it.
    /// Nearer the axis two lobes cannot hold the peak's shape, and are
    /// within a half.
    #[test]
    fn the_lobes_are_the_exact_scattering_of_haze() {
        for (cosine, exact) in exact() {
            let degrees = cosine.acos().to_degrees();
            let off = (f64::from(haze().eval(cosine as f32)) / exact - 1.0).abs();
            let allowed = if degrees < 15.0 { 0.5 } else { 0.25 };
            assert!(
                off < allowed,
                "at {degrees:.1}°: {off:.2} from the exact {exact:.3}"
            );
        }
    }

    /// Mean over the sphere of `f(cos θ)`.
    fn sphere_mean(f: impl Fn(f32) -> f32) -> f32 {
        const N: usize = 20_000;
        (0..N)
            .map(|i| f(-1.0 + 2.0 * (i as f32 + 0.5) / N as f32))
            .sum::<f32>()
            / N as f32
    }

    #[test]
    fn haze_scatters_what_it_takes_and_no_more() {
        let mean = sphere_mean(|c| haze().eval(c));
        assert!((mean - 1.0).abs() < 0.01, "{mean}");
    }

    /// Sub-micrometre droplets are strongly forward scattering: an asymmetry
    /// well above the old single lobe's 0.6, short of a cloud's 0.85+, and
    /// the exact scattering's own.
    #[test]
    fn haze_droplets_scatter_mostly_forward() {
        let g = sphere_mean(|c| haze().eval(c) * c);
        let steps = exact().len() as f64;
        let exact: f64 = exact()
            .iter()
            .map(|(c, light)| light * c * (1.0 - c * c).sqrt())
            .sum::<f64>()
            * std::f64::consts::PI
            / steps
            / 2.0;
        assert!((0.65..0.85).contains(&g), "{g}");
        assert!((f64::from(g) - exact).abs() < 0.03, "{g} against {exact}");
    }

    /// Looking down a beam is far brighter than looking across it, and the
    /// light that comes back is weak but not nothing.
    #[test]
    fn a_beam_blazes_ahead_and_fades_to_the_side() {
        let p = haze();
        let ahead = p.eval(1.0);
        let side = p.eval(0.0);
        let back = p.eval(-1.0);
        assert!(ahead > 100.0 * side, "{ahead} {side}");
        assert!(side < 0.25, "{side}");
        assert!(back > 0.0);
    }

    #[test]
    fn every_parameter_is_in_its_range() {
        let p = haze();
        assert!((0.0..1.0).contains(&p.g_hg));
        assert!((0.0..1.0).contains(&p.g_d));
        assert!((0.0..=1.0).contains(&p.w_d));
        assert!(p.alpha > 0.0);
    }
}
