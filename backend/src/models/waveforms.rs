use serde::{Deserialize, Serialize};

/// 3-band envelope data for rekordbox-style waveform rendering.
///
/// Values are in *stored envelope units*: a raw band peak divided by that
/// band's [`BandGains`] entry, log-compressed and scaled. Every envelope of one
/// track — the full one, the preview, and any range measured later — is in
/// these units, so a bucket from one is comparable with a bucket from another.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BandEnvelopes {
    /// Low frequency envelope (bass) - values 0.0-1.0
    pub low: Vec<f32>,
    /// Mid frequency envelope (vocals/instruments) - values 0.0-1.0
    pub mid: Vec<f32>,
    /// High frequency envelope (hats/air) - values 0.0-1.0
    pub high: Vec<f32>,
}

/// The three per-band divisors that turn raw band peaks into the units
/// [`BandEnvelopes`] is in.
///
/// Normalisation is per *track*, not per bucketization: these are the 99th
/// percentile of the full-resolution band peaks, measured once, and every
/// envelope of that track is compressed against them. They are stored beside
/// the envelopes because a visible range does not contain the whole-track
/// statistic it has to be measured against — without them a range could only
/// normalise against itself, which would draw a deep zoom in different units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandGains {
    pub low: f32,
    pub mid: f32,
    pub high: f32,
}

/// Waveform data for timeline visualization
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TrackWaveform {
    pub track_id: String,
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
    pub duration_seconds: f64,
}

/// Filtered audio at [`crate::audio::SAMPLE_RATE`] for a native GPU waveform.
/// This large payload crosses the typed dispatch seam once, without JSON
/// serialization.
pub struct WaveformSignal {
    /// Signed low/mid/high band samples, all of equal length.
    pub bands: [Vec<f32>; 3],
    pub gains: BandGains,
    /// Display ceilings shared with stored waveform normalization.
    pub ceilings: [f32; 3],
}
