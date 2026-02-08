//! Business logic for waveform operations.
//!
//! The database layer stores/retrieves serialized waveform payloads only.
//! All audio decoding and DSP happens here.

use realfft::RealFftPlanner;
use sqlx::SqlitePool;
use std::path::Path;
use std::time::Instant;

use crate::audio::{decode_track_samples, filter_3band, FilteredBands};
use crate::database::local;
use crate::models::waveforms::{BandEnvelopes, TrackWaveform};

/// Number of samples in preview waveform (low resolution for overview/minimap)
pub const PREVIEW_WAVEFORM_SIZE: usize = 1000;

/// Number of samples in full waveform (high resolution for zoomed view)
pub const FULL_WAVEFORM_SIZE: usize = 30000;

/// Ensure waveform data is computed and stored for a track
pub async fn ensure_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
    track_path: &Path,
    _duration_seconds: f64,
) -> Result<(), String> {
    let t_total = Instant::now();

    // Clear any stale data
    local::waveforms::delete_track_waveform(pool, track_id).await?;

    eprintln!("[waveform] computing waveforms for track {}", track_id);

    // Decode audio samples (returns stereo, convert to mono for waveform analysis)
    let t0 = Instant::now();
    let path = track_path.to_path_buf();
    let (samples, sample_rate) =
        tauri::async_runtime::spawn_blocking(move || -> Result<(Vec<f32>, u32), String> {
            let audio = decode_track_samples(&path, None)?;
            // Convert stereo to mono for waveform analysis
            Ok((audio.to_mono(), audio.sample_rate))
        })
        .await
        .map_err(|e| format!("Waveform decode task failed: {}", e))??;
    let decode_ms = t0.elapsed().as_millis();

    if samples.is_empty() {
        return Err("Cannot compute waveform for empty audio".into());
    }

    // Use the actual decoded sample count for duration — metadata can differ
    // due to encoder padding, VBR headers, etc.
    let decoded_duration = samples.len() as f64 / sample_rate as f64;

    let t0 = Instant::now();

    // Compute both preview and full waveforms
    let preview_samples = compute_waveform(&samples, PREVIEW_WAVEFORM_SIZE);
    let full_samples = compute_waveform(&samples, FULL_WAVEFORM_SIZE);
    let waveform_ms = t0.elapsed().as_millis();

    // Filter once, reuse for both resolutions
    let t0 = Instant::now();
    let filtered = filter_3band(&samples, sample_rate as f32);

    let bands = bucketize_band_envelopes(&filtered, samples.len(), FULL_WAVEFORM_SIZE);
    let preview_bands = bucketize_band_envelopes(&filtered, samples.len(), PREVIEW_WAVEFORM_SIZE);
    let bands_ms = t0.elapsed().as_millis();

    // Compute legacy colors for backwards compatibility
    let t0 = Instant::now();
    let colors = compute_spectral_colors(&samples, sample_rate, FULL_WAVEFORM_SIZE);
    let preview_colors = compute_spectral_colors(&samples, sample_rate, PREVIEW_WAVEFORM_SIZE);
    let colors_ms = t0.elapsed().as_millis();

    // Serialize to binary blobs (raw little-endian bytes)
    let preview_samples_blob = f32_slice_to_bytes(&preview_samples);
    let full_samples_blob = f32_slice_to_bytes(&full_samples);
    let bands_blob = band_envelopes_to_bytes(&bands);
    let preview_bands_blob = band_envelopes_to_bytes(&preview_bands);

    // Store in database
    let t0 = Instant::now();
    let result = local::waveforms::upsert_track_waveform(
        pool,
        track_id,
        &preview_samples_blob,
        &full_samples_blob,
        &colors,
        &preview_colors,
        &bands_blob,
        &preview_bands_blob,
        sample_rate as i64,
        decoded_duration,
    )
    .await;
    let db_ms = t0.elapsed().as_millis();

    eprintln!(
        "[waveform] track {} done in {}ms (decode={}ms waveform={}ms bands={}ms colors={}ms db={}ms)",
        track_id,
        t_total.elapsed().as_millis(),
        decode_ms,
        waveform_ms,
        bands_ms,
        colors_ms,
        db_ms,
    );

    result
}

/// Force-recompute waveform for a track (deletes cached data, recomputes, and returns fresh result).
pub async fn reprocess_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<TrackWaveform, String> {
    let duration_seconds = local::tracks::get_track_duration(pool, track_id)
        .await?
        .ok_or_else(|| format!("Track {} not found", track_id))?;
    let file_path = local::tracks::get_track_path_and_hash(pool, track_id)
        .await?
        .file_path;
    ensure_track_waveform(pool, track_id, Path::new(&file_path), duration_seconds).await?;
    let row = local::waveforms::fetch_track_waveform(pool, track_id)
        .await?
        .ok_or_else(|| format!("Waveform missing for track {} after reprocess", track_id))?;
    build_waveform(track_id, duration_seconds, row)
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
    let file_path = local::tracks::get_track_path_and_hash(pool, track_id)
        .await?
        .file_path;
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
    metadata_duration: f64,
    mut waveform: TrackWaveform,
) -> Result<TrackWaveform, String> {
    // Use decoded_duration if available (already set from DB row), otherwise fall back to metadata
    if waveform.duration_seconds <= 0.0 {
        waveform.duration_seconds = metadata_duration;
    }
    Ok(waveform)
}

/// Compute waveform data from audio samples
/// Returns min/max pairs for each bucket (interleaved: [min0, max0, min1, max1, ...])
pub fn compute_waveform(samples: &[f32], num_buckets: usize) -> Vec<f32> {
    if samples.is_empty() || num_buckets == 0 {
        return vec![0.0; num_buckets * 2];
    }

    if samples.len() < num_buckets {
        let mut result = Vec::with_capacity(num_buckets * 2);
        for i in 0..num_buckets {
            let sample = samples.get(i).copied().unwrap_or(0.0);
            result.push(sample.min(0.0));
            result.push(sample.max(0.0));
        }
        return result;
    }

    let total = samples.len() as f64;
    let buckets = num_buckets as f64;

    let mut result = Vec::with_capacity(num_buckets * 2);
    for bucket_idx in 0..num_buckets {
        let start = (bucket_idx as f64 * total / buckets) as usize;
        let end = (((bucket_idx + 1) as f64 * total / buckets) as usize).min(samples.len());

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

/// Compute 3-band envelopes (low, mid, high) for rekordbox-style waveform.
/// Standalone version that filters internally — use `bucketize_band_envelopes`
/// with pre-filtered bands when computing multiple resolutions.
pub fn compute_band_envelopes(
    samples: &[f32],
    sample_rate: u32,
    num_buckets: usize,
) -> BandEnvelopes {
    if samples.is_empty() || num_buckets == 0 {
        return BandEnvelopes {
            low: vec![0.0; num_buckets],
            mid: vec![0.0; num_buckets],
            high: vec![0.0; num_buckets],
        };
    }

    let filtered = filter_3band(samples, sample_rate as f32);
    bucketize_band_envelopes(&filtered, samples.len(), num_buckets)
}

/// Bucketize pre-filtered 3-band audio into envelope data.
/// The `total_samples` parameter is the length of the original (unfiltered) audio.
pub fn bucketize_band_envelopes(
    filtered: &FilteredBands,
    total_samples: usize,
    num_buckets: usize,
) -> BandEnvelopes {
    if total_samples < num_buckets || num_buckets == 0 {
        return BandEnvelopes {
            low: vec![0.0; num_buckets],
            mid: vec![0.0; num_buckets],
            high: vec![0.0; num_buckets],
        };
    }

    let total = total_samples as f64;
    let buckets = num_buckets as f64;

    let mut low_env = Vec::with_capacity(num_buckets);
    let mut mid_env = Vec::with_capacity(num_buckets);
    let mut high_env = Vec::with_capacity(num_buckets);

    for bucket_idx in 0..num_buckets {
        let start = (bucket_idx as f64 * total / buckets) as usize;
        let end = (((bucket_idx + 1) as f64 * total / buckets) as usize).min(total_samples);

        let low_peak = filtered.low[start..end]
            .iter()
            .fold(0.0f32, |max, &s| max.max(s.abs()));
        let mid_peak = filtered.mid[start..end]
            .iter()
            .fold(0.0f32, |max, &s| max.max(s.abs()));
        let high_peak = filtered.high[start..end]
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

/// Compute RGB colors based on spectral content (Legacy - kept for backwards compatibility).
/// Uses rayon to parallelize FFT computation across chunks of buckets.
pub fn compute_spectral_colors(samples: &[f32], sample_rate: u32, num_buckets: usize) -> Vec<u8> {
    use rayon::prelude::*;

    if samples.is_empty() || num_buckets == 0 {
        return vec![0; num_buckets * 3];
    }

    let fft_size = 2048;
    let bin_freq = sample_rate as f32 / fft_size as f32;
    let low_bin_end = (300.0 / bin_freq).ceil() as usize;
    let mid_bin_end = (3000.0 / bin_freq).ceil() as usize;

    // Pre-compute Hann window (shared across threads)
    let window: Vec<f32> = (0..fft_size)
        .map(|i| {
            0.5 * (1.0 - ((2.0 * std::f32::consts::PI * i as f32) / (fft_size as f32 - 1.0)).cos())
        })
        .collect();

    let total = samples.len() as f64;
    let buckets = num_buckets as f64;

    // Process buckets in parallel — each thread gets its own FFT plan + buffers
    let chunk_size = (num_buckets / rayon::current_num_threads().max(1)).max(256);

    let chunks: Vec<Vec<u8>> = (0..num_buckets)
        .collect::<Vec<_>>()
        .par_chunks(chunk_size)
        .map(|bucket_indices| {
            let mut planner = RealFftPlanner::<f32>::new();
            let r2c = planner.plan_fft_forward(fft_size);
            let mut spectrum = r2c.make_output_vec();
            let mut input_window = r2c.make_input_vec();
            let num_bins = spectrum.len();

            let mut chunk_result = Vec::with_capacity(bucket_indices.len() * 3);

            for &bucket_idx in bucket_indices {
                let start = (bucket_idx as f64 * total / buckets) as usize;
                if start + fft_size > samples.len() {
                    chunk_result.extend_from_slice(&[0, 0, 0]);
                    continue;
                }

                let slice = &samples[start..start + fft_size];
                for i in 0..fft_size {
                    input_window[i] = slice[i] * window[i];
                }

                if r2c.process(&mut input_window, &mut spectrum).is_err() {
                    chunk_result.extend_from_slice(&[0, 0, 0]);
                    continue;
                }

                let mut low_energy = 0.0f32;
                for bin in &spectrum[..low_bin_end.min(num_bins)] {
                    low_energy += (bin.re * bin.re + bin.im * bin.im).sqrt();
                }
                let mut mid_energy = 0.0f32;
                for bin in &spectrum[low_bin_end.min(num_bins)..mid_bin_end.min(num_bins)] {
                    mid_energy += (bin.re * bin.re + bin.im * bin.im).sqrt();
                }
                let mut high_energy = 0.0f32;
                for bin in &spectrum[mid_bin_end.min(num_bins)..num_bins] {
                    high_energy += (bin.re * bin.re + bin.im * bin.im).sqrt();
                }

                let l = (low_energy / 100.0).min(1.0);
                let m = (mid_energy / 100.0).min(1.0);
                let h = (high_energy / 100.0).min(1.0);

                let r = 30.0 * l + 220.0 * m + 80.0 * h;
                let g = 30.0 * l + 120.0 * m + 150.0 * h;
                let b = 220.0 * l + 20.0 * m + 150.0 * h;

                chunk_result.push(r.round().min(255.0) as u8);
                chunk_result.push(g.round().min(255.0) as u8);
                chunk_result.push(b.round().min(255.0) as u8);
            }

            chunk_result
        })
        .collect();

    // Flatten chunks into final result
    let total_bytes: usize = chunks.iter().map(|c| c.len()).sum();
    let mut result = Vec::with_capacity(total_bytes);
    for chunk in chunks {
        result.extend_from_slice(&chunk);
    }
    result
}

// -----------------------------------------------------------------------------
// Binary blob serialization helpers
// -----------------------------------------------------------------------------

/// Serialize a slice of f32 values to raw little-endian bytes
pub fn f32_slice_to_bytes(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for &val in data {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize raw little-endian bytes back to Vec<f32>
pub fn bytes_to_f32_vec(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Serialize BandEnvelopes to a single blob: [low..., mid..., high...]
/// Each band has the same length, so we can split evenly on decode.
fn band_envelopes_to_bytes(bands: &BandEnvelopes) -> Vec<u8> {
    let total = (bands.low.len() + bands.mid.len() + bands.high.len()) * 4;
    let mut bytes = Vec::with_capacity(total);
    for &val in bands
        .low
        .iter()
        .chain(bands.mid.iter())
        .chain(bands.high.iter())
    {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize a blob back to BandEnvelopes (3 equal-length bands)
pub fn bytes_to_band_envelopes(data: &[u8]) -> Option<BandEnvelopes> {
    let floats = bytes_to_f32_vec(data);
    if floats.len() % 3 != 0 {
        return None;
    }
    let band_len = floats.len() / 3;
    Some(BandEnvelopes {
        low: floats[..band_len].to_vec(),
        mid: floats[band_len..band_len * 2].to_vec(),
        high: floats[band_len * 2..].to_vec(),
    })
}
