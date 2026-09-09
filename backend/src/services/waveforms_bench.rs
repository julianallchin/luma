//! Opt-in timing of the exact sampler used by the native waveform strips.
//! Run from the GPUI workspace to use the desktop app's backend optimisation:
//! cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib waveform_sampling_timings -- --ignored --nocapture
use super::*;
use std::{hint::black_box, time::Instant};

#[test]
#[ignore = "diagnostic timing; no machine-dependent performance assertion"]
fn waveform_sampling_timings() {
    const RATE: u32 = 48_000;
    const SECONDS: usize = 206;
    // Decode is deliberately outside the measurement: these are stereo PCM
    // buffers of the same shape the process-wide audio cache supplies.
    let samples: Vec<f32> = (0..SECONDS * RATE as usize)
        .flat_map(|i| {
            let t = i as f32 / RATE as f32;
            let sample = (t * 80. * std::f32::consts::TAU).sin() * 0.4
                + (t * 1000. * std::f32::consts::TAU).sin() * 0.3
                + (t * 8000. * std::f32::consts::TAU).sin() * 0.15;
            [sample, sample * 0.9]
        })
        .collect();
    let gains = BandGains {
        low: 1.,
        mid: 1.,
        high: 1.,
    };
    println!("206s, 48kHz stereo; 1200 logical pixels; 30 measured calls after 3 warmups");
    for (name, start, end, buckets) in [
        ("deep zoom 500 px/s, 1x", 60., 64.8, 2400),
        ("deep zoom 500 px/s, 2x", 60., 64.8, 4800),
        ("opening zoom 50 px/s, 1x", 60., 108., 2400),
        ("opening zoom 50 px/s, 2x", 60., 108., 4800),
        ("wide zoom 10 px/s, 1x", 0., 206., 2060),
        ("minimap, 1x", 0., 206., 1200),
        ("minimap, 2x", 0., 206., 2400),
    ] {
        let range = (start * RATE as f64) as usize..(end * RATE as f64) as usize;
        let mut total = Vec::new();
        let mut compression = Vec::new();
        for iteration in 0..33 {
            let begin = Instant::now();
            let peaks = window_band_peaks(black_box(&samples), 2, RATE, range.clone(), buckets);
            let sampled = Instant::now();
            black_box(gains.compress(black_box(&peaks)));
            let done = Instant::now();
            if iteration >= 3 {
                total.push((done - begin).as_secs_f64() * 1000.);
                compression.push((done - sampled).as_secs_f64() * 1000.);
            }
        }
        total.sort_by(f64::total_cmp);
        compression.sort_by(f64::total_cmp);
        println!(
            "{name}: total median={:.3}ms p95={:.3}ms min={:.3}ms; compression median={:.3}ms",
            total[15], total[28], total[0], compression[15]
        );
    }
}

const FILTER_WARMUP_SECONDS: f64 = 0.25;

fn window_band_peaks(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    frames: std::ops::Range<usize>,
    buckets: usize,
) -> BandPeaks {
    let total = samples.len() / channels.max(1);
    if frames.end <= frames.start || total == 0 {
        return BandPeaks::silent(buckets);
    }

    // Filtered over a padded range, bucketized over the asked-for one: the pad
    // is what the filters settle in, so the buckets never see the transient.
    let warmup = (f64::from(sample_rate) * FILTER_WARMUP_SECONDS) as usize;
    let padded = frames.start.saturating_sub(warmup)..(frames.end + warmup).min(total);
    let mono: Vec<f32> = padded
        .clone()
        .map(|index| {
            let base = index * channels;
            samples[base..base + channels].iter().sum::<f32>() / channels as f32
        })
        .collect();

    let filtered = filter_3band(&mono, sample_rate as f32);
    let offset = frames.start - padded.start;
    bucketize_band_peaks(
        &filtered,
        offset..offset + (frames.end - frames.start),
        buckets,
    )
}
