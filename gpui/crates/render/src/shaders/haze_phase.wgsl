// The haze's phase function (`haze_phase.rs`): a Henyey-Greenstein forward
// peak blended with a Draine lobe, Jendersie and d'Eon's fit to Mie
// scattering by sub-micrometre droplets. `HAZE_PHASE_*` are prepended.
//
// Relative to isotropic: a medium that scattered equally every way would
// read 1 here, not 1/4pi. A cone's intensity is a dimmer times the optic's
// gain, and the absolute scale lives in the beam gain, so only the shape
// matters to the beams; the sun's term divides by 4pi itself.
fn haze_phase(cos_t: f32) -> f32 {
    let peak = haze_phase_lobe(cos_t, HAZE_PHASE_G_HG);
    let g = HAZE_PHASE_G_D;
    let bulk = haze_phase_lobe(cos_t, g) * (1.0 + HAZE_PHASE_ALPHA * cos_t * cos_t)
        / (1.0 + HAZE_PHASE_ALPHA * (1.0 + 2.0 * g * g) / 3.0);
    return mix(peak, bulk, HAZE_PHASE_W_D);
}

/// How far, as a share, the forward peak for light from a spread of
/// directions is from the peak at their middle: `cos_t` the middle's cosine,
/// `spread` the directions' variance about it in radians squared. The second
/// term of the mean, so it holds while it is small.
fn haze_phase_spread(cos_t: f32, spread: f32) -> f32 {
    let g = HAZE_PHASE_G_HG;
    let d = 1.0 + g * g - 2.0 * g * cos_t;
    return 0.5 * spread * abs(15.0 * g * g * (1.0 - cos_t * cos_t) - 3.0 * g * cos_t * d) / (d * d);
}

/// Henyey-Greenstein relative to isotropic.
fn haze_phase_lobe(cos_t: f32, g: f32) -> f32 {
    let g2 = g * g;
    let inverse = inverseSqrt(max(1.0 + g2 - 2.0 * g * cos_t, 1e-4));
    return (1.0 - g2) * inverse * inverse * inverse;
}
