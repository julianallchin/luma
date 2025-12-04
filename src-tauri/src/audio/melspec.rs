use super::fft::{FftService, FFT_SIZE};
use rayon::prelude::*;
use realfft::num_complex::Complex32;

pub const MEL_SPEC_WIDTH: usize = 512;
pub const MEL_SPEC_HEIGHT: usize = 128;
const HOP_SIZE: usize = 512;

pub fn generate_melspec(
    fft_service: &FftService,
    samples: &[f32],
    sample_rate: u32,
    width: usize,
    height: usize,
) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; width * height];
    }

    let filters_arc = fft_service.get_mel_filters(height, sample_rate);
    let filters = filters_arc.as_ref();

    let frame_count = if samples.len() <= FFT_SIZE {
        1
    } else {
        (samples.len() - FFT_SIZE) / HOP_SIZE + 1
    };

    let mut mel_frames = vec![vec![0.0f32; height]; frame_count];

    mel_frames.par_iter_mut().enumerate().for_each_init(
        || StftWorkspace {
            input: fft_service.plan.make_input_vec(),
            spectrum: fft_service.plan.make_output_vec(),
        },
        |workspace, (frame_index, mel_row)| {
            let start = frame_index * HOP_SIZE;
            for i in 0..FFT_SIZE {
                let sample = samples.get(start + i).copied().unwrap_or(0.0);
                workspace.input[i] = sample * fft_service.window[i];
            }

            if fft_service
                .plan
                .process(&mut workspace.input, &mut workspace.spectrum)
                .is_err()
            {
                return;
            }

            for (mel_idx, filter) in filters.iter().enumerate() {
                let mut energy = 0.0f32;
                for (bin, weight) in filter.iter().enumerate() {
                    if *weight == 0.0 {
                        continue;
                    }
                    energy += weight * workspace.spectrum[bin].norm();
                }
                mel_row[mel_idx] = energy;
            }
        },
    );

    aggregate_mel_frames(&mel_frames, width, height)
}

struct StftWorkspace {
    input: Vec<f32>,
    spectrum: Vec<Complex32>,
}

fn aggregate_mel_frames(frames: &[Vec<f32>], width: usize, height: usize) -> Vec<f32> {
    if frames.is_empty() {
        return vec![0.0; width * height];
    }

    let frame_count = frames.len();
    let mut aggregated = vec![0.0f32; width * height];

    for col in 0..width {
        let mut start = (col * frame_count) / width;
        let mut end = ((col + 1) * frame_count) / width;
        if end <= start {
            end = start + 1;
        }
        if start >= frame_count {
            start = frame_count.saturating_sub(1);
            end = frame_count;
        } else if end > frame_count {
            end = frame_count;
        }
        let count = (end - start).max(1);

        for bin in 0..height {
            let mut sum = 0.0f32;
            for frame in &frames[start..end] {
                sum += frame[bin];
            }
            aggregated[col * height + bin] = sum / count as f32;
        }
    }

    normalize_spectrogram(&mut aggregated);
    aggregated
}

fn normalize_spectrogram(data: &mut [f32]) {
    if data.is_empty() {
        return;
    }

    let eps = 1e-8;
    let mut log_values = Vec::with_capacity(data.len());
    for &value in data.iter() {
        log_values.push((value + eps).log10());
    }

    let (min_log, max_log) = log_values
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), &val| {
            (min.min(val), max.max(val))
        });
    let range = (max_log - min_log).max(1e-3);

    for (dst, &log_value) in data.iter_mut().zip(log_values.iter()) {
        *dst = ((log_value - min_log) / range).clamp(0.0, 1.0);
    }
}

