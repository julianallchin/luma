//! Tauri commands for waveform operations

use tauri::State;

use crate::database::Db;
use crate::models::waveforms::TrackWaveform;
use crate::preprocessing::AnalysisTaskGroup;
use crate::services::waveforms as waveform_service;

#[tauri::command]
pub async fn reprocess_waveform(
    db: State<'_, Db>,
    analysis_tasks: State<'_, AnalysisTaskGroup>,
    track_id: String,
) -> Result<TrackWaveform, String> {
    waveform_service::reprocess_track_waveform(&db.0, &analysis_tasks, &track_id).await
}
