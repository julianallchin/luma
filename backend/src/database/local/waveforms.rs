use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row, SqliteConnection, SqlitePool};

use crate::models::waveforms::{BandEnvelopes, BandGains, TrackWaveform};

/// Upsert waveform payload for a track (binary blob storage), owned by the
/// track's own uid.
#[cfg(test)]
pub async fn upsert_track_waveform(
    pool: &SqlitePool,
    waveform: &TrackWaveform,
    gains: BandGains,
) -> Result<(), String> {
    let uid = super::tracks::track_uid(pool, &waveform.track_id).await?;
    let waveform = TrackWaveform {
        uid,
        ..waveform.clone()
    };
    upsert_track_waveform_for_connection(
        &mut *pool
            .acquire()
            .await
            .map_err(|error| format!("Failed to acquire waveform connection: {error}"))?,
        &waveform,
        gains,
    )
    .await
}

/// Transaction-bound waveform publication. `gains` are the units the
/// envelopes are in, stored beside them because they are one measurement.
/// The caller sets `waveform.uid` from the track read through the same
/// authorized connection, so child ownership cannot be resolved from a
/// different identity snapshot. A missing resolution is stored empty.
pub async fn upsert_track_waveform_for_connection(
    connection: &mut SqliteConnection,
    waveform: &TrackWaveform,
    gains: BandGains,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO track_waveforms (track_id, uid, preview_samples_blob, full_samples_blob, bands_blob, preview_bands_blob, band_gain_low, band_gain_mid, band_gain_high, decoded_duration)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            preview_samples_blob = excluded.preview_samples_blob,
            full_samples_blob = excluded.full_samples_blob,
            bands_blob = excluded.bands_blob,
            preview_bands_blob = excluded.preview_bands_blob,
            band_gain_low = excluded.band_gain_low,
            band_gain_mid = excluded.band_gain_mid,
            band_gain_high = excluded.band_gain_high,
            decoded_duration = excluded.decoded_duration,
            updated_at = datetime('now')",
    )
    .bind(&waveform.track_id)
    .bind(&waveform.uid)
    .bind(f32_slice_to_bytes(&waveform.preview_samples))
    .bind(f32_slice_to_bytes(
        waveform.full_samples.as_deref().unwrap_or_default(),
    ))
    .bind(band_envelopes_to_bytes(waveform.bands.as_ref()))
    .bind(band_envelopes_to_bytes(waveform.preview_bands.as_ref()))
    .bind(f64::from(gains.low))
    .bind(f64::from(gains.mid))
    .bind(f64::from(gains.high))
    .bind(waveform.duration_seconds)
    .execute(connection)
    .await
    .map_err(|e| format!("Failed to store waveform: {}", e))?;

    Ok(())
}

/// Fetch cached waveform row for a track
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
         bands_blob, preview_bands_blob, decoded_duration
         FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(connection)
    .await
    .map_err(|e| format!("Failed to fetch waveform: {}", e))
}

/// The units a track's stored band envelopes are in, or `None` when this
/// device has no cached row for it.
pub async fn fetch_band_gains(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<Option<BandGains>, String> {
    let row: Option<(f64, f64, f64)> = sqlx::query_as(
        "SELECT band_gain_low, band_gain_mid, band_gain_high
         FROM track_waveforms WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| format!("Failed to fetch waveform band gains: {error}"))?;

    Ok(row.map(|(low, mid, high)| BandGains {
        low: low as f32,
        mid: mid as f32,
        high: high as f32,
    }))
}

impl<'r> FromRow<'r, SqliteRow> for TrackWaveform {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let track_id: String = row.try_get("track_id")?;
        let uid: Option<String> = row.try_get("uid")?;

        // Deserialize binary blobs to typed fields
        let preview_samples_blob: Vec<u8> = row.try_get("preview_samples_blob")?;
        let preview_samples = bytes_to_f32_vec(&preview_samples_blob);

        let full_samples: Option<Vec<f32>> = row
            .try_get::<Option<Vec<u8>>, _>("full_samples_blob")?
            .map(|b| bytes_to_f32_vec(&b));

        let bands: Option<BandEnvelopes> = row
            .try_get::<Option<Vec<u8>>, _>("bands_blob")?
            .and_then(|b| bytes_to_band_envelopes(&b));

        let preview_bands: Option<BandEnvelopes> = row
            .try_get::<Option<Vec<u8>>, _>("preview_bands_blob")?
            .and_then(|b| bytes_to_band_envelopes(&b));

        // Prefer decoded_duration (true audio length) over metadata duration
        let duration_seconds: f64 = row
            .try_get::<Option<f64>, _>("decoded_duration")
            .ok()
            .flatten()
            .or_else(|| row.try_get("duration_seconds").ok())
            .unwrap_or(0.0);

        Ok(TrackWaveform {
            track_id,
            uid,
            preview_samples,
            full_samples,
            bands,
            preview_bands,
            duration_seconds,
        })
    }
}

/// Serialize a slice of f32 values to raw little-endian bytes
fn f32_slice_to_bytes(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for &val in data {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize raw little-endian bytes back to Vec<f32>
fn bytes_to_f32_vec(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Serialize BandEnvelopes to a single blob: [low..., mid..., high...]
/// Each band has the same length, so we can split evenly on decode.
fn band_envelopes_to_bytes(bands: Option<&BandEnvelopes>) -> Vec<u8> {
    let Some(bands) = bands else {
        return Vec::new();
    };
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
fn bytes_to_band_envelopes(data: &[u8]) -> Option<BandEnvelopes> {
    let floats = bytes_to_f32_vec(data);
    if !floats.len().is_multiple_of(3) {
        return None;
    }
    let band_len = floats.len() / 3;
    Some(BandEnvelopes {
        low: floats[..band_len].to_vec(),
        mid: floats[band_len..band_len * 2].to_vec(),
        high: floats[band_len * 2..].to_vec(),
    })
}
