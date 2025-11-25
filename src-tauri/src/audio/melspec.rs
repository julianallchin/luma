use rayon::prelude::*;
use realfft::{num_complex::Complex32, RealFftPlanner};
use std::f32::consts::PI;

pub const MEL_SPEC_WIDTH: usize = 512;
pub const MEL_SPEC_HEIGHT: usize = 128;
const FFT_SIZE: usize = 2048;
const HOP_SIZE: usize = 512;

pub fn generate_melspec(
    samples: &[f32],
    sample_rate: u32,
    width: usize,
    height: usize,
) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; width * height];
    }

    let filters = build_mel_filters(height, FFT_SIZE, sample_rate);
    let window = hann_window(FFT_SIZE);

    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let frame_count = if samples.len() <= FFT_SIZE {
        1
    } else {
        (samples.len() - FFT_SIZE) / HOP_SIZE + 1
    };

    let mut mel_frames = vec![vec![0.0f32; height]; frame_count];

    mel_frames.par_iter_mut().enumerate().for_each_init(
        || StftWorkspace {
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
        },
        |workspace, (frame_index, mel_row)| {
            let start = frame_index * HOP_SIZE;
            for i in 0..FFT_SIZE {
                let sample = samples.get(start + i).copied().unwrap_or(0.0);
                workspace.input[i] = sample * window[i];
            }

            if fft
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

fn build_mel_filters(mel_bins: usize, fft_size: usize, sample_rate: u32) -> Vec<Vec<f32>> {
    let freq_bins = fft_size / 2 + 1;
    let mel_min = hz_to_mel(0.0);
    let mel_max = hz_to_mel(sample_rate as f32 / 2.0);
    let mut mel_points = Vec::with_capacity(mel_bins + 2);
    for i in 0..(mel_bins + 2) {
        mel_points.push(mel_min + (mel_max - mel_min) * i as f32 / (mel_bins + 1) as f32);
    }

    let hz_points: Vec<f32> = mel_points.iter().map(|&mel| mel_to_hz(mel)).collect();
    let bin_points: Vec<usize> = hz_points
        .iter()
        .map(|&hz| {
            let bin = ((hz / sample_rate as f32) * fft_size as f32).floor() as usize;
            bin.min(freq_bins - 1)
        })
        .collect();

    let mut filters = vec![vec![0.0f32; freq_bins]; mel_bins];
    for m in 1..=mel_bins {
        let left = bin_points[m - 1];
        let center = bin_points[m];
        let right = bin_points[m + 1];

        if center > left {
            for k in left..center {
                filters[m - 1][k] = (k - left) as f32 / (center - left) as f32;
            }
        }
        if right > center {
            for k in center..right {
                filters[m - 1][k] = (right - k) as f32 / (right - center) as f32;
            }
        }
    }

    filters
}

fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let angle = 2.0 * PI * i as f32 / (size as f32 - 1.0);
            0.5 * (1.0 - angle.cos())
        })
        .collect()
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

