use super::fft::{FftService, FFT_SIZE};
use rayon::prelude::*;

const HOP_SIZE: usize = 512;

pub fn calculate_frequency_amplitude(
    fft_service: &FftService,
    samples: &[f32],
    sample_rate: u32,
    frequency_ranges: &[[f32; 2]],
) -> Vec<f32> {
    if samples.is_empty() || sample_rate == 0 || frequency_ranges.is_empty() {
        return Vec::new();
    }

    let frame_count = if samples.len() <= FFT_SIZE {
        1
    } else {
        (samples.len() - FFT_SIZE) / HOP_SIZE + 1
    };

    let mut raw_amplitudes = vec![0.0f32; frame_count];

    let freq_resolution = sample_rate as f32 / FFT_SIZE as f32;
    let spectrum_len = FFT_SIZE / 2 + 1;

    // Convert frequency ranges to FFT bin ranges once
    let bin_ranges: Vec<(usize, usize)> = frequency_ranges
        .iter()
        .map(|&[min_f, max_f]| {
            let min_b = (min_f / freq_resolution).floor() as usize;
            let max_b = (max_f / freq_resolution).ceil() as usize;
            (
                min_b.min(spectrum_len - 1),
                max_b.min(spectrum_len - 1).max(min_b),
            )
        })
        .collect();

    raw_amplitudes.par_iter_mut().enumerate().for_each_init(
        || {
            (
                fft_service.plan.make_input_vec(),
                fft_service.plan.make_output_vec(),
            )
        },
        |(input_vec, spectrum_vec), (i, amplitude_out)| {
            let start = i * HOP_SIZE;

            // Prepare input buffer with windowing
            for j in 0..FFT_SIZE {
                let sample = samples.get(start + j).copied().unwrap_or(0.0);
                input_vec[j] = sample * fft_service.window[j];
            }

            // Perform FFT
            if fft_service.plan.process(input_vec, spectrum_vec).is_ok() {
                let mut total_sum_magnitude = 0.0;
                let mut total_count = 0;

                // Aggregate magnitude across all specified bin ranges
                for (min_b, max_b) in &bin_ranges {
                    for bin in *min_b..=*max_b {
                        let val = spectrum_vec[bin].norm(); // Magnitude
                        total_sum_magnitude += val;
                        total_count += 1;
                    }
                }

                let avg_magnitude = if total_count > 0 {
                    total_sum_magnitude / total_count as f32
                } else {
                    0.0
                };

                *amplitude_out = (avg_magnitude / FFT_SIZE as f32) * 4.0;
            } else {
                *amplitude_out = 0.0;
            }
        },
    );

    raw_amplitudes
}
