use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 3-band envelope data for rekordbox-style waveform rendering
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../src/bindings/schema.ts")]
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
#[ts(export, export_to = "../src/bindings/schema.ts")]
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
