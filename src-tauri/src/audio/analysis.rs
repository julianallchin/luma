use realfft::RealFftPlanner;
use std::f32::consts::PI;

const FFT_SIZE: usize = 2048;
const HOP_SIZE: usize = 512;

pub fn calculate_frequency_amplitude(
    samples: &[f32],
    sample_rate: u32,
    frequency_ranges: &[[f32; 2]],
) -> Vec<f32> {
    if samples.is_empty() || sample_rate == 0 || frequency_ranges.is_empty() {
        return Vec::new();
    }

    let window = hann_window(FFT_SIZE);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let frame_count = if samples.len() <= FFT_SIZE {
        1
    } else {
        (samples.len() - FFT_SIZE) / HOP_SIZE + 1
    };

    let mut raw_amplitudes = Vec::with_capacity(frame_count);

    let mut input_vec = fft.make_input_vec();
    let mut spectrum_vec = fft.make_output_vec();

    let freq_resolution = sample_rate as f32 / FFT_SIZE as f32;
    let spectrum_len = FFT_SIZE / 2 + 1;

    // Convert frequency ranges to FFT bin ranges once
    let bin_ranges: Vec<(usize, usize)> = frequency_ranges.iter().map(|&[min_f, max_f]| {
        let min_b = (min_f / freq_resolution).floor() as usize;
        let max_b = (max_f / freq_resolution).ceil() as usize;
        (min_b.min(spectrum_len - 1), max_b.min(spectrum_len - 1).max(min_b))
    }).collect();


    for i in 0..frame_count {
        let start = i * HOP_SIZE;

        // Prepare input buffer with windowing
        for j in 0..FFT_SIZE {
            let sample = samples.get(start + j).copied().unwrap_or(0.0);
            input_vec[j] = sample * window[j];
        }

        // Perform FFT
        if fft.process(&mut input_vec, &mut spectrum_vec).is_ok() {
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

            let scaled = (avg_magnitude / FFT_SIZE as f32) * 4.0;
            raw_amplitudes.push(scaled);
        } else {
            raw_amplitudes.push(0.0);
        }
    }

    raw_amplitudes
}

fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let angle = 2.0 * PI * i as f32 / (size as f32 - 1.0);
            0.5 * (1.0 - angle.cos())
        })
        .collect()
}