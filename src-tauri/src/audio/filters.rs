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

/// Apply a Butterworth lowpass filter (2nd order).
pub fn lowpass_filter(samples: &[f32], cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    let Some(cutoff) = normalize_cutoff(cutoff_hz, sample_rate) else {
        return samples.to_vec();
    };

    let omega = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
    let cos_omega = omega.cos();
    let alpha = omega.sin() / (2.0 * 0.707); // Q = 0.707 for butterworth

    let b0 = (1.0 - cos_omega) / 2.0;
    let b1 = 1.0 - cos_omega;
    let b2 = (1.0 - cos_omega) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;

    // Normalize coefficients
    let b0 = b0 / a0;
    let b1 = b1 / a0;
    let b2 = b2 / a0;
    let a1 = a1 / a0;
    let a2 = a2 / a0;

    let mut output = vec![0.0; samples.len()];
    let mut x1 = 0.0;
    let mut x2 = 0.0;
    let mut y1 = 0.0;
    let mut y2 = 0.0;

    for (i, &x) in samples.iter().enumerate() {
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        output[i] = y;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
    }

    output
}

/// Apply a Butterworth highpass filter (2nd order).
pub fn highpass_filter(samples: &[f32], cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    let Some(cutoff) = normalize_cutoff(cutoff_hz, sample_rate) else {
        return samples.to_vec();
    };

    let omega = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
    let cos_omega = omega.cos();
    let alpha = omega.sin() / (2.0 * 0.707);

    let b0 = (1.0 + cos_omega) / 2.0;
    let b1 = -(1.0 + cos_omega);
    let b2 = (1.0 + cos_omega) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;

    let b0 = b0 / a0;
    let b1 = b1 / a0;
    let b2 = b2 / a0;
    let a1 = a1 / a0;
    let a2 = a2 / a0;

    let mut output = vec![0.0; samples.len()];
    let mut x1 = 0.0;
    let mut x2 = 0.0;
    let mut y1 = 0.0;
    let mut y2 = 0.0;

    for (i, &x) in samples.iter().enumerate() {
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        output[i] = y;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
    }

    output
}
