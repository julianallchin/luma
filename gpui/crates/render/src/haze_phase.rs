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
//! The phase function is Jendersie and d'Eon's fit to Mie scattering by
//! water droplets ("An Approximate Mie Scattering Function for Fog and Cloud
//! Rendering", SIGGRAPH 2023 Talks, and its supplemental for diameters under
//! 5 µm): a Henyey-Greenstein peak blended with a Draine lobe, all four
//! parameters a function of droplet diameter. Glycol's refractive index
//! (about 1.43) is higher than water's (1.33), which the fit does not model; it
//! would send slightly more light back.
//!
//! Every user of the phase reads it relative to isotropic (isotropic is 1,
//! not 1/4π) except the outdoor sun term, which divides by 4π itself.

/// Droplet diameter, micrometres: the middle of the sub-micrometre mass the
/// measurements above found.
const DROPLET_UM: f32 = 0.8;

/// One droplet size's phase function: `(1 − w)·HG(g_hg) + w·Draine(g_d, α)`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lobes {
    g_hg: f32,
    g_d: f32,
    alpha: f32,
    w_d: f32,
}

impl Lobes {
    /// Jendersie and d'Eon's parameters for diameter `d` micrometres, in
    /// 0.1 ≤ d < 1.5 (supplemental, equations 15 to 18).
    fn for_droplet(d: f32) -> Self {
        assert!((0.1..1.5).contains(&d), "the fit's sub-micrometre range");
        let l = d.ln();
        Self {
            g_hg: 0.862 - 0.143 * l * l,
            g_d: 0.379_685
                * (1.196_92
                    * ((l - 0.238_604) * (l + 1.006_67) / (0.507_522 - 0.156_77 * l)).cos()
                    + 1.379_32 * l
                    + 0.062_583_5)
                    .cos()
                + 0.344_213,
            alpha: 250.0,
            w_d: 0.146_209 * (3.387_07 * l + 2.111_93).cos() + 0.316_072 + 0.077_891_7 * l,
        }
    }

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
    let g = Lobes::for_droplet(DROPLET_UM).g_hg;
    let d = 1.0 + g * g - 2.0 * g * cos_t;
    0.5 * spread * (15.0 * g * g * (1.0 - cos_t * cos_t) - 3.0 * g * cos_t * d).abs() / (d * d)
}

/// The phase constants and `haze_phase`, for any module that scatters light
/// in the haze.
pub(crate) fn prelude() -> String {
    let lobes = Lobes::for_droplet(DROPLET_UM);
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
        Lobes::for_droplet(DROPLET_UM)
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
    /// well above the old single lobe's 0.6, short of a cloud's 0.85+.
    #[test]
    fn haze_droplets_scatter_mostly_forward() {
        let g = sphere_mean(|c| haze().eval(c) * c);
        assert!((0.75..0.9).contains(&g), "{g}");
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
