//! Benchmark for waveform DSP pipeline.
//!
//! Generates synthetic audio (simulating a ~206s track at 48kHz mono)
//! and benchmarks each stage of waveform generation independently.
//!
//! Run with: cargo run --release --bin bench_waveform

use std::time::Instant;

use luma_lib::audio::{filter_3band, highpass_filter, lowpass_filter};
use luma_lib::services::waveforms::{
    bucketize_band_envelopes, compute_band_envelopes, compute_spectral_colors, compute_waveform,
    FULL_WAVEFORM_SIZE, PREVIEW_WAVEFORM_SIZE,
};

fn generate_test_audio(duration_secs: f64, sample_rate: u32) -> Vec<f32> {
    let num_samples = (duration_secs * sample_rate as f64) as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let bass = (2.0 * std::f32::consts::PI * 80.0 * t).sin() * 0.4;
        let mid = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.3;
        let high = (2.0 * std::f32::consts::PI * 8000.0 * t).sin() * 0.15;
        let envelope = (2.0 * std::f32::consts::PI * 0.5 * t).sin().abs() * 0.5 + 0.5;
        samples.push((bass + mid + high) * envelope);
    }
    samples
}

fn bench<F: FnMut() -> R, R>(name: &str, iterations: usize, mut f: F) -> std::time::Duration {
    // Warmup
    for _ in 0..2 {
        std::hint::black_box(f());
    }

    let start = Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(f());
    }
    let elapsed = start.elapsed();
    let per_iter = elapsed / iterations as u32;

    println!(
        "  {:<40} {:>8.2}ms  ({} iters, {:.2}ms total)",
        name,
        per_iter.as_secs_f64() * 1000.0,
        iterations,
        elapsed.as_secs_f64() * 1000.0,
    );
    per_iter
}

fn main() {
    let duration_secs = 206.0;
    let sample_rate: u32 = 48000;

    println!("Generating test audio: {duration_secs}s @ {sample_rate}Hz...");
    let samples = generate_test_audio(duration_secs, sample_rate);
    println!(
        "  {} samples ({:.1} MB)\n",
        samples.len(),
        samples.len() as f64 * 4.0 / 1_000_000.0
    );

    let iters = 5;
    println!("=== Individual stages ({iters} iterations each) ===\n");

    let t_preview_waveform = bench("compute_waveform (preview)", iters, || {
        compute_waveform(&samples, PREVIEW_WAVEFORM_SIZE)
    });

    let t_full_waveform = bench("compute_waveform (full)", iters, || {
        compute_waveform(&samples, FULL_WAVEFORM_SIZE)
    });

    let t_preview_bands = bench("compute_band_envelopes (preview)", iters, || {
        compute_band_envelopes(&samples, sample_rate, PREVIEW_WAVEFORM_SIZE)
    });

    let t_full_bands = bench("compute_band_envelopes (full)", iters, || {
        compute_band_envelopes(&samples, sample_rate, FULL_WAVEFORM_SIZE)
    });

    let t_preview_colors = bench("compute_spectral_colors (preview)", iters, || {
        compute_spectral_colors(&samples, sample_rate, PREVIEW_WAVEFORM_SIZE)
    });

    let t_full_colors = bench("compute_spectral_colors (full)", iters, || {
        compute_spectral_colors(&samples, sample_rate, FULL_WAVEFORM_SIZE)
    });

    // Filter benchmarks
    println!("\n=== Filter stages ({iters} iterations each) ===\n");

    bench("lowpass_filter (250Hz)", iters, || {
        lowpass_filter(&samples, 250.0, sample_rate as f32)
    });

    bench("highpass_filter (250Hz)", iters, || {
        highpass_filter(&samples, 250.0, sample_rate as f32)
    });

    bench("filter_3band (parallel)", iters, || {
        filter_3band(&samples, sample_rate as f32)
    });

    // Shared-filter pipeline benchmark
    println!("\n=== Shared-filter pipeline ({iters} iterations each) ===\n");

    bench("filter_3band + bucketize both resolutions", iters, || {
        let filtered = filter_3band(&samples, sample_rate as f32);
        let bands = bucketize_band_envelopes(&filtered, samples.len(), FULL_WAVEFORM_SIZE);
        let preview_bands =
            bucketize_band_envelopes(&filtered, samples.len(), PREVIEW_WAVEFORM_SIZE);
        (bands, preview_bands)
    });

    // Full pipeline benchmark
    println!("\n=== Full pipeline ({iters} iterations each) ===\n");

    bench("full pipeline (all 6 outputs)", iters, || {
        let preview_waveform = compute_waveform(&samples, PREVIEW_WAVEFORM_SIZE);
        let full_waveform = compute_waveform(&samples, FULL_WAVEFORM_SIZE);
        let filtered = filter_3band(&samples, sample_rate as f32);
        let bands = bucketize_band_envelopes(&filtered, samples.len(), FULL_WAVEFORM_SIZE);
        let preview_bands =
            bucketize_band_envelopes(&filtered, samples.len(), PREVIEW_WAVEFORM_SIZE);
        let preview_colors = compute_spectral_colors(&samples, sample_rate, PREVIEW_WAVEFORM_SIZE);
        let full_colors = compute_spectral_colors(&samples, sample_rate, FULL_WAVEFORM_SIZE);
        (
            preview_waveform,
            full_waveform,
            bands,
            preview_bands,
            preview_colors,
            full_colors,
        )
    });

    // Summary
    println!("\n=== Summary ===\n");
    let total = t_preview_waveform
        + t_full_waveform
        + t_preview_bands
        + t_full_bands
        + t_preview_colors
        + t_full_colors;
    println!(
        "  Total (sum of stages):       {:.2}ms",
        total.as_secs_f64() * 1000.0
    );

    let pcts = [
        ("preview waveform", t_preview_waveform),
        ("full waveform", t_full_waveform),
        ("preview bands", t_preview_bands),
        ("full bands", t_full_bands),
        ("preview colors", t_preview_colors),
        ("full colors", t_full_colors),
    ];

    println!();
    for (name, t) in &pcts {
        let pct = t.as_secs_f64() / total.as_secs_f64() * 100.0;
        println!(
            "  {:<40} {:>5.1}%  ({:.2}ms)",
            name,
            pct,
            t.as_secs_f64() * 1000.0
        );
    }
}
