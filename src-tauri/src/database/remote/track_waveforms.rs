// Remote CRUD operations for track_waveforms table

use super::common::{SupabaseClient, SyncError};
use crate::models::waveforms::TrackWaveform;
use serde::Serialize;

/// Payload for upserting track waveforms to Supabase
/// Only preview data is synced; full waveform is regenerated locally
#[derive(Serialize)]
struct TrackWaveformPayload<'a> {
    uid: &'a str,
    track_id: &'a str,
    preview_samples: &'a [f32],
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_colors: Option<&'a [u8]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_bands_low: Option<&'a [f32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_bands_mid: Option<&'a [f32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_bands_high: Option<&'a [f32]>,
    sample_rate: i32,
    duration_seconds: f64,
}

/// Upsert track waveform in Supabase (idempotent).
///
/// The track_id is taken directly from the waveform record (already a UUID).
/// Only preview waveform data is synced. Full waveform and bands are regenerated locally.
pub async fn upsert_track_waveform(
    client: &SupabaseClient,
    waveform: &TrackWaveform,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = waveform
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = TrackWaveformPayload {
        uid,
        track_id: &waveform.track_id,
        preview_samples: &waveform.preview_samples,
        preview_colors: waveform.preview_colors.as_deref(),
        preview_bands_low: waveform.preview_bands.as_ref().map(|b| b.low.as_slice()),
        preview_bands_mid: waveform.preview_bands.as_ref().map(|b| b.mid.as_slice()),
        preview_bands_high: waveform.preview_bands.as_ref().map(|b| b.high.as_slice()),
        sample_rate: waveform.sample_rate as i32,
        duration_seconds: waveform.duration_seconds,
    };

    client
        .upsert_no_return("track_waveforms", &payload, "track_id", access_token)
        .await
}
