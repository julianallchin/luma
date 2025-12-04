use realfft::{RealFftPlanner, RealToComplex};
use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

pub const FFT_SIZE: usize = 2048;

#[derive(Clone)]
pub struct FftService {
    pub plan: Arc<dyn RealToComplex<f32>>,
    pub window: Vec<f32>,
    mel_filters_cache: Arc<Mutex<HashMap<(usize, u32), Vec<Vec<f32>>>>>,
}

impl FftService {
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let plan = planner.plan_fft_forward(FFT_SIZE);
        let window = hann_window(FFT_SIZE);
        Self {
            plan,
            window,
            mel_filters_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn get_mel_filters(&self, mel_bins: usize, sample_rate: u32) -> Arc<Vec<Vec<f32>>> {
        let mut cache = self.mel_filters_cache.lock().unwrap();
        let key = (mel_bins, sample_rate);

        if let Some(filters) = cache.get(&key) {
            return Arc::new(filters.clone());
        }

        let filters = Self::build_mel_filters_internal(mel_bins, FFT_SIZE, sample_rate);
        cache.insert(key, filters.clone());
        Arc::new(filters)
    }

    // Helper for building mel filters (moved from melspec.rs)
    fn build_mel_filters_internal(mel_bins: usize, fft_size: usize, sample_rate: u32) -> Vec<Vec<f32>> {
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
