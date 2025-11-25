use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::State;
use ts_rs::TS;
use realfft::RealFftPlanner;

use crate::audio::decoder::decode_track_samples;
use crate::database::Db;
use crate::tracks::TARGET_SAMPLE_RATE;
use std::path::Path;

/// Number of samples in preview waveform (low resolution for overview/minimap)
pub const PREVIEW_WAVEFORM_SIZE: usize = 1000;

/// Number of samples in full waveform (high resolution for zoomed view)
pub const FULL_WAVEFORM_SIZE: usize = 10000;

/// 3-band envelope data for rekordbox-style waveform rendering
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct BandEnvelopes {
    /// Low frequency envelope (bass) - values 0.0-1.0
    pub low: Vec<f32>,
    /// Mid frequency envelope (vocals/instruments) - values 0.0-1.0
    pub mid: Vec<f32>,
    /// High frequency envelope (hats/air) - values 0.0-1.0
    pub high: Vec<f32>,
}

/// Waveform data for timeline visualization
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackWaveform {
    #[ts(type = "number")]
    pub track_id: i64,
    /// Low-resolution waveform samples (min/max pairs for each bucket)
    pub preview_samples: Vec<f32>,
    /// High-resolution waveform samples (min/max pairs for each bucket)
    pub full_samples: Option<Vec<f32>>,
    /// 3-band envelopes for full waveform (rekordbox-style)
    pub bands: Option<BandEnvelopes>,
    /// 3-band envelopes for preview waveform
    pub preview_bands: Option<BandEnvelopes>,
    /// Legacy: Colors for each bucket in full_samples (interleaved R, G, B bytes)
    pub colors: Option<Vec<u8>>,
    /// Legacy: Colors for each bucket in preview_samples (interleaved R, G, B bytes)
    pub preview_colors: Option<Vec<u8>>,
    #[ts(type = "number")]
    pub sample_rate: u32,
    pub duration_seconds: f64,
}

/// Compute waveform data from audio samples
/// Returns min/max pairs for each bucket (interleaved: [min0, max0, min1, max1, ...])
fn compute_waveform(samples: &[f32], num_buckets: usize) -> Vec<f32> {
    if samples.is_empty() || num_buckets == 0 {
        return vec![0.0; num_buckets * 2];
    }

    let bucket_size = samples.len() / num_buckets;
    if bucket_size == 0 {
        // More buckets than samples - just return normalized samples
        let mut result = Vec::with_capacity(num_buckets * 2);
        for i in 0..num_buckets {
            let sample = samples.get(i).copied().unwrap_or(0.0);
            result.push(sample.min(0.0)); // min
            result.push(sample.max(0.0)); // max
        }
        return result;
    }

    let mut result = Vec::with_capacity(num_buckets * 2);
    for bucket_idx in 0..num_buckets {
        let start = bucket_idx * bucket_size;
        let end = ((bucket_idx + 1) * bucket_size).min(samples.len());
        
        let bucket = &samples[start..end];
        let (min_val, max_val) = bucket.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(min, max), &sample| (min.min(sample), max.max(sample)),
        );

        result.push(if min_val.is_finite() { min_val } else { 0.0 });
        result.push(if max_val.is_finite() { max_val } else { 0.0 });
    }

    result
}

/// Apply butterworth lowpass filter (2nd order)
fn lowpass_filter(samples: &[f32], cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    let omega = 2.0 * std::f32::consts::PI * cutoff_hz / sample_rate;
    let cos_omega = omega.cos();
    let alpha = omega.sin() / (2.0 * 0.707); // Q = 0.707 for butterworth
    
    let b0 = (1.0 - cos_omega) / 2.0;
    let b1 = 1.0 - cos_omega;
    let b2 = (1.0 - cos_omega) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;
    
    // Normalize coefficients
    let b0 = b0 / a0;
    let b1 = b1 / a0;
    let b2 = b2 / a0;
    let a1 = a1 / a0;
    let a2 = a2 / a0;
    
    let mut output = vec![0.0; samples.len()];
    let mut x1 = 0.0;
    let mut x2 = 0.0;
    let mut y1 = 0.0;
    let mut y2 = 0.0;
    
    for (i, &x) in samples.iter().enumerate() {
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        output[i] = y;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
    }
    
    output
}

/// Apply butterworth highpass filter (2nd order)
fn highpass_filter(samples: &[f32], cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    let omega = 2.0 * std::f32::consts::PI * cutoff_hz / sample_rate;
    let cos_omega = omega.cos();
    let alpha = omega.sin() / (2.0 * 0.707);
    
    let b0 = (1.0 + cos_omega) / 2.0;
    let b1 = -(1.0 + cos_omega);
    let b2 = (1.0 + cos_omega) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha;
    
    let b0 = b0 / a0;
    let b1 = b1 / a0;
    let b2 = b2 / a0;
    let a1 = a1 / a0;
    let a2 = a2 / a0;
    
    let mut output = vec![0.0; samples.len()];
    let mut x1 = 0.0;
    let mut x2 = 0.0;
    let mut y1 = 0.0;
    let mut y2 = 0.0;
    
    for (i, &x) in samples.iter().enumerate() {
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        output[i] = y;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
    }
    
    output
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
    
    // Band boundaries (Hz)
    const LOW_END: f32 = 250.0;
    const MID_END: f32 = 4000.0;
    
    // Filter the audio into 3 bands
    let low_audio = lowpass_filter(samples, LOW_END, sr);
    
    // Mid Band: Cascade filters to avoid phase issues from subtraction
    // Highpass(250) -> Lowpass(4000)
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
    
    // Compute peak envelope for each band
    let mut low_env = Vec::with_capacity(num_buckets);
    let mut mid_env = Vec::with_capacity(num_buckets);
    let mut high_env = Vec::with_capacity(num_buckets);
    
    for bucket_idx in 0..num_buckets {
        let start = bucket_idx * bucket_size;
        let end = ((bucket_idx + 1) * bucket_size).min(samples.len());
        
        // Compute peak amplitude in this bucket
        let low_peak = low_audio[start..end].iter().fold(0.0f32, |max, &s| max.max(s.abs()));
        let mid_peak = mid_audio[start..end].iter().fold(0.0f32, |max, &s| max.max(s.abs()));
        let high_peak = high_audio[start..end].iter().fold(0.0f32, |max, &s| max.max(s.abs()));
        
        low_env.push(low_peak);
        mid_env.push(mid_peak);
        high_env.push(high_peak);
    }
    
    // Normalize each band
    // Use 99th percentile to avoid clipping outliers
    // Also gate noise floor to prevent boosting silence
    fn normalize_band(env: &mut [f32]) {
        if env.is_empty() { return; }
        
        let mut sorted: Vec<f32> = env.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        
        let p99_idx = (sorted.len() as f32 * 0.99) as usize;
        let p99 = sorted.get(p99_idx).copied().unwrap_or(1.0);
        
        // Noise gate: if the band is very quiet, don't boost it fully
        // Arbitrary threshold -40dB approx 0.01
        let peak_val = p99.max(0.0001);
        
        for v in env.iter_mut() {
            *v = (*v / peak_val).clamp(0.0, 1.0);
        }
    }
    
    normalize_band(&mut low_env);
    normalize_band(&mut mid_env);
    normalize_band(&mut high_env);
    
    // Apply logarithmic compression (as requested "render it log or something")
    // Mapping: y = log10(1 + 9x) / log10(10) -> Maps 0..1 to 0..1 but boosts mids
    fn apply_log_compression(env: &mut [f32]) {
        for v in env.iter_mut() {
            // Hybrid log-like curve that keeps 0 at 0 and 1 at 1
            // This boosts low values significantly:
            // x=0.1 -> 0.3
            // x=0.5 -> 0.7
            *v = (1.0 + 9.0 * *v).log10(); 
        }
    }

    apply_log_compression(&mut low_env);
    apply_log_compression(&mut mid_env);
    apply_log_compression(&mut high_env);
    
    // Scale Factors:
    // Low: 0.95 (prevent hard clipping at top)
    // Mid: 0.8 (visual hierarchy)
    // High: 0.6 (prevent washing out other bands)
    for v in low_env.iter_mut() { *v *= 0.95; }
    for v in mid_env.iter_mut() { *v *= 0.8; }
    for v in high_env.iter_mut() { *v *= 0.6; }
    
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
    
    // Pre-compute window (Hanning)
    let window: Vec<f32> = (0..fft_size)
        .map(|i| 0.5 * (1.0 - ((2.0 * std::f32::consts::PI * i as f32) / (fft_size as f32 - 1.0)).cos()))
        .collect();

    let bin_freq = sample_rate as f32 / fft_size as f32;
    
    // Frequencies:
    // Low: < 300Hz (Bass/Kick)
    // Mid: 300Hz - 4kHz (Vocals/Instruments)
    // High: > 4kHz (Hats/Air)
    let low_start = (20.0 / bin_freq).floor() as usize;
    let low_end = (300.0 / bin_freq).ceil() as usize;
    let mid_start = (300.0 / bin_freq).floor() as usize;
    let mid_end = (4000.0 / bin_freq).ceil() as usize;
    let high_start = (4000.0 / bin_freq).floor() as usize;
    let high_end = (20000.0 / bin_freq).ceil() as usize;

    let max_bin = spectrum.len().min(high_end + 1);

    let mut e_low_vec = Vec::with_capacity(num_buckets);
    let mut e_mid_vec = Vec::with_capacity(num_buckets);
    let mut e_high_vec = Vec::with_capacity(num_buckets);

    let step = samples.len().max(1) as f32 / num_buckets as f32;

    for i in 0..num_buckets {
        // Center of the bucket
        let center_idx = (i as f32 * step + step / 2.0) as usize;
        
        // Extract frame
        let start_idx = center_idx.saturating_sub(fft_size / 2);
        
        // Prepare input with windowing
        for j in 0..fft_size {
             if start_idx + j < samples.len() {
                 input_window[j] = samples[start_idx + j] * window[j];
             } else {
                 input_window[j] = 0.0;
             }
        }

        // FFT
        r2c.process(&mut input_window, &mut spectrum).unwrap();

        // Sum energies
        let mut sum_low = 0.0;
        let mut sum_mid = 0.0;
        let mut sum_high = 0.0;

        for k in 0..max_bin {
            let amp = spectrum[k].norm_sqr(); // |X|^2
            if k >= low_start && k < low_end { sum_low += amp; }
            else if k >= mid_start && k < mid_end { sum_mid += amp; }
            else if k >= high_start && k < high_end { sum_high += amp; }
        }

        e_low_vec.push(sum_low);
        e_mid_vec.push(sum_mid);
        e_high_vec.push(sum_high);
    }

    // Normalize
    let log_transform = |e: f32| (e + 1e-12).log10();
    let normalize = |vals: &mut [f32]| {
        let min_val = vals.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max_val = vals.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let range = max_val - min_val + 1e-12;
        for v in vals.iter_mut() {
            *v = (*v - min_val) / range;
            *v = v.clamp(0.0, 1.0);
        }
    };

    // Apply log transform
    let mut l_log: Vec<f32> = e_low_vec.iter().map(|&x| log_transform(x)).collect();
    let mut m_log: Vec<f32> = e_mid_vec.iter().map(|&x| log_transform(x)).collect();
    let mut h_log: Vec<f32> = e_high_vec.iter().map(|&x| log_transform(x)).collect();

    // Normalize bands globally (0.0 - 1.0)
    normalize(&mut l_log);
    normalize(&mut m_log);
    normalize(&mut h_log);

    // Weights - Reduce Highs significantly to prevent washing out
    for x in &mut l_log { *x *= 1.0; }
    for x in &mut m_log { *x *= 0.9; }
    for x in &mut h_log { *x *= 0.6; } // Reduce highs influence

    // High Gamma for contrast (separation)
    let gamma = 2.5; 

    let mut result = Vec::with_capacity(num_buckets * 3);
    for i in 0..num_buckets {
        let l = l_log[i].powf(gamma);
        let m = m_log[i].powf(gamma);
        let h = h_log[i].powf(gamma);

        // Rekordbox "RGB" approximate palette:
        // Low (Bass) -> Deep Blue / Purple
        // Mid (Vocals) -> Orange / Gold
        // High (Hats) -> White / Cyan Overlay

        // Base (Low)
        let mut r = 30.0 * l;
        let mut g = 30.0 * l;
        let mut b = 220.0 * l;

        // Add Mid (Orange)
        r += 220.0 * m;
        g += 120.0 * m;
        b += 20.0 * m;

        // Add High (White/Cyan brightness)
        // Reduced contribution to R to keep Mids distinct
        r += 80.0 * h;
        g += 150.0 * h;
        b += 150.0 * h;

        // Clamp
        let r_byte = r.round().min(255.0) as u8;
        let g_byte = g.round().min(255.0) as u8;
        let b_byte = b.round().min(255.0) as u8;

        result.push(r_byte);
        result.push(g_byte);
        result.push(b_byte);
    }

    result
}

/// Ensure waveform data is computed and stored for a track
pub async fn ensure_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
    track_path: &Path,
    _duration_seconds: f64,
) -> Result<(), String> {
    // Delete existing waveform data for this track to ensure clean state
    sqlx::query("DELETE FROM track_waveforms WHERE track_id = ?")
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear existing waveform: {}", e))?;

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
    let colors_json = serde_json::to_string(&colors)
        .map_err(|e| format!("Failed to serialize colors: {}", e))?;
    let preview_colors_json = serde_json::to_string(&preview_colors)
        .map_err(|e| format!("Failed to serialize preview colors: {}", e))?;
    let bands_json = serde_json::to_string(&bands)
        .map_err(|e| format!("Failed to serialize bands: {}", e))?;
    let preview_bands_json = serde_json::to_string(&preview_bands)
        .map_err(|e| format!("Failed to serialize preview bands: {}", e))?;

    // Store in database
    sqlx::query(
        "INSERT INTO track_waveforms (track_id, preview_samples_json, full_samples_json, colors_json, preview_colors_json, bands_json, preview_bands_json, sample_rate)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            preview_samples_json = excluded.preview_samples_json,
            full_samples_json = excluded.full_samples_json,
            colors_json = excluded.colors_json,
            preview_colors_json = excluded.preview_colors_json,
            bands_json = excluded.bands_json,
            preview_bands_json = excluded.preview_bands_json,
            sample_rate = excluded.sample_rate,
            updated_at = datetime('now')"
    )
    .bind(track_id)
    .bind(&preview_json)
    .bind(&full_json)
    .bind(&colors_json)
    .bind(&preview_colors_json)
    .bind(&bands_json)
    .bind(&preview_bands_json)
    .bind(sample_rate as i64)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to store waveform: {}", e))?;

    eprintln!("[waveform] stored waveforms for track {}", track_id);
    Ok(())
}

/// Get waveform data for a track
#[tauri::command]
pub async fn get_track_waveform(
    db: State<'_, Db>,
    track_id: i64,
) -> Result<TrackWaveform, String> {
    // Get track duration
    let duration_seconds: Option<f64> =
        sqlx::query_scalar("SELECT duration_seconds FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&db.0)
            .await
            .map_err(|e| format!("Failed to fetch track: {}", e))?
            .ok_or_else(|| format!("Track {} not found", track_id))?;

    let duration = duration_seconds.unwrap_or(0.0);

    // Try to get cached waveform
    let row: Option<(String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT preview_samples_json, full_samples_json, colors_json, preview_colors_json, bands_json, preview_bands_json, sample_rate FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch waveform: {}", e))?;

    match row {
        Some((preview_json, full_json, colors_json, preview_colors_json, bands_json, preview_bands_json, sample_rate)) => {
            let preview_samples: Vec<f32> = serde_json::from_str(&preview_json)
                .map_err(|e| format!("Failed to parse preview waveform: {}", e))?;
            let full_samples: Option<Vec<f32>> = full_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|e| format!("Failed to parse full waveform: {}", e))?;
            let colors: Option<Vec<u8>> = colors_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|e| format!("Failed to parse colors: {}", e))?;
            let preview_colors: Option<Vec<u8>> = preview_colors_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|e| format!("Failed to parse preview colors: {}", e))?;
            let bands: Option<BandEnvelopes> = bands_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|e| format!("Failed to parse bands: {}", e))?;
            let preview_bands: Option<BandEnvelopes> = preview_bands_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|e| format!("Failed to parse preview bands: {}", e))?;

            Ok(TrackWaveform {
                track_id,
                preview_samples,
                full_samples,
                bands,
                preview_bands,
                colors,
                preview_colors,
                sample_rate: sample_rate as u32,
                duration_seconds: duration,
            })
        }
        None => {
            // Return empty waveform if not computed yet
            Ok(TrackWaveform {
                track_id,
                preview_samples: vec![0.0; PREVIEW_WAVEFORM_SIZE * 2],
                full_samples: None,
                bands: None,
                preview_bands: None,
                colors: None,
                preview_colors: None,
                sample_rate: TARGET_SAMPLE_RATE,
                duration_seconds: duration,
            })
        }
    }
}
