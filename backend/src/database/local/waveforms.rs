use sqlx::{SqliteConnection, SqlitePool};

use super::tracks::track_uid;
use crate::models::waveforms::{BandGains, TrackWaveform};

/// One row of `track_waveforms` as the caller has it, before it becomes bind
/// parameters.
///
/// The blobs and the gains travel together because they are one measurement:
/// `band_gains` is the units `bands_blob` and `preview_bands_blob` are in, and
/// a row that stored the envelopes without them would be a row nothing else can
/// be compared against.
pub struct StoredWaveform<'a> {
    pub preview_samples_blob: &'a [u8],
    pub full_samples_blob: &'a [u8],
    pub colors_blob: &'a [u8],
    pub preview_colors_blob: &'a [u8],
    pub bands_blob: &'a [u8],
    pub preview_bands_blob: &'a [u8],
    pub band_gains: BandGains,
    pub sample_rate: i64,
    pub decoded_duration: f64,
}

/// Upsert waveform payload for a track (binary blob storage)
pub async fn upsert_track_waveform(
    pool: &SqlitePool,
    track_id: &str,
    waveform: &StoredWaveform<'_>,
) -> Result<(), String> {
    let uid = track_uid(pool, track_id).await?;

    upsert_track_waveform_for_connection(
        &mut *pool
            .acquire()
            .await
            .map_err(|error| format!("Failed to acquire waveform connection: {error}"))?,
        track_id,
        uid.as_deref(),
        waveform,
    )
    .await
}

/// Transaction-bound waveform publication. The caller supplies the track UID
/// read through the same authorized connection, so child ownership cannot be
/// resolved from a different identity snapshot.
pub async fn upsert_track_waveform_for_connection(
    connection: &mut SqliteConnection,
    track_id: &str,
    uid: Option<&str>,
    waveform: &StoredWaveform<'_>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO track_waveforms (track_id, uid, preview_samples_blob, full_samples_blob, colors_blob, preview_colors_blob, bands_blob, preview_bands_blob, band_gain_low, band_gain_mid, band_gain_high, sample_rate, decoded_duration)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            preview_samples_blob = excluded.preview_samples_blob,
            full_samples_blob = excluded.full_samples_blob,
            colors_blob = excluded.colors_blob,
            preview_colors_blob = excluded.preview_colors_blob,
            bands_blob = excluded.bands_blob,
            preview_bands_blob = excluded.preview_bands_blob,
            band_gain_low = excluded.band_gain_low,
            band_gain_mid = excluded.band_gain_mid,
            band_gain_high = excluded.band_gain_high,
            sample_rate = excluded.sample_rate,
            decoded_duration = excluded.decoded_duration,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(uid)
    .bind(waveform.preview_samples_blob)
    .bind(waveform.full_samples_blob)
    .bind(waveform.colors_blob)
    .bind(waveform.preview_colors_blob)
    .bind(waveform.bands_blob)
    .bind(waveform.preview_bands_blob)
    .bind(f64::from(waveform.band_gains.low))
    .bind(f64::from(waveform.band_gains.mid))
    .bind(f64::from(waveform.band_gains.high))
    .bind(waveform.sample_rate)
    .bind(waveform.decoded_duration)
    .execute(connection)
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
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to acquire waveform connection: {error}"))?;
    fetch_track_waveform_for_connection(&mut connection, track_id).await
}

pub async fn fetch_track_waveform_for_connection(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<Option<TrackWaveform>, String> {
    sqlx::query_as::<_, TrackWaveform>(
        "SELECT track_id, uid, preview_samples_blob, full_samples_blob,
         colors_blob, preview_colors_blob, bands_blob, preview_bands_blob, sample_rate,
         decoded_duration
         FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(connection)
    .await
    .map_err(|e| format!("Failed to fetch waveform: {}", e))
}

/// The units a track's stored band envelopes are in, or `None` for a row
/// written before they were kept — see [`StoredWaveform::band_gains`].
///
/// All three columns are written together, so a row with one has all three;
/// a row missing any is treated as a row missing all of them.
pub async fn fetch_band_gains(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<Option<BandGains>, String> {
    let row: Option<(Option<f64>, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT band_gain_low, band_gain_mid, band_gain_high
         FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| format!("Failed to fetch waveform band gains: {error}"))?;

    Ok(match row {
        Some((Some(low), Some(mid), Some(high))) => Some(BandGains {
            low: low as f32,
            mid: mid as f32,
            high: high as f32,
        }),
        _ => None,
    })
}
