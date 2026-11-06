use realfft::{RealFftPlanner, RealToComplex};
use std::f32::consts::PI;
use std::sync::Arc;

pub const FFT_SIZE: usize = 2048;

pub struct FftService {
    pub plan: Arc<dyn RealToComplex<f32>>,
    pub window: Vec<f32>,
}

impl FftService {
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let plan = planner.plan_fft_forward(FFT_SIZE);
        let window = hann_window(FFT_SIZE);
        Self { plan, window }
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
