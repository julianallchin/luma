use sqlx::SqlitePool;

use super::tracks::track_uid;
use crate::models::waveforms::TrackWaveform;

/// Delete waveform rows for a track
pub async fn delete_track_waveform(pool: &SqlitePool, track_id: &str) -> Result<(), String> {
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
    track_id: &str,
    preview_samples_blob: &[u8],
    full_samples_blob: &[u8],
    colors_blob: &[u8],
    preview_colors_blob: &[u8],
    bands_blob: &[u8],
    preview_bands_blob: &[u8],
    sample_rate: i64,
    decoded_duration: f64,
) -> Result<(), String> {
    let uid = track_uid(pool, track_id).await?;

    sqlx::query(
        "INSERT INTO track_waveforms (track_id, uid, preview_samples_blob, full_samples_blob, colors_blob, preview_colors_blob, bands_blob, preview_bands_blob, sample_rate, decoded_duration)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            preview_samples_blob = excluded.preview_samples_blob,
            full_samples_blob = excluded.full_samples_blob,
            colors_blob = excluded.colors_blob,
            preview_colors_blob = excluded.preview_colors_blob,
            bands_blob = excluded.bands_blob,
            preview_bands_blob = excluded.preview_bands_blob,
            sample_rate = excluded.sample_rate,
            decoded_duration = excluded.decoded_duration,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(preview_samples_blob)
    .bind(full_samples_blob)
    .bind(colors_blob)
    .bind(preview_colors_blob)
    .bind(bands_blob)
    .bind(preview_bands_blob)
    .bind(sample_rate)
    .bind(decoded_duration)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to store waveform: {}", e))?;

    Ok(())
}

/// Fetch cached waveform row for a track
/// Note: duration_seconds will be set to 0.0 and must be updated by the caller
pub async fn fetch_track_waveform(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<TrackWaveform>, String> {
    sqlx::query_as::<_, TrackWaveform>(
        "SELECT track_id, uid, preview_samples_blob, full_samples_blob,
         colors_blob, preview_colors_blob, bands_blob, preview_bands_blob, sample_rate,
         decoded_duration
         FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch waveform: {}", e))
}

// -----------------------------------------------------------------------------
// Delta sync support
// -----------------------------------------------------------------------------

/// List dirty track_waveform track_ids for tracks owned by the current user
pub async fn list_dirty_track_waveform_ids(
    pool: &SqlitePool,
    uid: &str,
) -> Result<Vec<String>, String> {
    sqlx::query_scalar(
        "SELECT tw.track_id
         FROM track_waveforms tw
         JOIN tracks t ON tw.track_id = t.id
         WHERE t.uid = ? AND (tw.synced_at IS NULL OR tw.updated_at > tw.synced_at)",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list dirty track_waveform ids: {}", e))
}

/// Mark track_waveform as synced
pub async fn mark_track_waveform_synced(pool: &SqlitePool, track_id: &str) -> Result<(), String> {
    sqlx::query(
        "UPDATE track_waveforms SET synced_at = updated_at, version = version + 1 WHERE track_id = ?",
    )
    .bind(track_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to mark track_waveform synced: {}", e))?;
    Ok(())
}
