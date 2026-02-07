use sqlx::SqlitePool;

use crate::models::waveforms::TrackWaveform;

/// Delete waveform rows for a track
pub async fn delete_track_waveform(pool: &SqlitePool, track_id: i64) -> Result<(), String> {
    sqlx::query("DELETE FROM track_waveforms WHERE track_id = ?")
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear existing waveform: {}", e))?;
    Ok(())
}

/// Upsert waveform payload for a track (binary blob storage)
#[allow(clippy::too_many_arguments)]
pub async fn upsert_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
    preview_samples_blob: &[u8],
    full_samples_blob: &[u8],
    colors_blob: &[u8],
    preview_colors_blob: &[u8],
    bands_blob: &[u8],
    preview_bands_blob: &[u8],
    sample_rate: i64,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO track_waveforms (track_id, preview_samples_blob, full_samples_blob, colors_blob, preview_colors_blob, bands_blob, preview_bands_blob, sample_rate)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            preview_samples_blob = excluded.preview_samples_blob,
            full_samples_blob = excluded.full_samples_blob,
            colors_blob = excluded.colors_blob,
            preview_colors_blob = excluded.preview_colors_blob,
            bands_blob = excluded.bands_blob,
            preview_bands_blob = excluded.preview_bands_blob,
            sample_rate = excluded.sample_rate,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(preview_samples_blob)
    .bind(full_samples_blob)
    .bind(colors_blob)
    .bind(preview_colors_blob)
    .bind(bands_blob)
    .bind(preview_bands_blob)
    .bind(sample_rate)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to store waveform: {}", e))?;

    Ok(())
}

/// Fetch cached waveform row for a track
/// Note: duration_seconds will be set to 0.0 and must be updated by the caller
pub async fn fetch_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<Option<TrackWaveform>, String> {
    sqlx::query_as::<_, TrackWaveform>(
        "SELECT track_id, remote_id, uid, preview_samples_blob, full_samples_blob,
         colors_blob, preview_colors_blob, bands_blob, preview_bands_blob, sample_rate
         FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch waveform: {}", e))
}
