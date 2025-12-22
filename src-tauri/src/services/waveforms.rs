//! Business logic for waveform operations.
//!
//! The database layer stores/retrieves serialized waveform payloads only.
//! All audio decoding and DSP happens here.

use realfft::RealFftPlanner;
use sqlx::SqlitePool;
use std::path::Path;

use crate::audio::{decoder::decode_track_samples, highpass_filter, lowpass_filter};
use crate::database::local;
use crate::models::waveforms::{BandEnvelopes, TrackWaveform};

/// Number of samples in preview waveform (low resolution for overview/minimap)
pub const PREVIEW_WAVEFORM_SIZE: usize = 1000;

/// Number of samples in full waveform (high resolution for zoomed view)
pub const FULL_WAVEFORM_SIZE: usize = 10000;

/// Ensure waveform data is computed and stored for a track
pub async fn ensure_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
    track_path: &Path,
    _duration_seconds: f64,
) -> Result<(), String> {
    // Clear any stale data
    local::waveforms::delete_track_waveform(pool, track_id).await?;

    eprintln!("[waveform] computing waveforms for track {}", track_id);

    // Decode audio samples
    let path = track_path.to_path_buf();
    let (samples, sample_rate) =
        tauri::async_runtime::spawn_blocking(move || decode_track_samples(&path, None))
            .await
            .map_err(|e| format!("Waveform decode task failed: {}", e))??;

    if samples.is_empty() {
        return Err("Cannot compute waveform for empty audio".into());
    }

    // Compute both preview and full waveforms
    let preview_samples = compute_waveform(&samples, PREVIEW_WAVEFORM_SIZE);
    let full_samples = compute_waveform(&samples, FULL_WAVEFORM_SIZE);

    // Compute 3-band envelopes (new rekordbox-style)
    let bands = compute_band_envelopes(&samples, sample_rate, FULL_WAVEFORM_SIZE);
    let preview_bands = compute_band_envelopes(&samples, sample_rate, PREVIEW_WAVEFORM_SIZE);

    // Compute legacy colors for backwards compatibility
    let colors = compute_spectral_colors(&samples, sample_rate, FULL_WAVEFORM_SIZE);
    let preview_colors = compute_spectral_colors(&samples, sample_rate, PREVIEW_WAVEFORM_SIZE);

    // Serialize to JSON
    let preview_json = serde_json::to_string(&preview_samples)
        .map_err(|e| format!("Failed to serialize preview waveform: {}", e))?;
    let full_json = serde_json::to_string(&full_samples)
        .map_err(|e| format!("Failed to serialize full waveform: {}", e))?;
    let colors_json =
        serde_json::to_string(&colors).map_err(|e| format!("Failed to serialize colors: {}", e))?;
    let preview_colors_json = serde_json::to_string(&preview_colors)
        .map_err(|e| format!("Failed to serialize preview colors: {}", e))?;
    let bands_json =
        serde_json::to_string(&bands).map_err(|e| format!("Failed to serialize bands: {}", e))?;
    let preview_bands_json = serde_json::to_string(&preview_bands)
        .map_err(|e| format!("Failed to serialize preview bands: {}", e))?;

    // Store in database
    local::waveforms::upsert_track_waveform(
        pool,
        track_id,
        &preview_json,
        &full_json,
        &colors_json,
        &preview_colors_json,
        &bands_json,
        &preview_bands_json,
        sample_rate as i64,
    )
    .await
}

/// Get waveform for a track, computing if missing.
pub async fn get_track_waveform(pool: &SqlitePool, track_id: i64) -> Result<TrackWaveform, String> {
    let duration_seconds = local::tracks::get_track_duration(pool, track_id)
        .await?
        .ok_or_else(|| format!("Track {} not found", track_id))?;

    // Try cached waveform
    let row = local::waveforms::fetch_track_waveform(pool, track_id).await?;

    if let Some(row) = row {
        return build_waveform(track_id, duration_seconds, row);
    }

    // If not cached, compute and fetch again
    let file_path = local::tracks::get_track_path_and_hash(pool, track_id).await?.file_path;
    ensure_track_waveform(pool, track_id, Path::new(&file_path), duration_seconds).await?;
    let cached = local::waveforms::fetch_track_waveform(pool, track_id).await?;
    let row = cached.ok_or_else(|| format!("Waveform missing for track {}", track_id))?;
    build_waveform(track_id, duration_seconds, row)
}

// -----------------------------------------------------------------------------
// DSP helpers
// -----------------------------------------------------------------------------

fn build_waveform(
    _track_id: i64,
    duration_seconds: f64,
    mut waveform: TrackWaveform,
) -> Result<TrackWaveform, String> {
    // Set the duration_seconds field which isn't in the database
    waveform.duration_seconds = duration_seconds;
    Ok(waveform)
}

/// Compute waveform data from audio samples
/// Returns min/max pairs for each bucket (interleaved: [min0, max0, min1, max1, ...])
fn compute_waveform(samples: &[f32], num_buckets: usize) -> Vec<f32> {
    if samples.is_empty() || num_buckets == 0 {
        return vec![0.0; num_buckets * 2];
    }

    let bucket_size = samples.len() / num_buckets;
    if bucket_size == 0 {
        let mut result = Vec::with_capacity(num_buckets * 2);
        for i in 0..num_buckets {
            let sample = samples.get(i).copied().unwrap_or(0.0);
            result.push(sample.min(0.0));
            result.push(sample.max(0.0));
        }
        return result;
    }

    let mut result = Vec::with_capacity(num_buckets * 2);
    for bucket_idx in 0..num_buckets {
        let start = bucket_idx * bucket_size;
        let end = ((bucket_idx + 1) * bucket_size).min(samples.len());

        let bucket = &samples[start..end];
        let (min_val, max_val) = bucket
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), &sample| {
                (min.min(sample), max.max(sample))
            });

        result.push(if min_val.is_finite() { min_val } else { 0.0 });
        result.push(if max_val.is_finite() { max_val } else { 0.0 });
    }

    result
}

/// Compute 3-band envelopes (low, mid, high) for rekordbox-style waveform
fn compute_band_envelopes(samples: &[f32], sample_rate: u32, num_buckets: usize) -> BandEnvelopes {
    if samples.is_empty() || num_buckets == 0 {
        return BandEnvelopes {
            low: vec![0.0; num_buckets],
            mid: vec![0.0; num_buckets],
            high: vec![0.0; num_buckets],
        };
    }

    let sr = sample_rate as f32;

    const LOW_END: f32 = 250.0;
    const MID_END: f32 = 4000.0;

    let low_audio = lowpass_filter(samples, LOW_END, sr);
    let mid_temp = highpass_filter(samples, LOW_END, sr);
    let mid_audio = lowpass_filter(&mid_temp, MID_END, sr);
    let high_audio = highpass_filter(samples, MID_END, sr);

    let bucket_size = samples.len() / num_buckets;
    if bucket_size == 0 {
        return BandEnvelopes {
            low: vec![0.0; num_buckets],
            mid: vec![0.0; num_buckets],
            high: vec![0.0; num_buckets],
        };
    }

    let mut low_env = Vec::with_capacity(num_buckets);
    let mut mid_env = Vec::with_capacity(num_buckets);
    let mut high_env = Vec::with_capacity(num_buckets);

    for bucket_idx in 0..num_buckets {
        let start = bucket_idx * bucket_size;
        let end = ((bucket_idx + 1) * bucket_size).min(samples.len());

        let low_peak = low_audio[start..end]
            .iter()
            .fold(0.0f32, |max, &s| max.max(s.abs()));
        let mid_peak = mid_audio[start..end]
            .iter()
            .fold(0.0f32, |max, &s| max.max(s.abs()));
        let high_peak = high_audio[start..end]
            .iter()
            .fold(0.0f32, |max, &s| max.max(s.abs()));

        low_env.push(low_peak);
        mid_env.push(mid_peak);
        high_env.push(high_peak);
    }

    fn normalize_band(env: &mut [f32]) {
        if env.is_empty() {
            return;
        }

        let mut sorted: Vec<f32> = env.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let p99_idx = (sorted.len() as f32 * 0.99) as usize;
        let p99 = sorted.get(p99_idx).copied().unwrap_or(1.0);

        let peak_val = p99.max(0.0001);

        for v in env.iter_mut() {
            *v = (*v / peak_val).clamp(0.0, 1.0);
        }
    }

    normalize_band(&mut low_env);
    normalize_band(&mut mid_env);
    normalize_band(&mut high_env);

    fn apply_log_compression(env: &mut [f32]) {
        for v in env.iter_mut() {
            *v = (1.0 + 9.0 * *v).log10();
        }
    }

    apply_log_compression(&mut low_env);
    apply_log_compression(&mut mid_env);
    apply_log_compression(&mut high_env);

    for v in low_env.iter_mut() {
        *v *= 0.95;
    }
    for v in mid_env.iter_mut() {
        *v *= 0.8;
    }
    for v in high_env.iter_mut() {
        *v *= 0.6;
    }

    BandEnvelopes {
        low: low_env,
        mid: mid_env,
        high: high_env,
    }
}

/// Compute RGB colors based on spectral content (Legacy - kept for backwards compatibility)
fn compute_spectral_colors(samples: &[f32], sample_rate: u32, num_buckets: usize) -> Vec<u8> {
    if samples.is_empty() || num_buckets == 0 {
        return vec![0; num_buckets * 3];
    }

    let fft_size = 2048;
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(fft_size);

    let mut spectrum = r2c.make_output_vec();
    let mut input_window = r2c.make_input_vec();

    let window: Vec<f32> = (0..fft_size)
        .map(|i| {
            0.5 * (1.0 - ((2.0 * std::f32::consts::PI * i as f32) / (fft_size as f32 - 1.0)).cos())
        })
        .collect();

    let bin_freq = sample_rate as f32 / fft_size as f32;

    let mut result = Vec::with_capacity(num_buckets * 3);

    let hop_size = (samples.len() / num_buckets).max(fft_size);

    for bucket_idx in 0..num_buckets {
        let start = bucket_idx * hop_size;
        if start + fft_size > samples.len() {
            result.extend_from_slice(&[0, 0, 0]);
            continue;
        }

        let slice = &samples[start..start + fft_size];
        for i in 0..fft_size {
            input_window[i] = slice[i] * window[i];
        }

        if r2c.process(&mut input_window, &mut spectrum).is_err() {
            result.extend_from_slice(&[0, 0, 0]);
            continue;
        }

        let mut low_energy = 0.0;
        let mut mid_energy = 0.0;
        let mut high_energy = 0.0;

        for (bin, freq_bin) in spectrum.iter().enumerate() {
            let freq = bin as f32 * bin_freq;
            let mag = (freq_bin.re.powi(2) + freq_bin.im.powi(2)).sqrt();

            if freq < 300.0 {
                low_energy += mag;
            } else if freq < 3000.0 {
                mid_energy += mag;
            } else {
                high_energy += mag;
            }
        }

        let l = (low_energy / 100.0).min(1.0);
        let m = (mid_energy / 100.0).min(1.0);
        let h = (high_energy / 100.0).min(1.0);

        let mut r = 30.0 * l;
        let mut g = 30.0 * l;
        let mut b = 220.0 * l;

        r += 220.0 * m;
        g += 120.0 * m;
        b += 20.0 * m;

        r += 80.0 * h;
        g += 150.0 * h;
        b += 150.0 * h;

        let r_byte = r.round().min(255.0) as u8;
        let g_byte = g.round().min(255.0) as u8;
        let b_byte = b.round().min(255.0) as u8;

        result.push(r_byte);
        result.push(g_byte);
        result.push(b_byte);
    }

    result
}
