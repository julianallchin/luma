use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::State;
use ts_rs::TS;

use crate::database::Db;
use crate::tracks::{decode_track_samples, TARGET_SAMPLE_RATE};
use std::path::Path;

/// Number of samples in preview waveform (low resolution for overview/minimap)
pub const PREVIEW_WAVEFORM_SIZE: usize = 1000;

/// Number of samples in full waveform (high resolution for zoomed view)
pub const FULL_WAVEFORM_SIZE: usize = 10000;

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

/// Ensure waveform data is computed and stored for a track
pub async fn ensure_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
    track_path: &Path,
    _duration_seconds: f64,
) -> Result<(), String> {
    // Check if waveform already exists
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_waveforms WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to check waveform cache: {}", e))?;

    if existing.is_some() {
        eprintln!("[waveform] cache hit for track {}", track_id);
        return Ok(());
    }

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

    // Serialize to JSON
    let preview_json = serde_json::to_string(&preview_samples)
        .map_err(|e| format!("Failed to serialize preview waveform: {}", e))?;
    let full_json = serde_json::to_string(&full_samples)
        .map_err(|e| format!("Failed to serialize full waveform: {}", e))?;

    // Store in database
    sqlx::query(
        "INSERT INTO track_waveforms (track_id, preview_samples_json, full_samples_json, sample_rate)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            preview_samples_json = excluded.preview_samples_json,
            full_samples_json = excluded.full_samples_json,
            sample_rate = excluded.sample_rate,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&preview_json)
    .bind(&full_json)
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
    let row: Option<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT preview_samples_json, full_samples_json, sample_rate FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch waveform: {}", e))?;

    match row {
        Some((preview_json, full_json, sample_rate)) => {
            let preview_samples: Vec<f32> = serde_json::from_str(&preview_json)
                .map_err(|e| format!("Failed to parse preview waveform: {}", e))?;
            let full_samples: Option<Vec<f32>> = full_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|e| format!("Failed to parse full waveform: {}", e))?;

            Ok(TrackWaveform {
                track_id,
                preview_samples,
                full_samples,
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
                sample_rate: TARGET_SAMPLE_RATE,
                duration_seconds: duration,
            })
        }
    }
}

