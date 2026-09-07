/// Simple 2nd-order Butterworth filters shared across audio features.
/// These helpers are intentionally lightweight; they trade steep slopes
/// for speed and low allocations.

fn normalize_cutoff(cutoff_hz: f32, sample_rate: f32) -> Option<f32> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return None;
    }

    let nyquist = sample_rate * 0.5;
    let max_cutoff = (nyquist - 1.0).max(1.0);
    if !max_cutoff.is_finite() || max_cutoff <= 0.0 {
        return None;
    }

    let target = cutoff_hz.max(1.0);
    Some(target.min(max_cutoff))
}

/// IIR biquad coefficients (pre-normalized by a0).
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl BiquadCoeffs {
    fn lowpass(cutoff_hz: f32, sample_rate: f32) -> Option<Self> {
        let cutoff = normalize_cutoff(cutoff_hz, sample_rate)?;
        let omega = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let cos_omega = omega.cos();
        let alpha = omega.sin() / (2.0 * 0.707);

        let a0 = 1.0 + alpha;
        Some(Self {
            b0: (1.0 - cos_omega) / 2.0 / a0,
            b1: (1.0 - cos_omega) / a0,
            b2: (1.0 - cos_omega) / 2.0 / a0,
            a1: -2.0 * cos_omega / a0,
            a2: (1.0 - alpha) / a0,
        })
    }

    fn highpass(cutoff_hz: f32, sample_rate: f32) -> Option<Self> {
        let cutoff = normalize_cutoff(cutoff_hz, sample_rate)?;
        let omega = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let cos_omega = omega.cos();
        let alpha = omega.sin() / (2.0 * 0.707);

        let a0 = 1.0 + alpha;
        Some(Self {
            b0: (1.0 + cos_omega) / 2.0 / a0,
            b1: -(1.0 + cos_omega) / a0,
            b2: (1.0 + cos_omega) / 2.0 / a0,
            a1: -2.0 * cos_omega / a0,
            a2: (1.0 - alpha) / a0,
        })
    }

    /// Apply this biquad filter in-place.
    #[inline]
    fn apply_inplace(&self, samples: &mut [f32]) {
        let (b0, b1, b2, a1, a2) = (self.b0, self.b1, self.b2, self.a1, self.a2);
        let mut x1 = 0.0f32;
        let mut x2 = 0.0f32;
        let mut y1 = 0.0f32;
        let mut y2 = 0.0f32;

        for s in samples.iter_mut() {
            let x = *s;
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            *s = y;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
        }
    }
}

/// Apply a Butterworth lowpass filter (2nd order).
pub fn lowpass_filter(samples: &[f32], cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    let Some(coeffs) = BiquadCoeffs::lowpass(cutoff_hz, sample_rate) else {
        return samples.to_vec();
    };
    let mut output = samples.to_vec();
    coeffs.apply_inplace(&mut output);
    output
}

/// Apply a Butterworth highpass filter (2nd order).
pub fn highpass_filter(samples: &[f32], cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    let Some(coeffs) = BiquadCoeffs::highpass(cutoff_hz, sample_rate) else {
        return samples.to_vec();
    };
    let mut output = samples.to_vec();
    coeffs.apply_inplace(&mut output);
    output
}

/// Pre-filtered 3-band audio (low, mid, high) for shared use across resolutions.
pub struct FilteredBands {
    pub low: Vec<f32>,
    pub mid: Vec<f32>,
    pub high: Vec<f32>,
}

/// Filter audio into 3 bands using Butterworth crossover filters.
/// Returns owned filtered buffers that can be shared for multiple bucket resolutions.
///
/// Bands: low (<250Hz), mid (250-4000Hz), high (>4000Hz).
/// Uses rayon to run the 3 filter chains in parallel.
pub fn filter_3band(samples: &[f32], sample_rate: f32) -> FilteredBands {
    const LOW_END: f32 = 250.0;
    const MID_END: f32 = 4000.0;

    let lp_low = BiquadCoeffs::lowpass(LOW_END, sample_rate);
    let hp_low = BiquadCoeffs::highpass(LOW_END, sample_rate);
    let lp_mid = BiquadCoeffs::lowpass(MID_END, sample_rate);
    let hp_high = BiquadCoeffs::highpass(MID_END, sample_rate);

    // Run the 3 band filters in parallel
    let (low, (mid, high)) = rayon::join(
        || {
            let mut buf = samples.to_vec();
            if let Some(c) = &lp_low {
                c.apply_inplace(&mut buf);
            }
            buf
        },
        || {
            rayon::join(
                || {
                    // Mid = highpass(250) then lowpass(4000)
                    let mut buf = samples.to_vec();
                    if let Some(c) = &hp_low {
                        c.apply_inplace(&mut buf);
                    }
                    if let Some(c) = &lp_mid {
                        c.apply_inplace(&mut buf);
                    }
                    buf
                },
                || {
                    let mut buf = samples.to_vec();
                    if let Some(c) = &hp_high {
                        c.apply_inplace(&mut buf);
                    }
                    buf
                },
            )
        },
    );

    FilteredBands { low, mid, high }
}
