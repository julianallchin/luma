use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row};
use ts_rs::TS;

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
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    /// Low-resolution waveform samples (min/max pairs for each bucket)
    pub preview_samples: Vec<f32>,
    /// High-resolution waveform samples (min/max pairs for each bucket)
    /// Note: Not synced to cloud - regenerated locally from audio
    pub full_samples: Option<Vec<f32>>,
    /// 3-band envelopes for full waveform (rekordbox-style)
    /// Note: Not synced to cloud - regenerated locally from audio
    pub bands: Option<BandEnvelopes>,
    /// 3-band envelopes for preview waveform
    pub preview_bands: Option<BandEnvelopes>,
    /// Legacy: Colors for each bucket in full_samples (interleaved R, G, B bytes)
    /// Note: Not synced to cloud - regenerated locally from audio
    pub colors: Option<Vec<u8>>,
    /// Legacy: Colors for each bucket in preview_samples (interleaved R, G, B bytes)
    pub preview_colors: Option<Vec<u8>>,
    #[ts(type = "number")]
    pub sample_rate: u32,
    pub duration_seconds: f64,
}

impl<'r> FromRow<'r, SqliteRow> for TrackWaveform {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let track_id: i64 = row.try_get("track_id")?;
        let remote_id: Option<String> = row.try_get("remote_id")?;
        let uid: Option<String> = row.try_get("uid")?;

        // Deserialize JSON strings to typed fields
        let preview_samples_json: String = row.try_get("preview_samples_json")?;
        let preview_samples: Vec<f32> = serde_json::from_str(&preview_samples_json)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        let full_samples: Option<Vec<f32>> = row
            .try_get::<Option<String>, _>("full_samples_json")?
            .map(|s| serde_json::from_str(&s) as Result<Vec<f32>, _>)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        let colors: Option<Vec<u8>> = row
            .try_get::<Option<String>, _>("colors_json")?
            .map(|s| serde_json::from_str(&s) as Result<Vec<u8>, _>)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        let preview_colors: Option<Vec<u8>> = row
            .try_get::<Option<String>, _>("preview_colors_json")?
            .map(|s| serde_json::from_str(&s) as Result<Vec<u8>, _>)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        let bands: Option<BandEnvelopes> = row
            .try_get::<Option<String>, _>("bands_json")?
            .map(|s| serde_json::from_str(&s) as Result<BandEnvelopes, _>)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        let preview_bands: Option<BandEnvelopes> = row
            .try_get::<Option<String>, _>("preview_bands_json")?
            .map(|s| serde_json::from_str(&s) as Result<BandEnvelopes, _>)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        let sample_rate_i64: i64 = row.try_get("sample_rate")?;
        let sample_rate = sample_rate_i64 as u32;

        // duration_seconds must be provided separately by the caller
        // since it's not in the track_waveforms table
        let duration_seconds: f64 = row.try_get("duration_seconds").unwrap_or(0.0);

        Ok(TrackWaveform {
            track_id,
            remote_id,
            uid,
            preview_samples,
            full_samples,
            bands,
            preview_bands,
            colors,
            preview_colors,
            sample_rate,
            duration_seconds,
        })
    }
}
