//! Random-access causal spectra. Only the immutable FFT plan is cached; a
//! sample depends on its audio and absolute time, never previous playback.
use super::fft::{FftService, FFT_SIZE};

thread_local! {
    static FFT: FftService = FftService::new();
}

pub(crate) fn magnitude_spectrum(
    samples: &[f32],
    anchor: usize,
    out: &mut [f32],
) -> Result<(), String> {
    FFT.with(|fft| {
        let mut input = fft.plan.make_input_vec();
        let mut spectrum = fft.plan.make_output_vec();
        let start = anchor as i64 - (FFT_SIZE as i64 - 1);
        for (index, value) in input.iter_mut().enumerate() {
            let source = start + index as i64;
            *value = if source < 0 {
                0.
            } else {
                samples.get(source as usize).copied().unwrap_or(0.)
            } * fft.window[index];
        }
        fft.plan
            .process(&mut input, &mut spectrum)
            .map_err(|e| format!("could not compute audio spectrum: {e}"))?;
        for (value, complex) in out.iter_mut().zip(spectrum) {
            *value = complex.norm();
        }
        Ok(())
    })
}

pub(crate) fn sample(
    samples: &[f32],
    sample_rate: u32,
    seconds: f64,
    hold_edges: bool,
) -> Result<(Vec<f64>, f64), String> {
    let spectrum = raw(samples, sample_rate, seconds, hold_edges)?;
    Ok((
        spectrum
            .into_iter()
            .map(|v| f64::from(v / FFT_SIZE as f32 * 4.))
            .collect(),
        f64::from(sample_rate) / FFT_SIZE as f64,
    ))
}

fn raw(
    samples: &[f32],
    sample_rate: u32,
    seconds: f64,
    hold_edges: bool,
) -> Result<Vec<f32>, String> {
    if sample_rate == 0 || samples.is_empty() {
        return Err("audio spectrum needs nonempty audio and a sample rate".into());
    }
    if !seconds.is_finite() {
        return Err("audio spectrum time must be finite".into());
    }
    let mut spectrum = vec![0.; FFT_SIZE / 2 + 1];
    // Preserve the original sample anchoring precision. The hold option makes
    // old edge behavior explicit; ordinary spectrum nodes are silent outside.
    let seconds = seconds as f32;
    if hold_edges || (seconds >= 0. && seconds < samples.len() as f32 / sample_rate as f32) {
        let anchor = ((seconds.max(0.) * sample_rate as f32).round() as i64)
            .clamp(0, samples.len() as i64 - 1) as usize;
        magnitude_spectrum(samples, anchor, &mut spectrum)?;
    }
    Ok(spectrum)
}

pub(crate) fn sample_band(
    samples: &[f32],
    sample_rate: u32,
    seconds: f64,
    range: [f32; 2],
) -> Result<f32, String> {
    let spectrum = raw(samples, sample_rate, seconds, false)?;
    Ok(band_energy(&spectrum, &[range], sample_rate))
}

/// Mean bin magnitude across the FFT bins covered by `ranges`, scaled to match
/// legacy `calculate_frequency_amplitude`. `spectrum` is raw `.norm()` magnitudes.
pub(crate) fn band_energy(spectrum: &[f32], ranges: &[[f32; 2]], sample_rate: u32) -> f32 {
    if ranges.is_empty() || spectrum.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let freq_resolution = sample_rate as f32 / FFT_SIZE as f32;
    let spectrum_len = spectrum.len(); // FFT_SIZE/2 + 1
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for &[min_f, max_f] in ranges {
        let min_b = (min_f / freq_resolution).floor() as usize;
        let max_b = (max_f / freq_resolution).ceil() as usize;
        let min_b = min_b.min(spectrum_len - 1);
        let max_b = max_b.min(spectrum_len - 1).max(min_b);
        for value in &spectrum[min_b..=max_b] {
            sum += value;
            count += 1;
        }
    }
    (sum / count as f32 / FFT_SIZE as f32) * 4.0
}
