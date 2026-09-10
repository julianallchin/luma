use super::fft::{FftService, FFT_SIZE};
use rayon::prelude::*;
use realfft::num_complex::Complex32;

pub const MEL_SPEC_WIDTH: usize = 512;
pub const MEL_SPEC_HEIGHT: usize = 128;
const HOP_SIZE: usize = 512;

/// A clip-cropped view of one exact audio source. Columns and frequency rows
/// retain their coordinates so preview overlays use the track's real clock.
#[derive(Clone, Debug)]
pub struct Spectrogram {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub span: (f32, f32),
    pub frequencies_hz: Vec<f32>,
}

pub fn inspect(samples: &[f32], sample_rate: u32, span: (f32, f32)) -> Result<Spectrogram, String> {
    if sample_rate == 0
        || samples.is_empty()
        || !span.0.is_finite()
        || !span.1.is_finite()
        || span.1 <= span.0
    {
        return Err(
            "spectrogram needs nonempty audio, a sample rate and a finite time span".into(),
        );
    }
    let sr = sample_rate as f32;
    let start = ((span.0.max(0.) * sr) as usize).min(samples.len());
    let end = ((span.1.max(0.) * sr).ceil() as usize).clamp(start, samples.len());
    let (data, span) = if start < end {
        (
            generate_melspec(
                &FftService::new(),
                &samples[start..end],
                sample_rate,
                MEL_SPEC_WIDTH,
                MEL_SPEC_HEIGHT,
            ),
            (start as f32 / sr, end as f32 / sr),
        )
    } else {
        (vec![0.; MEL_SPEC_WIDTH * MEL_SPEC_HEIGHT], span)
    };
    Ok(Spectrogram {
        width: MEL_SPEC_WIDTH,
        height: MEL_SPEC_HEIGHT,
        data,
        span,
        frequencies_hz: super::fft::mel_center_frequencies(MEL_SPEC_HEIGHT, sample_rate),
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrogram_crops_on_the_absolute_clock_and_is_black_outside_audio() {
        let samples: Vec<_> = (0..8000)
            .map(|i| (i as f32 * std::f32::consts::TAU * 440. / 8000.).sin())
            .collect();
        let full = inspect(&samples, 8000, (0., 1.)).unwrap();
        assert!(full.data.iter().any(|v| *v > 0.9));
        let cropped = inspect(&samples, 8000, (-1., 0.5)).unwrap();
        assert_eq!(cropped.span, (0., 0.5));
        assert_eq!(
            cropped.data,
            generate_melspec(
                &FftService::new(),
                &samples[..4000],
                8000,
                MEL_SPEC_WIDTH,
                MEL_SPEC_HEIGHT
            )
        );
        assert_eq!(
            cropped.frequencies_hz,
            super::super::fft::mel_center_frequencies(MEL_SPEC_HEIGHT, 8000)
        );
        for span in [(-2., -1.), (2., 3.)] {
            let empty = inspect(&samples, 8000, span).unwrap();
            assert_eq!(empty.span, span);
            assert!(empty.data.iter().all(|v| *v == 0.));
        }
        assert!(inspect(&samples, 0, (0., 1.)).is_err());
        assert!(inspect(&samples, 8000, (f32::NAN, 1.)).is_err());
    }
}
