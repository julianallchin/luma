use crate::dispatch::{AppServices, CommandError};
use crate::models::waveforms::TrackWaveform;
use crate::services::waveforms as waveform_service;

pub async fn get_track_waveform(
    services: &AppServices,
    track_id: String,
) -> Result<TrackWaveform, CommandError> {
    Ok(
        waveform_service::get_track_waveform(&services.db.0, &services.analysis_tasks, &track_id)
            .await?,
    )
}

/// `get_track_waveform` minus the cache lookup: both funnel into
/// `ensure_track_waveform` under the same analysis lease.
pub async fn reprocess_waveform(
    services: &AppServices,
    track_id: String,
) -> Result<TrackWaveform, CommandError> {
    Ok(waveform_service::reprocess_track_waveform(
        &services.db.0,
        &services.analysis_tasks,
        &track_id,
    )
    .await?)
}
