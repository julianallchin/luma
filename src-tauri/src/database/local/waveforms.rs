use sqlx::SqlitePool;

/// Delete waveform rows for a track
pub async fn delete_track_waveform(pool: &SqlitePool, track_id: i64) -> Result<(), String> {
    sqlx::query("DELETE FROM track_waveforms WHERE track_id = ?")
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear existing waveform: {}", e))?;
    Ok(())
}

/// Upsert waveform payload for a track
#[allow(clippy::too_many_arguments)]
pub async fn upsert_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
    preview_samples_json: &str,
    full_samples_json: &str,
    colors_json: &str,
    preview_colors_json: &str,
    bands_json: &str,
    preview_bands_json: &str,
    sample_rate: i64,
) -> Result<(), String> {
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
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(preview_samples_json)
    .bind(full_samples_json)
    .bind(colors_json)
    .bind(preview_colors_json)
    .bind(bands_json)
    .bind(preview_bands_json)
    .bind(sample_rate)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to store waveform: {}", e))?;

    Ok(())
}

/// Fetch cached waveform row for a track
pub async fn fetch_track_waveform(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<
    Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    )>,
    String,
> {
    sqlx::query_as(
        "SELECT preview_samples_json, full_samples_json, colors_json, preview_colors_json, bands_json, preview_bands_json, sample_rate FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch waveform: {}", e))
}
